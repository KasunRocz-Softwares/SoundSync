use crate::PcmFormat;

/// Status values avoid allocating an error on the audio path.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum QueueStatus {
    Ok,
    Full,
    Empty,
    InvalidFormat,
    BlockTooLarge,
    OutputTooSmall,
    Discontinuity,
}

#[derive(Clone, Copy, Default)]
struct Slot {
    first_frame: u64,
    frames: usize,
}

/// Fixed-capacity, single-owner queue. The producer copies into preallocated storage.
/// Use a separate queue per output for independent fanout. The caller owns synchronization
/// across threads; no method here locks, allocates, logs, or waits.
pub struct BoundedPcmQueue {
    format: PcmFormat,
    max_frames: usize,
    slots: Vec<Slot>,
    storage: Vec<f32>,
    head: usize,
    len: usize,
    next_frame: Option<u64>,
}

impl BoundedPcmQueue {
    /// Allocate once on a control thread, before the queue enters an audio callback.
    pub fn new(
        format: PcmFormat,
        capacity_blocks: usize,
        max_frames: usize,
    ) -> Result<Self, QueueStatus> {
        if format.sample_rate == 0
            || format.channels == 0
            || capacity_blocks == 0
            || max_frames == 0
        {
            return Err(QueueStatus::InvalidFormat);
        }
        let samples_per_slot = max_frames
            .checked_mul(usize::from(format.channels))
            .ok_or(QueueStatus::InvalidFormat)?;
        let total_samples = samples_per_slot
            .checked_mul(capacity_blocks)
            .ok_or(QueueStatus::InvalidFormat)?;
        Ok(Self {
            format,
            max_frames,
            slots: vec![Slot::default(); capacity_blocks],
            storage: vec![0.0; total_samples],
            head: 0,
            len: 0,
            next_frame: None,
        })
    }

    pub fn len(&self) -> usize {
        self.len
    }
    pub fn storage_capacity_samples(&self) -> usize {
        self.storage.capacity()
    }

    pub fn reset(&mut self) {
        self.head = 0;
        self.len = 0;
        self.next_frame = None;
    }

    pub fn push(&mut self, block: &crate::PcmBlock) -> QueueStatus {
        if block.format != self.format {
            return QueueStatus::InvalidFormat;
        }
        self.push_samples(block.first_frame, &block.samples)
    }

    /// Copies a borrowed interleaved buffer into preallocated queue storage.
    pub fn push_samples(&mut self, first_frame: u64, samples: &[f32]) -> QueueStatus {
        let channels = usize::from(self.format.channels);
        if samples.len() % channels != 0 {
            return QueueStatus::InvalidFormat;
        }
        let frames = samples.len() / channels;
        if frames == 0 || frames > self.max_frames {
            return QueueStatus::BlockTooLarge;
        }
        let Some(next_frame) = first_frame.checked_add(frames as u64) else {
            return QueueStatus::InvalidFormat;
        };
        let discontinuity = self
            .next_frame
            .is_some_and(|expected| expected != first_frame);
        if discontinuity {
            self.reset();
        }
        if self.len == self.slots.len() {
            return QueueStatus::Full;
        }
        let tail = (self.head + self.len) % self.slots.len();
        let stride = self.max_frames * usize::from(self.format.channels);
        let start = tail * stride;
        self.storage[start..start + samples.len()].copy_from_slice(samples);
        self.slots[tail] = Slot {
            first_frame,
            frames,
        };
        self.len += 1;
        self.next_frame = Some(next_frame);
        if discontinuity {
            QueueStatus::Discontinuity
        } else {
            QueueStatus::Ok
        }
    }

    pub fn pop_into(&mut self, output: &mut [f32]) -> Result<(u64, usize), QueueStatus> {
        if self.len == 0 {
            return Err(QueueStatus::Empty);
        }
        let slot = self.slots[self.head];
        let samples = slot.frames * usize::from(self.format.channels);
        if output.len() < samples {
            return Err(QueueStatus::OutputTooSmall);
        }
        let start = self.head * self.max_frames * usize::from(self.format.channels);
        output[..samples].copy_from_slice(&self.storage[start..start + samples]);
        self.head = (self.head + 1) % self.slots.len();
        self.len -= 1;
        Ok((slot.first_frame, slot.frames))
    }
}

#[cfg(test)]
mod tests {
    use super::{BoundedPcmQueue, QueueStatus};
    use crate::{PcmBlock, PcmFormat};

    fn format() -> PcmFormat {
        PcmFormat {
            sample_rate: 48_000,
            channels: 2,
        }
    }
    fn block(first_frame: u64, samples: &[f32]) -> PcmBlock {
        PcmBlock {
            first_frame,
            format: format(),
            samples: samples.to_vec(),
        }
    }

    #[test]
    fn fifo_and_exact_capacity_with_full_rejection() {
        let mut queue = BoundedPcmQueue::new(format(), 2, 2).unwrap();
        assert_eq!(queue.push(&block(10, &[1., 2., 3., 4.])), QueueStatus::Ok);
        assert_eq!(queue.push(&block(12, &[5., 6.])), QueueStatus::Ok);
        assert_eq!(queue.len(), 2);
        assert_eq!(queue.push(&block(13, &[7., 8.])), QueueStatus::Full);
        let mut out = [0.; 4];
        assert_eq!(queue.pop_into(&mut out), Ok((10, 2)));
        assert_eq!(out, [1., 2., 3., 4.]);
        assert_eq!(queue.pop_into(&mut out), Ok((12, 1)));
        assert_eq!(&out[..2], &[5., 6.]);
        assert_eq!(queue.pop_into(&mut out), Err(QueueStatus::Empty));
    }

    #[test]
    fn fanout_copies_without_shared_consumer_state() {
        let mut a = BoundedPcmQueue::new(format(), 1, 2).unwrap();
        let mut b = BoundedPcmQueue::new(format(), 1, 2).unwrap();
        let source = block(0, &[1., 2.]);
        assert_eq!(a.push(&source), QueueStatus::Ok);
        assert_eq!(b.push(&source), QueueStatus::Ok);
        let mut out = [0.; 4];
        assert_eq!(a.pop_into(&mut out), Ok((0, 1)));
        assert_eq!(a.pop_into(&mut out), Err(QueueStatus::Empty));
        assert_eq!(b.len(), 1);
        assert_eq!(b.pop_into(&mut out), Ok((0, 1)));
    }

    #[test]
    fn discontinuity_discards_stale_blocks_and_reset_clears_state() {
        let mut queue = BoundedPcmQueue::new(format(), 2, 2).unwrap();
        assert_eq!(queue.push(&block(0, &[1., 2.])), QueueStatus::Ok);
        assert_eq!(
            queue.push(&block(100, &[3., 4.])),
            QueueStatus::Discontinuity
        );
        assert_eq!(queue.len(), 1);
        let mut out = [0.; 4];
        assert_eq!(queue.pop_into(&mut out), Ok((100, 1)));
        assert_eq!(&out[..2], &[3., 4.]);
        queue.reset();
        assert_eq!(queue.pop_into(&mut out), Err(QueueStatus::Empty));
    }

    #[test]
    fn storage_never_exceeds_configured_bounds() {
        let mut queue = BoundedPcmQueue::new(format(), 2, 2).unwrap();
        let initial = queue.storage_capacity_samples();
        assert_eq!(initial, 8);
        assert_eq!(
            queue.push(&block(0, &[1., 2., 3., 4., 5., 6.])),
            QueueStatus::BlockTooLarge
        );
        for i in 0..100 {
            let first = i * 2;
            assert_eq!(
                queue.push(&block(first, &[1., 2., 3., 4.])),
                QueueStatus::Ok
            );
            let mut out = [0.; 4];
            assert_eq!(queue.pop_into(&mut out), Ok((first, 2)));
            assert_eq!(queue.storage_capacity_samples(), initial);
        }
    }

    #[test]
    fn callback_can_push_borrowed_samples_without_constructing_a_block() {
        let mut queue = BoundedPcmQueue::new(format(), 1, 2).unwrap();
        let input = [0.25, -0.25, 0.5, -0.5];
        assert_eq!(queue.push_samples(50, &input), QueueStatus::Ok);
        let mut output = [0.; 4];
        assert_eq!(queue.pop_into(&mut output), Ok((50, 2)));
        assert_eq!(output, input);
        assert_eq!(queue.storage_capacity_samples(), 4);
    }
}

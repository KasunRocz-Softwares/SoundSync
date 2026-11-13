use crate::PcmFormat;
use crossbeam_queue::ArrayQueue;
use std::sync::{
    atomic::{AtomicU64, Ordering},
    Arc,
};

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

struct Slot {
    first_frame: u64,
    frames: usize,
    samples: Vec<f32>,
}

/// Fixed-capacity queue. Direct methods have one owner; `into_spsc` transfers it to
/// distinct lock-free producer and consumer handles for capture/render callbacks.
/// Use a separate queue per output for independent fanout.
pub struct BoundedPcmQueue {
    format: PcmFormat,
    max_frames: usize,
    slots: Vec<Slot>,
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
        let mut slots = Vec::with_capacity(capacity_blocks);
        for _ in 0..capacity_blocks {
            slots.push(Slot {
                first_frame: 0,
                frames: 0,
                samples: vec![0.0; samples_per_slot],
            });
        }
        debug_assert_eq!(
            slots.iter().map(|slot| slot.samples.len()).sum::<usize>(),
            total_samples
        );
        Ok(Self {
            format,
            max_frames,
            slots,
            head: 0,
            len: 0,
            next_frame: None,
        })
    }

    pub fn len(&self) -> usize {
        self.len
    }
    pub fn storage_capacity_samples(&self) -> usize {
        self.slots.iter().map(|slot| slot.samples.capacity()).sum()
    }

    pub fn reset(&mut self) {
        self.head = 0;
        self.len = 0;
        self.next_frame = None;
    }

    /// Control-thread conversion to cross-thread SPSC handles. PCM slots move
    /// without copying; only queue metadata is allocated. Neither handle
    /// allocates, locks, logs, or waits in operation.
    pub fn into_spsc(self) -> (PcmProducer, PcmConsumer) {
        let capacity = self.slots.len();
        let stride = self.max_frames * usize::from(self.format.channels);
        let inner = Arc::new(SpscInner {
            format: self.format,
            max_frames: self.max_frames,
            capacity_samples: capacity * stride,
            free: ArrayQueue::new(capacity),
            ready: ArrayQueue::new(capacity),
            generation: AtomicU64::new(0),
        });
        let mut slots = self.slots;
        slots.rotate_left(self.head);
        for (index, old) in slots.into_iter().enumerate() {
            let slot = TransportSlot {
                first_frame: old.first_frame,
                frames: old.frames,
                generation: 0,
                samples: old.samples,
            };
            if index < self.len {
                inner.ready.push(slot).unwrap_or_else(|_| unreachable!());
            } else {
                inner.free.push(slot).unwrap_or_else(|_| unreachable!());
            }
        }
        let producer = PcmProducer {
            inner: Arc::clone(&inner),
            next_frame: self.next_frame,
            discontinuity_pending: false,
        };
        let consumer = PcmConsumer {
            inner,
            pending: None,
        };
        (producer, consumer)
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
        let slot = &mut self.slots[tail];
        slot.samples[..samples.len()].copy_from_slice(samples);
        slot.first_frame = first_frame;
        slot.frames = frames;
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
        let slot = &self.slots[self.head];
        let samples = slot.frames * usize::from(self.format.channels);
        if output.len() < samples {
            return Err(QueueStatus::OutputTooSmall);
        }
        output[..samples].copy_from_slice(&slot.samples[..samples]);
        self.head = (self.head + 1) % self.slots.len();
        self.len -= 1;
        Ok((slot.first_frame, slot.frames))
    }
}

struct TransportSlot {
    first_frame: u64,
    frames: usize,
    generation: u64,
    samples: Vec<f32>,
}

struct SpscInner {
    format: PcmFormat,
    max_frames: usize,
    capacity_samples: usize,
    free: ArrayQueue<TransportSlot>,
    ready: ArrayQueue<TransportSlot>,
    generation: AtomicU64,
}

/// Unique capture-side handle. Move it to one producer thread; do not clone it.
pub struct PcmProducer {
    inner: Arc<SpscInner>,
    next_frame: Option<u64>,
    discontinuity_pending: bool,
}

impl PcmProducer {
    pub fn storage_capacity_samples(&self) -> usize {
        self.inner.capacity_samples
    }

    pub fn push(&mut self, block: &crate::PcmBlock) -> QueueStatus {
        if block.format != self.inner.format {
            return QueueStatus::InvalidFormat;
        }
        self.push_samples(block.first_frame, &block.samples)
    }

    /// Nonblocking copy from caller-owned PCM. A full queue rejects the block.
    pub fn push_samples(&mut self, first_frame: u64, samples: &[f32]) -> QueueStatus {
        let channels = usize::from(self.inner.format.channels);
        if samples.len() % channels != 0 {
            return QueueStatus::InvalidFormat;
        }
        let frames = samples.len() / channels;
        if frames == 0 || frames > self.inner.max_frames {
            return QueueStatus::BlockTooLarge;
        }
        let Some(next_frame) = first_frame.checked_add(frames as u64) else {
            return QueueStatus::InvalidFormat;
        };
        if self
            .next_frame
            .is_some_and(|expected| expected != first_frame)
        {
            self.reset();
        }
        let Some(mut slot) = self.inner.free.pop() else {
            return QueueStatus::Full;
        };
        slot.first_frame = first_frame;
        slot.frames = frames;
        slot.generation = self.inner.generation.load(Ordering::Acquire);
        slot.samples[..samples.len()].copy_from_slice(samples);
        self.inner
            .ready
            .push(slot)
            .unwrap_or_else(|_| unreachable!());
        self.next_frame = Some(next_frame);
        if self.discontinuity_pending {
            self.discontinuity_pending = false;
            QueueStatus::Discontinuity
        } else {
            QueueStatus::Ok
        }
    }

    /// Flushes queued old-generation blocks without touching a slot held by the consumer.
    /// An in-flight consumer checks generation before it returns audio.
    pub fn reset(&mut self) {
        self.inner.generation.fetch_add(1, Ordering::AcqRel);
        self.next_frame = None;
        self.discontinuity_pending = true;
        while let Some(slot) = self.inner.ready.pop() {
            self.inner
                .free
                .push(slot)
                .unwrap_or_else(|_| unreachable!());
        }
    }
}

/// Unique render-side handle. Move it to one consumer thread; do not clone it.
pub struct PcmConsumer {
    inner: Arc<SpscInner>,
    pending: Option<TransportSlot>,
}

impl PcmConsumer {
    pub fn storage_capacity_samples(&self) -> usize {
        self.inner.capacity_samples
    }

    /// A short output buffer leaves the block pending for the next callback.
    pub fn pop_into(&mut self, output: &mut [f32]) -> Result<(u64, usize), QueueStatus> {
        loop {
            let Some(slot) = self.pending.take().or_else(|| self.inner.ready.pop()) else {
                return Err(QueueStatus::Empty);
            };
            let generation = self.inner.generation.load(Ordering::Acquire);
            if slot.generation != generation {
                self.inner
                    .free
                    .push(slot)
                    .unwrap_or_else(|_| unreachable!());
                continue;
            }
            let count = slot.frames * usize::from(self.inner.format.channels);
            if output.len() < count {
                self.pending = Some(slot);
                return Err(QueueStatus::OutputTooSmall);
            }
            output[..count].copy_from_slice(&slot.samples[..count]);
            if slot.generation != self.inner.generation.load(Ordering::Acquire) {
                self.inner
                    .free
                    .push(slot)
                    .unwrap_or_else(|_| unreachable!());
                continue;
            }
            let result = (slot.first_frame, slot.frames);
            self.inner
                .free
                .push(slot)
                .unwrap_or_else(|_| unreachable!());
            return Ok(result);
        }
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

    #[test]
    fn split_queue_hands_off_pcm_across_threads_in_order() {
        let queue = BoundedPcmQueue::new(format(), 4, 1).unwrap();
        let (mut producer, mut consumer) = queue.into_spsc();
        let writer = std::thread::spawn(move || {
            for frame in 0..10_000_u64 {
                let samples = [frame as f32, -(frame as f32)];
                loop {
                    match producer.push_samples(frame, &samples) {
                        QueueStatus::Ok => break,
                        QueueStatus::Full => std::thread::yield_now(),
                        status => panic!("unexpected producer status: {status:?}"),
                    }
                }
            }
            producer.storage_capacity_samples()
        });
        let reader = std::thread::spawn(move || {
            let mut output = [0.0; 2];
            for frame in 0..10_000_u64 {
                loop {
                    match consumer.pop_into(&mut output) {
                        Ok((first, 1)) => {
                            assert_eq!(first, frame);
                            assert_eq!(output, [frame as f32, -(frame as f32)]);
                            break;
                        }
                        Err(QueueStatus::Empty) => std::thread::yield_now(),
                        result => panic!("unexpected consumer result: {result:?}"),
                    }
                }
            }
            consumer.storage_capacity_samples()
        });
        assert_eq!(writer.join().unwrap(), 8);
        assert_eq!(reader.join().unwrap(), 8);
    }

    #[test]
    fn split_queue_discards_stale_audio_on_discontinuity() {
        let (mut producer, mut consumer) =
            BoundedPcmQueue::new(format(), 2, 1).unwrap().into_spsc();
        assert_eq!(producer.push_samples(0, &[1., 2.]), QueueStatus::Ok);
        assert_eq!(producer.push_samples(1, &[3., 4.]), QueueStatus::Ok);
        assert_eq!(
            producer.push_samples(100, &[5., 6.]),
            QueueStatus::Discontinuity
        );
        let mut output = [0.; 2];
        assert_eq!(consumer.pop_into(&mut output), Ok((100, 1)));
        assert_eq!(output, [5., 6.]);
        assert_eq!(consumer.pop_into(&mut output), Err(QueueStatus::Empty));
    }
}

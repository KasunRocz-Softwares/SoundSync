//! Sums several captures (one per routed app) into one stream. Every capture's
//! blocks are stamped on the same QPC-derived frame timeline, so blocks with the
//! same frame number are simultaneous. Frames are released a little behind the
//! newest frame seen, giving slower captures time to contribute; a capture that
//! is silent (sends nothing) simply contributes nothing. Runs on the engine
//! control thread with storage allocated up front.

pub struct TimelineMixer {
    channels: usize,
    capacity: usize,
    lag: u64,
    ring: Vec<f32>,
    emit_from: Option<u64>,
    latest: u64,
}

impl TimelineMixer {
    pub fn new(channels: usize, capacity_frames: usize, lag_frames: u64) -> Self {
        Self {
            channels,
            capacity: capacity_frames,
            lag: lag_frames,
            ring: vec![0.0; capacity_frames * channels],
            emit_from: None,
            latest: 0,
        }
    }

    pub fn reset(&mut self) {
        self.ring.fill(0.0);
        self.emit_from = None;
        self.latest = 0;
    }

    /// Adds an interleaved block stamped with its first frame.
    pub fn add(&mut self, first: u64, samples: &[f32]) {
        let ch = self.channels;
        let frames = samples.len() / ch;
        if frames == 0 {
            return;
        }
        let start = *self.emit_from.get_or_insert(first);
        let end = first + frames as u64;
        if end > start + self.capacity as u64 {
            // Too far ahead of what has been released (e.g. a capture restarted
            // after a long pause): drop the stale window and restart from here.
            self.ring.fill(0.0);
            self.emit_from = Some(first);
            self.latest = 0;
        }
        let start = self.emit_from.unwrap();
        for i in 0..frames {
            let frame = first + i as u64;
            if frame < start {
                continue; // arrived after its frames were released
            }
            let slot = (frame % self.capacity as u64) as usize * ch;
            for c in 0..ch {
                self.ring[slot + c] += samples[i * ch + c];
            }
        }
        self.latest = self.latest.max(end);
    }

    /// Releases `out.len() / channels` frames when they are at least `lag`
    /// behind the newest data. Returns the block's first frame.
    pub fn pop(&mut self, out: &mut [f32]) -> Option<u64> {
        let ch = self.channels;
        let frames = (out.len() / ch) as u64;
        let start = self.emit_from?;
        if start + frames + self.lag > self.latest {
            return None;
        }
        for i in 0..frames as usize {
            let slot = ((start + i as u64) % self.capacity as u64) as usize * ch;
            out[i * ch..(i + 1) * ch].copy_from_slice(&self.ring[slot..slot + ch]);
            self.ring[slot..slot + ch].fill(0.0);
        }
        self.emit_from = Some(start + frames);
        Some(start)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sums_simultaneous_blocks_from_two_captures() {
        let mut mixer = TimelineMixer::new(2, 4_800, 0);
        mixer.add(1_000, &[0.25; 8]);
        mixer.add(1_000, &[0.5; 8]);
        let mut out = [0.0; 8];
        assert_eq!(mixer.pop(&mut out), Some(1_000));
        assert_eq!(out, [0.75; 8]);
        assert_eq!(mixer.pop(&mut out), None);
    }

    #[test]
    fn waits_for_lag_and_aligns_offset_blocks() {
        let mut mixer = TimelineMixer::new(1, 4_800, 2);
        mixer.add(0, &[1.0; 4]);
        let mut out = [0.0; 4];
        assert_eq!(
            mixer.pop(&mut out),
            None,
            "needs 2 frames of lag beyond the block"
        );
        mixer.add(2, &[1.0; 4]); // second app, 2 frames later; newest frame is now 6
        assert_eq!(mixer.pop(&mut out), Some(0));
        assert_eq!(out, [1.0, 1.0, 2.0, 2.0]);
    }

    #[test]
    fn late_frames_are_dropped_and_far_jumps_restart() {
        let mut mixer = TimelineMixer::new(1, 16, 0);
        mixer.add(0, &[1.0; 8]);
        let mut out = [0.0; 8];
        assert_eq!(mixer.pop(&mut out), Some(0));
        mixer.add(4, &[5.0; 8]); // frames 4..8 already released
        assert_eq!(mixer.pop(&mut out), None);
        mixer.add(12, &[1.0; 4]);
        assert_eq!(mixer.pop(&mut out), Some(8));
        assert_eq!(out, [5.0, 5.0, 5.0, 5.0, 1.0, 1.0, 1.0, 1.0]);
        mixer.add(1_000, &[2.0; 8]);
        assert_eq!(mixer.pop(&mut out), Some(1_000));
        assert_eq!(out, [2.0; 8]);
    }
}

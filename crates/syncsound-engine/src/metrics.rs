use serde::Serialize;

/// Callback-local counters. Publish a copied snapshot from a control thread.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize)]
pub struct AudioMetrics {
    pub underruns: u64,
    pub overruns: u64,
    pub discontinuities: u64,
    pub buffered_frames: u64,
}

impl AudioMetrics {
    pub fn record_underrun(&mut self) {
        self.underruns = self.underruns.saturating_add(1);
    }
    pub fn record_overrun(&mut self) {
        self.overruns = self.overruns.saturating_add(1);
    }
    pub fn record_discontinuity(&mut self) {
        self.discontinuities = self.discontinuities.saturating_add(1);
    }
    pub fn set_buffered_frames(&mut self, frames: u64) {
        self.buffered_frames = frames;
    }
}

#[cfg(test)]
mod tests {
    use super::AudioMetrics;

    #[test]
    fn counts_queue_events_without_growing_storage() {
        let mut metrics = AudioMetrics::default();
        metrics.record_underrun();
        metrics.record_overrun();
        metrics.record_discontinuity();
        metrics.set_buffered_frames(480);
        assert_eq!(metrics.underruns, 1);
        assert_eq!(metrics.overruns, 1);
        assert_eq!(metrics.discontinuities, 1);
        assert_eq!(metrics.buffered_frames, 480);
    }
}

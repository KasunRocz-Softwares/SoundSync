use serde::Serialize;

/// Maps a monotonic QPC tick domain to absolute PCM frame positions.
#[derive(Clone, Copy, Debug, Serialize)]
pub struct MasterTimeline {
    sample_rate: u32,
    qpc_frequency: u64,
    anchor_qpc: u64,
    anchor_frame: u64,
}

impl MasterTimeline {
    pub fn new(
        sample_rate: u32,
        qpc_frequency: u64,
        anchor_qpc: u64,
        anchor_frame: u64,
    ) -> Option<Self> {
        if sample_rate == 0 || qpc_frequency == 0 {
            return None;
        }
        Some(Self {
            sample_rate,
            qpc_frequency,
            anchor_qpc,
            anchor_frame,
        })
    }

    pub fn reset(&mut self, anchor_qpc: u64, anchor_frame: u64) {
        self.anchor_qpc = anchor_qpc;
        self.anchor_frame = anchor_frame;
    }

    pub fn frame_at(&self, qpc: u64) -> u64 {
        let ticks = i128::from(qpc) - i128::from(self.anchor_qpc);
        let frames = i128::from(self.anchor_frame)
            + (ticks * i128::from(self.sample_rate)).div_euclid(i128::from(self.qpc_frequency));
        frames.clamp(0, i128::from(u64::MAX)) as u64
    }

    pub fn qpc_at(&self, frame: u64) -> u64 {
        let frames = i128::from(frame) - i128::from(self.anchor_frame);
        let ticks = i128::from(self.anchor_qpc)
            + (frames * i128::from(self.qpc_frequency)).div_euclid(i128::from(self.sample_rate));
        ticks.clamp(0, i128::from(u64::MAX)) as u64
    }
}

/// Observes two independent monotonically increasing frame clocks. Each estimate
/// spans at least 48,000 master frames so callback-sized integer rounding does
/// not alias a small drift into 0 or thousands of ppm. Outliers above 1000 ppm
/// are ignored. Correction is clamped and slews by at most 50 ppm per estimate.
#[derive(Clone, Copy, Debug)]
pub struct ClockEstimator {
    max_correction_ppm: f64,
    window_start: Option<(u64, u64)>,
    last_seen: Option<(u64, u64)>,
    ppm: f64,
    applied_ppm: f64,
}

impl ClockEstimator {
    pub fn new(max_correction_ppm: f64) -> Option<Self> {
        if !max_correction_ppm.is_finite()
            || max_correction_ppm <= 0.0
            || max_correction_ppm > 1_000.0
        {
            return None;
        }
        Some(Self {
            max_correction_ppm,
            window_start: None,
            last_seen: None,
            ppm: 0.0,
            applied_ppm: 0.0,
        })
    }

    pub fn reset(&mut self) {
        self.window_start = None;
        self.last_seen = None;
        self.ppm = 0.0;
        self.applied_ppm = 0.0;
    }
    pub fn ppm(&self) -> f64 {
        self.ppm
    }

    pub fn observe(&mut self, master_frame: u64, device_frame: u64) -> bool {
        let Some((start_master, start_device)) = self.window_start else {
            self.window_start = Some((master_frame, device_frame));
            self.last_seen = Some((master_frame, device_frame));
            return true;
        };
        if let Some((last_master, last_device)) = self.last_seen {
            if master_frame <= last_master || device_frame < last_device {
                return false;
            }
        }
        let (Some(master_delta), Some(device_delta)) = (
            master_frame.checked_sub(start_master),
            device_frame.checked_sub(start_device),
        ) else {
            return false;
        };
        if device_delta == 0 {
            return false;
        }
        if master_delta < 48_000 {
            self.last_seen = Some((master_frame, device_frame));
            return true;
        }
        let ppm = (device_delta as f64 - master_delta as f64) / master_delta as f64 * 1_000_000.0;
        if !ppm.is_finite() || ppm.abs() > 1_000.0 {
            return false;
        }
        self.ppm = ppm;
        let target = ppm.clamp(-self.max_correction_ppm, self.max_correction_ppm);
        self.applied_ppm += (target - self.applied_ppm).clamp(-50.0, 50.0);
        self.window_start = Some((master_frame, device_frame));
        self.last_seen = Some((master_frame, device_frame));
        true
    }

    /// Source frames consumed per device output frame.
    pub fn correction_ratio(&self) -> f64 {
        1.0 / (1.0 + self.applied_ppm * 0.000_001)
    }
}

#[cfg(test)]
mod tests {
    use super::{ClockEstimator, MasterTimeline};

    #[test]
    fn qpc_maps_to_integer_frames_and_back() {
        let timeline = MasterTimeline::new(48_000, 10_000_000, 1_000, 240).unwrap();
        assert_eq!(timeline.frame_at(1_000), 240);
        assert_eq!(timeline.frame_at(10_001_000), 48_240);
        assert_eq!(timeline.qpc_at(48_240), 10_001_000);
        assert_eq!(timeline.frame_at(0), 235);
    }

    #[test]
    fn reset_reanchors_after_discontinuity() {
        let mut timeline = MasterTimeline::new(48_000, 10_000_000, 0, 0).unwrap();
        timeline.reset(20_000_000, 1_000);
        assert_eq!(timeline.frame_at(20_000_000), 1_000);
        assert_eq!(timeline.frame_at(30_000_000), 49_000);
    }

    #[test]
    fn estimates_positive_and_negative_hundred_ppm() {
        let mut fast = ClockEstimator::new(500.0).unwrap();
        assert!(fast.observe(0, 0));
        assert!(fast.observe(1_000_000, 1_000_100));
        assert!((fast.ppm() - 100.0).abs() < 0.001);
        assert!((fast.correction_ratio() - 1.0 / 1.00005).abs() < 1e-9);
        let mut slow = ClockEstimator::new(500.0).unwrap();
        assert!(slow.observe(0, 0));
        assert!(slow.observe(1_000_000, 999_900));
        assert!((slow.ppm() + 100.0).abs() < 0.001);
    }

    #[test]
    fn rejects_outliers_and_clamps_correction() {
        let mut estimator = ClockEstimator::new(200.0).unwrap();
        assert!(estimator.observe(0, 0));
        assert!(!estimator.observe(1_000_000, 1_005_000));
        assert_eq!(estimator.ppm(), 0.0);
        assert!(estimator.observe(1_000_000, 1_000_400));
        assert!((estimator.ppm() - 400.0).abs() < 0.001);
        assert!((estimator.correction_ratio() - 1.0 / 1.00005).abs() < 1e-9);
        for window in 2..=4_u64 {
            assert!(estimator.observe(window * 1_000_000, window * 1_000_400));
        }
        assert!((estimator.correction_ratio() - 1.0 / 1.0002).abs() < 1e-9);
        estimator.reset();
        assert_eq!(estimator.ppm(), 0.0);
        assert_eq!(estimator.correction_ratio(), 1.0);
    }

    #[test]
    fn correction_slews_when_drift_changes_direction() {
        let mut estimator = ClockEstimator::new(200.0).unwrap();
        assert!(estimator.observe(0, 0));
        assert!(estimator.observe(1_000_000, 1_000_400));
        assert!((estimator.correction_ratio() - 1.0 / 1.00005).abs() < 1e-9);
        assert!(estimator.observe(2_000_000, 2_000_000));
        assert!((estimator.ppm() + 400.0).abs() < 0.001);
        assert_eq!(estimator.correction_ratio(), 1.0);
    }

    #[test]
    fn callback_cadence_estimates_hundred_ppm_from_integer_frames() {
        for device_rate in [10_001_u64, 9_999_u64] {
            let mut estimator = ClockEstimator::new(200.0).unwrap();
            assert!(estimator.observe(0, 0));
            for callback in 1..=1_000_u64 {
                let master = callback * 480;
                let device = master * device_rate / 10_000;
                assert!(estimator.observe(master, device), "callback {callback}");
            }
            let target = if device_rate > 10_000 { 100.0 } else { -100.0 };
            assert!(
                (estimator.ppm() - target).abs() < 25.0,
                "rate {device_rate}: {}",
                estimator.ppm()
            );
        }
    }

    #[test]
    fn first_estimate_slews_from_unity_before_reaching_clamp() {
        let mut estimator = ClockEstimator::new(200.0).unwrap();
        assert!(estimator.observe(0, 0));
        assert!(estimator.observe(1_000_000, 1_000_400));
        assert!((estimator.correction_ratio() - 1.0 / 1.00005).abs() < 1e-9);
        assert!(estimator.observe(2_000_000, 2_000_800));
        assert!((estimator.correction_ratio() - 1.0 / 1.0001).abs() < 1e-9);
    }
}

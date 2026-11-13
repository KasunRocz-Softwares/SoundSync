//! Per-output equalizer and bass management: low-cut (high-pass) for small
//! speakers, bass / mid / treble bands, and automatic headroom so boosts do not
//! clip. Biquads follow the RBJ Audio EQ Cookbook.
//!
//! Coefficients are computed on the control thread and handed to the render
//! thread through a seqlock of atomics, so the audio path never locks or
//! allocates; a torn read is detected and simply retried on the next callback.
use crate::commands::CommandError;
use serde::{Deserialize, Serialize};
use std::f64::consts::PI;
use std::sync::atomic::{AtomicU32, AtomicU64, Ordering};

pub const MAX_BAND_DB: f32 = 12.0;
pub const BASS_HZ: f64 = 100.0;
pub const MID_HZ: f64 = 1_000.0;
pub const TREBLE_HZ: f64 = 8_000.0;
/// Allowed low-cut corner frequencies (0 = off).
pub const LOW_CUT_OPTIONS: [u32; 7] = [0, 40, 60, 80, 100, 120, 150];
const STAGES: usize = 4;

#[derive(Clone, Copy, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct EqSettings {
    pub enabled: bool,
    /// High-pass corner in Hz, 0 = off. Removes bass a small speaker cannot play.
    #[serde(default)]
    pub low_cut_hz: u32,
    #[serde(default)]
    pub bass_db: f32,
    #[serde(default)]
    pub mid_db: f32,
    #[serde(default)]
    pub treble_db: f32,
}

impl EqSettings {
    pub fn validate(&self) -> Result<(), CommandError> {
        if !LOW_CUT_OPTIONS.contains(&self.low_cut_hz) {
            return Err(CommandError::new(
                "invalid_eq",
                "Low cut must be off, 40, 60, 80, 100, 120 or 150 Hz",
            ));
        }
        for band in [self.bass_db, self.mid_db, self.treble_db] {
            if !band.is_finite() || band.abs() > MAX_BAND_DB {
                return Err(CommandError::new(
                    "invalid_eq",
                    format!("EQ bands must be between -{MAX_BAND_DB} and +{MAX_BAND_DB} dB"),
                ));
            }
        }
        Ok(())
    }

    /// Pre-gain that keeps the largest boost from clipping.
    pub fn headroom(&self) -> f32 {
        if !self.enabled {
            return 1.0;
        }
        let boost = self.bass_db.max(self.mid_db).max(self.treble_db).max(0.0);
        10f32.powf(-boost / 20.0)
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Biquad {
    pub b0: f32,
    pub b1: f32,
    pub b2: f32,
    pub a1: f32,
    pub a2: f32,
}

impl Biquad {
    pub const IDENTITY: Biquad = Biquad {
        b0: 1.0,
        b1: 0.0,
        b2: 0.0,
        a1: 0.0,
        a2: 0.0,
    };

    fn normalize(b0: f64, b1: f64, b2: f64, a0: f64, a1: f64, a2: f64) -> Biquad {
        Biquad {
            b0: (b0 / a0) as f32,
            b1: (b1 / a0) as f32,
            b2: (b2 / a0) as f32,
            a1: (a1 / a0) as f32,
            a2: (a2 / a0) as f32,
        }
    }

    pub fn high_pass(freq: f64, rate: f64) -> Biquad {
        let w = 2.0 * PI * freq / rate;
        let alpha = w.sin() / (2.0 * std::f64::consts::FRAC_1_SQRT_2);
        let c = w.cos();
        Self::normalize(
            (1.0 + c) / 2.0,
            -(1.0 + c),
            (1.0 + c) / 2.0,
            1.0 + alpha,
            -2.0 * c,
            1.0 - alpha,
        )
    }

    pub fn low_shelf(freq: f64, rate: f64, db: f64) -> Biquad {
        if db.abs() < 0.01 {
            return Self::IDENTITY;
        }
        let a = 10f64.powf(db / 40.0);
        let w = 2.0 * PI * freq / rate;
        let (s, c) = (w.sin(), w.cos());
        let alpha = s / 2.0 * std::f64::consts::SQRT_2; // shelf slope 1
        let sq = 2.0 * a.sqrt() * alpha;
        Self::normalize(
            a * ((a + 1.0) - (a - 1.0) * c + sq),
            2.0 * a * ((a - 1.0) - (a + 1.0) * c),
            a * ((a + 1.0) - (a - 1.0) * c - sq),
            (a + 1.0) + (a - 1.0) * c + sq,
            -2.0 * ((a - 1.0) + (a + 1.0) * c),
            (a + 1.0) + (a - 1.0) * c - sq,
        )
    }

    pub fn high_shelf(freq: f64, rate: f64, db: f64) -> Biquad {
        if db.abs() < 0.01 {
            return Self::IDENTITY;
        }
        let a = 10f64.powf(db / 40.0);
        let w = 2.0 * PI * freq / rate;
        let (s, c) = (w.sin(), w.cos());
        let alpha = s / 2.0 * std::f64::consts::SQRT_2;
        let sq = 2.0 * a.sqrt() * alpha;
        Self::normalize(
            a * ((a + 1.0) + (a - 1.0) * c + sq),
            -2.0 * a * ((a - 1.0) + (a + 1.0) * c),
            a * ((a + 1.0) + (a - 1.0) * c - sq),
            (a + 1.0) - (a - 1.0) * c + sq,
            2.0 * ((a - 1.0) - (a + 1.0) * c),
            (a + 1.0) - (a - 1.0) * c - sq,
        )
    }

    pub fn peaking(freq: f64, rate: f64, db: f64, q: f64) -> Biquad {
        if db.abs() < 0.01 {
            return Self::IDENTITY;
        }
        let a = 10f64.powf(db / 40.0);
        let w = 2.0 * PI * freq / rate;
        let alpha = w.sin() / (2.0 * q);
        let c = w.cos();
        Self::normalize(
            1.0 + alpha * a,
            -2.0 * c,
            1.0 - alpha * a,
            1.0 + alpha / a,
            -2.0 * c,
            1.0 - alpha / a,
        )
    }

    /// Magnitude response at `freq` (for tests and the UI curve).
    pub fn magnitude(&self, freq: f64, rate: f64) -> f64 {
        let w = 2.0 * PI * freq / rate;
        let (c1, s1, c2, s2) = (w.cos(), w.sin(), (2.0 * w).cos(), (2.0 * w).sin());
        let (b0, b1, b2) = (f64::from(self.b0), f64::from(self.b1), f64::from(self.b2));
        let (a1, a2) = (f64::from(self.a1), f64::from(self.a2));
        let nr = b0 + b1 * c1 + b2 * c2;
        let ni = -(b1 * s1 + b2 * s2);
        let dr = 1.0 + a1 * c1 + a2 * c2;
        let di = -(a1 * s1 + a2 * s2);
        ((nr * nr + ni * ni) / (dr * dr + di * di)).sqrt()
    }
}

pub fn design(settings: &EqSettings, rate: u32) -> [Biquad; STAGES] {
    if !settings.enabled {
        return [Biquad::IDENTITY; STAGES];
    }
    let rate = f64::from(rate);
    [
        if settings.low_cut_hz > 0 {
            Biquad::high_pass(f64::from(settings.low_cut_hz), rate)
        } else {
            Biquad::IDENTITY
        },
        Biquad::low_shelf(BASS_HZ, rate, f64::from(settings.bass_db)),
        Biquad::peaking(MID_HZ, rate, f64::from(settings.mid_db), 0.9),
        Biquad::high_shelf(TREBLE_HZ, rate, f64::from(settings.treble_db)),
    ]
}

/// Seqlock-published coefficients: control thread writes, render thread reads.
#[derive(Debug, Default)]
pub struct EqShared {
    version: AtomicU64,
    coefficients: [AtomicU32; STAGES * 5],
    headroom: AtomicU32,
}

impl EqShared {
    pub fn new() -> Self {
        let shared = Self::default();
        shared.publish(&[Biquad::IDENTITY; STAGES], 1.0);
        shared
    }

    pub fn publish(&self, stages: &[Biquad; STAGES], headroom: f32) {
        self.version.fetch_add(1, Ordering::AcqRel); // odd: writing
        for (i, s) in stages.iter().enumerate() {
            for (j, v) in [s.b0, s.b1, s.b2, s.a1, s.a2].iter().enumerate() {
                self.coefficients[i * 5 + j].store(v.to_bits(), Ordering::Relaxed);
            }
        }
        self.headroom.store(headroom.to_bits(), Ordering::Relaxed);
        self.version.fetch_add(1, Ordering::AcqRel); // even: stable
    }

    pub fn version(&self) -> u64 {
        self.version.load(Ordering::Acquire)
    }

    /// Returns None while a write is in progress or if it changed mid-read.
    pub fn read(&self) -> Option<([Biquad; STAGES], f32, u64)> {
        let before = self.version.load(Ordering::Acquire);
        if before % 2 == 1 {
            return None;
        }
        let mut stages = [Biquad::IDENTITY; STAGES];
        for (i, s) in stages.iter_mut().enumerate() {
            let c = |j: usize| f32::from_bits(self.coefficients[i * 5 + j].load(Ordering::Relaxed));
            *s = Biquad {
                b0: c(0),
                b1: c(1),
                b2: c(2),
                a1: c(3),
                a2: c(4),
            };
        }
        let headroom = f32::from_bits(self.headroom.load(Ordering::Relaxed));
        let after = self.version.load(Ordering::Acquire);
        (before == after).then_some((stages, headroom, after))
    }
}

/// Render-thread filter state for one stereo output.
#[derive(Clone, Copy, Debug)]
pub struct EqProcessor {
    stages: [Biquad; STAGES],
    headroom: f32,
    version: u64,
    active: bool,
    // [stage][channel] = (z1, z2), transposed direct form II
    state: [[(f32, f32); 2]; STAGES],
}

impl Default for EqProcessor {
    fn default() -> Self {
        Self {
            stages: [Biquad::IDENTITY; STAGES],
            headroom: 1.0,
            version: u64::MAX,
            active: false,
            state: [[(0.0, 0.0); 2]; STAGES],
        }
    }
}

impl EqProcessor {
    /// Picks up new coefficients if they changed. Call once per callback.
    pub fn refresh(&mut self, shared: &EqShared) {
        if shared.version() == self.version {
            return;
        }
        if let Some((stages, headroom, version)) = shared.read() {
            self.stages = stages;
            self.headroom = headroom;
            self.version = version;
            self.active = stages.iter().any(|s| *s != Biquad::IDENTITY) || headroom != 1.0;
            if !self.active {
                self.state = [[(0.0, 0.0); 2]; STAGES];
            }
        }
    }

    #[inline]
    pub fn process(&mut self, frame: &mut [f32]) {
        if !self.active {
            return;
        }
        for (ch, sample) in frame.iter_mut().take(2).enumerate() {
            let mut x = *sample * self.headroom;
            for (stage, s) in self.stages.iter().zip(self.state.iter_mut()) {
                let (z1, z2) = s[ch];
                let y = stage.b0 * x + z1;
                s[ch] = (
                    stage.b1 * x - stage.a1 * y + z2,
                    stage.b2 * x - stage.a2 * y,
                );
                x = y;
            }
            *sample = if x.is_finite() { x } else { 0.0 };
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn db(x: f64) -> f64 {
        20.0 * x.log10()
    }

    #[test]
    fn shelves_and_peak_hit_their_target_gains() {
        let rate = 48_000.0;
        assert!((db(Biquad::low_shelf(100.0, rate, 6.0).magnitude(20.0, rate)) - 6.0).abs() < 0.5);
        assert!(db(Biquad::low_shelf(100.0, rate, 6.0).magnitude(5_000.0, rate)).abs() < 0.3);
        assert!(
            (db(Biquad::high_shelf(8_000.0, rate, -6.0).magnitude(20_000.0, rate)) + 6.0).abs()
                < 0.8
        );
        assert!(
            (db(Biquad::peaking(1_000.0, rate, 9.0, 0.9).magnitude(1_000.0, rate)) - 9.0).abs()
                < 0.05
        );
        assert_eq!(Biquad::peaking(1_000.0, rate, 0.0, 0.9), Biquad::IDENTITY);
    }

    #[test]
    fn low_cut_removes_bass_below_the_corner() {
        let rate = 48_000.0;
        let hp = Biquad::high_pass(80.0, rate);
        assert!(db(hp.magnitude(20.0, rate)) < -20.0);
        assert!((db(hp.magnitude(80.0, rate)) + 3.0).abs() < 0.2);
        assert!(db(hp.magnitude(1_000.0, rate)).abs() < 0.1);
    }

    #[test]
    fn processor_applies_published_eq_with_headroom() {
        let shared = EqShared::new();
        let settings = EqSettings {
            enabled: true,
            low_cut_hz: 0,
            bass_db: 6.0,
            mid_db: 0.0,
            treble_db: 0.0,
        };
        shared.publish(&design(&settings, 48_000), settings.headroom());
        let mut eq = EqProcessor::default();
        eq.refresh(&shared);
        // DC through a +6 dB low shelf with −6 dB headroom ≈ unity.
        let mut frame = [0.5, 0.5];
        for _ in 0..48_000 {
            frame = [0.5, 0.5];
            eq.process(&mut frame);
        }
        assert!((frame[0] - 0.5).abs() < 0.01, "{frame:?}");
        // Disabled = bit-exact passthrough.
        shared.publish(&design(&EqSettings::default(), 48_000), 1.0);
        eq.refresh(&shared);
        let mut f = [0.25, -0.75];
        eq.process(&mut f);
        assert_eq!(f, [0.25, -0.75]);
    }

    #[test]
    fn seqlock_rejects_reads_during_a_write() {
        let shared = EqShared::new();
        let v = shared.version();
        assert_eq!(v % 2, 0);
        shared.version.fetch_add(1, Ordering::AcqRel); // simulate writer mid-update
        assert!(shared.read().is_none());
        shared.version.fetch_add(1, Ordering::AcqRel);
        assert!(shared.read().is_some());
    }

    #[test]
    fn validates_settings() {
        let mut s = EqSettings {
            enabled: true,
            ..Default::default()
        };
        assert!(s.validate().is_ok());
        s.low_cut_hz = 70;
        assert!(s.validate().is_err());
        s.low_cut_hz = 80;
        s.bass_db = 13.0;
        assert!(s.validate().is_err());
        s.bass_db = f32::NAN;
        assert!(s.validate().is_err());
        s.bass_db = 12.0;
        assert!(s.validate().is_ok());
        assert!((s.headroom() - 10f32.powf(-0.6)).abs() < 1e-6);
    }
}

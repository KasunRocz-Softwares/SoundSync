//! Output safety limiter and loudness metering (ITU-R BS.1770 / EBU R128).
//! Both run on the render thread: fixed-size state, no allocation, no locks.
use crate::eq::Biquad;
use std::sync::atomic::{AtomicU32, Ordering};

/// The limiter keeps every output at or below −1 dBFS.
pub const LIMIT_CEILING: f32 = 0.891_250_9;
const LIMIT_RELEASE_S: f32 = 0.15;

/// Zero-latency, stereo-linked peak limiter. The gain drops instantly to hold
/// the sample at the ceiling and recovers over ~150 ms, so outputs keep their
/// timing (no look-ahead delay) and calibration stays valid.
#[derive(Clone, Debug)]
pub struct Limiter {
    gain: f32,
    release: f32,
    /// Deepest gain reduction since the last `take_reduction`.
    deepest: f32,
}

impl Limiter {
    pub fn new(rate: u32) -> Self {
        Self {
            gain: 1.0,
            release: 1.0 - (-1.0 / (LIMIT_RELEASE_S * rate as f32)).exp(),
            deepest: 1.0,
        }
    }

    #[inline]
    pub fn process(&mut self, frame: &mut [f32]) {
        let peak = frame.iter().fold(0.0f32, |m, s| m.max(s.abs()));
        self.gain += (1.0 - self.gain) * self.release;
        if peak * self.gain > LIMIT_CEILING {
            self.gain = LIMIT_CEILING / peak;
        }
        for s in frame.iter_mut() {
            *s = if s.is_finite() { *s * self.gain } else { 0.0 };
        }
        self.deepest = self.deepest.min(self.gain);
    }

    /// Gain reduction in dB (≥ 0) since the last call, then resets.
    pub fn take_reduction_db(&mut self) -> f32 {
        let db = -20.0 * self.deepest.max(1e-6).log10();
        self.deepest = self.gain;
        db.max(0.0)
    }
}

pub const LUFS_FLOOR: f32 = -70.0;
/// 100 ms blocks: 4 make the momentary window, 30 the short-term window.
const BLOCKS: usize = 30;
/// Integrated-loudness histogram: 0.1 LU bins from −70 to +5 LUFS.
const BINS: usize = 750;

/// K-weighting filter pair for one sample rate (BS.1770-4, via the analog
/// prototypes so any rate works).
fn k_weighting(rate: u32) -> (Biquad, Biquad) {
    let fs = f64::from(rate);
    let (f0, g, q) = (1_681.974_450_955_533, 3.999_843_853_973_347, 0.707_175_236_955_419_6);
    let k = (std::f64::consts::PI * f0 / fs).tan();
    let vh = 10f64.powf(g / 20.0);
    let vb = vh.powf(0.499_666_774_154_541_6);
    let a0 = 1.0 + k / q + k * k;
    let shelf = Biquad {
        b0: ((vh + vb * k / q + k * k) / a0) as f32,
        b1: (2.0 * (k * k - vh) / a0) as f32,
        b2: ((vh - vb * k / q + k * k) / a0) as f32,
        a1: (2.0 * (k * k - 1.0) / a0) as f32,
        a2: ((1.0 - k / q + k * k) / a0) as f32,
    };
    let (f0, q) = (38.135_470_876_024_44, 0.500_327_037_323_877_3);
    let k = (std::f64::consts::PI * f0 / fs).tan();
    let a0 = 1.0 + k / q + k * k;
    let high_pass = Biquad {
        b0: 1.0,
        b1: -2.0,
        b2: 1.0,
        a1: (2.0 * (k * k - 1.0) / a0) as f32,
        a2: ((1.0 - k / q + k * k) / a0) as f32,
    };
    (shelf, high_pass)
}

fn lufs(power: f64) -> f32 {
    if power <= 1e-12 {
        LUFS_FLOOR
    } else {
        ((-0.691 + 10.0 * power.log10()) as f32).max(LUFS_FLOOR)
    }
}

/// Momentary (400 ms), short-term (3 s) and gated integrated loudness of a
/// stereo stream, plus its sample peak.
#[derive(Clone, Debug)]
pub struct LoudnessMeter {
    shelf: Biquad,
    high_pass: Biquad,
    state: [[(f32, f32); 2]; 2],
    block_len: u32,
    block_pos: u32,
    block_sum: f64,
    blocks: [f64; BLOCKS],
    block_index: usize,
    filled: usize,
    hist_count: Box<[u32; BINS]>,
    hist_power: Box<[f64; BINS]>,
    peak: f32,
}

impl LoudnessMeter {
    pub fn new(rate: u32) -> Self {
        let (shelf, high_pass) = k_weighting(rate);
        Self {
            shelf,
            high_pass,
            state: [[(0.0, 0.0); 2]; 2],
            block_len: (rate / 10).max(1),
            block_pos: 0,
            block_sum: 0.0,
            blocks: [0.0; BLOCKS],
            block_index: 0,
            filled: 0,
            hist_count: Box::new([0; BINS]),
            hist_power: Box::new([0.0; BINS]),
            peak: 0.0,
        }
    }

    /// Forgets everything (the meter restarts when someone starts watching).
    pub fn reset(&mut self) {
        self.state = [[(0.0, 0.0); 2]; 2];
        self.block_pos = 0;
        self.block_sum = 0.0;
        self.blocks = [0.0; BLOCKS];
        self.filled = 0;
        self.hist_count.fill(0);
        self.hist_power.fill(0.0);
        self.peak = 0.0;
    }

    #[inline]
    pub fn push(&mut self, frame: &[f32]) {
        let mut sum = 0.0f64;
        for (c, &x) in frame.iter().take(2).enumerate() {
            self.peak = self.peak.max(x.abs());
            let y = self.shelf.tick(&mut self.state[c][0], x);
            let y = self.high_pass.tick(&mut self.state[c][1], y);
            sum += f64::from(y * y);
        }
        self.block_sum += sum;
        self.block_pos += 1;
        if self.block_pos >= self.block_len {
            self.blocks[self.block_index] = self.block_sum / f64::from(self.block_len);
            self.block_index = (self.block_index + 1) % BLOCKS;
            self.filled = (self.filled + 1).min(BLOCKS);
            self.block_pos = 0;
            self.block_sum = 0.0;
            if self.filled >= 4 {
                // Every 100 ms a new 400 ms gating block (75 % overlap).
                let power = self.window(4);
                let l = lufs(power);
                if l > LUFS_FLOOR {
                    let bin = (((l - LUFS_FLOOR) * 10.0) as usize).min(BINS - 1);
                    self.hist_count[bin] += 1;
                    self.hist_power[bin] += power;
                }
            }
        }
    }

    fn window(&self, n: usize) -> f64 {
        let n = n.min(self.filled);
        if n == 0 {
            return 0.0;
        }
        (1..=n)
            .map(|k| self.blocks[(self.block_index + BLOCKS - k) % BLOCKS])
            .sum::<f64>()
            / n as f64
    }

    pub fn momentary(&self) -> f32 {
        if self.filled < 4 { LUFS_FLOOR } else { lufs(self.window(4)) }
    }
    pub fn short_term(&self) -> f32 {
        if self.filled < BLOCKS { lufs(self.window(self.filled)) } else { lufs(self.window(BLOCKS)) }
    }
    /// Gated integrated loudness: absolute gate −70 LUFS, relative gate −10 LU.
    pub fn integrated(&self) -> f32 {
        let (count, power) = self
            .hist_count
            .iter()
            .zip(self.hist_power.iter())
            .fold((0u64, 0.0f64), |(c, p), (n, s)| (c + u64::from(*n), p + s));
        if count == 0 {
            return LUFS_FLOOR;
        }
        let gate = lufs(power / count as f64) - 10.0;
        let first = (((gate - LUFS_FLOOR) * 10.0).max(0.0) as usize).min(BINS);
        let (count, power) = self.hist_count[first..]
            .iter()
            .zip(self.hist_power[first..].iter())
            .fold((0u64, 0.0f64), |(c, p), (n, s)| (c + u64::from(*n), p + s));
        if count == 0 { LUFS_FLOOR } else { lufs(power / count as f64) }
    }
    /// Highest sample since the last call, in dBFS, then resets.
    pub fn take_peak_db(&mut self) -> f32 {
        let db = 20.0 * self.peak.max(1e-6).log10();
        self.peak = 0.0;
        db.max(-90.0)
    }
}

/// Loudness and limiter readings for one output, published through atomics.
#[derive(Debug)]
pub struct LevelShared {
    momentary: AtomicU32,
    short_term: AtomicU32,
    integrated: AtomicU32,
    peak: AtomicU32,
    /// Deepest limiter gain reduction since the control thread last read it.
    limiter: AtomicU32,
}

impl Default for LevelShared {
    fn default() -> Self {
        Self {
            momentary: AtomicU32::new(LUFS_FLOOR.to_bits()),
            short_term: AtomicU32::new(LUFS_FLOOR.to_bits()),
            integrated: AtomicU32::new(LUFS_FLOOR.to_bits()),
            peak: AtomicU32::new((-90.0f32).to_bits()),
            limiter: AtomicU32::new(0.0f32.to_bits()),
        }
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct LevelReading {
    pub momentary_lufs: f32,
    pub short_term_lufs: f32,
    pub integrated_lufs: f32,
    pub peak_db: f32,
    pub limiter_db: f32,
}

impl LevelShared {
    pub fn publish_loudness(&self, meter: &mut LoudnessMeter) {
        let store = |a: &AtomicU32, v: f32| a.store(v.to_bits(), Ordering::Relaxed);
        store(&self.momentary, meter.momentary());
        store(&self.short_term, meter.short_term());
        store(&self.integrated, meter.integrated());
        // Keep the loudest peak until the control thread reads it.
        let peak = meter.take_peak_db();
        let held = f32::from_bits(self.peak.load(Ordering::Relaxed));
        if peak > held {
            store(&self.peak, peak);
        }
    }
    pub fn publish_limiter(&self, reduction_db: f32) {
        let held = f32::from_bits(self.limiter.load(Ordering::Relaxed));
        if reduction_db > held {
            self.limiter.store(reduction_db.to_bits(), Ordering::Relaxed);
        }
    }
    /// Reads and clears the held peak and limiter values.
    pub fn take(&self) -> LevelReading {
        let load = |a: &AtomicU32| f32::from_bits(a.load(Ordering::Relaxed));
        LevelReading {
            momentary_lufs: load(&self.momentary),
            short_term_lufs: load(&self.short_term),
            integrated_lufs: load(&self.integrated),
            peak_db: f32::from_bits(self.peak.swap((-90.0f32).to_bits(), Ordering::Relaxed)),
            limiter_db: f32::from_bits(self.limiter.swap(0.0f32.to_bits(), Ordering::Relaxed)),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::f32::consts::PI;

    const RATE: u32 = 48_000;

    fn sine(freq: f32, amp: f32, i: usize) -> f32 {
        amp * (2.0 * PI * freq * i as f32 / RATE as f32).sin()
    }

    #[test]
    fn limiter_holds_the_ceiling_and_leaves_quiet_audio_alone() {
        let mut lim = Limiter::new(RATE);
        for i in 0..RATE as usize {
            let x = sine(100.0, 3.0, i);
            let mut f = [x, x];
            lim.process(&mut f);
            assert!(f[0].abs() <= LIMIT_CEILING + 1e-6 && f[1].abs() <= LIMIT_CEILING + 1e-6);
        }
        let reduction = lim.take_reduction_db();
        assert!((reduction - 20.0 * (3.0 / LIMIT_CEILING).log10()).abs() < 0.5, "{reduction}");

        let mut quiet = Limiter::new(RATE);
        for i in 0..RATE as usize {
            let x = sine(1_000.0, 0.5, i);
            let mut f = [x, -x];
            quiet.process(&mut f);
            assert_eq!(f, [x, -x]);
        }
        assert_eq!(quiet.take_reduction_db(), 0.0);
    }

    #[test]
    fn limiter_recovers_after_a_peak_and_drops_nan() {
        let mut lim = Limiter::new(RATE);
        let mut f = [4.0, 0.0];
        lim.process(&mut f);
        for _ in 0..RATE {
            let mut f = [0.1, 0.1];
            lim.process(&mut f);
        }
        let mut f = [0.5, 0.5];
        lim.process(&mut f);
        assert!((f[0] - 0.5).abs() < 0.01, "{f:?}");
        let mut f = [f32::NAN, 0.2];
        lim.process(&mut f);
        assert_eq!(f[0], 0.0);
    }

    #[test]
    fn a_minus_20_dbfs_1khz_tone_reads_minus_20_lufs() {
        let mut m = LoudnessMeter::new(RATE);
        for i in 0..(RATE as usize * 4) {
            let x = sine(997.0, 0.1, i);
            m.push(&[x, x]);
        }
        for value in [m.momentary(), m.short_term(), m.integrated()] {
            assert!((value + 20.0).abs() < 0.3, "{value}");
        }
        assert!((m.take_peak_db() + 20.0).abs() < 0.1);
    }

    #[test]
    fn integrated_loudness_gates_out_silence() {
        let mut m = LoudnessMeter::new(RATE);
        for i in 0..(RATE as usize * 3) {
            let x = sine(997.0, 0.1, i);
            m.push(&[x, x]);
        }
        for _ in 0..(RATE as usize * 6) {
            m.push(&[0.0, 0.0]);
        }
        assert!((m.integrated() + 20.0).abs() < 0.5, "{}", m.integrated());
        assert_eq!(m.momentary(), LUFS_FLOOR);
        m.reset();
        assert_eq!(m.integrated(), LUFS_FLOOR);
    }

    #[test]
    fn shared_readings_hold_the_peak_until_read() {
        let shared = LevelShared::default();
        shared.publish_limiter(3.0);
        shared.publish_limiter(1.0);
        let r = shared.take();
        assert_eq!(r.limiter_db, 3.0);
        assert_eq!(shared.take().limiter_db, 0.0);
    }
}

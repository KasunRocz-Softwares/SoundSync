//! Room correction: measure each speaker's frequency response at the listening
//! seat with a logarithmic sine sweep and derive a gentle 10-band correction.
//!
//! A log sweep spends equal time in every octave, so the energy recorded in an
//! octave band (relative to the same band of the dry sweep) is that speaker's
//! response there, including the room. The correction mostly cuts peaks, boosts
//! dips only a little, never boosts below a speaker's natural bass roll-off,
//! and ignores bands where the room noise was too close to the signal.
//!
//! The microphone's own response is included in the measurement. A headset or
//! laptop microphone is not flat, so results are relative and kept mild.
use crate::eq::Biquad;
use serde::{Deserialize, Serialize};
use std::f64::consts::PI;

pub const BANDS: [f64; 10] = [
    31.5, 63.0, 125.0, 250.0, 500.0, 1_000.0, 2_000.0, 4_000.0, 8_000.0, 16_000.0,
];
pub const LEAD_IN_S: f64 = 1.0;
pub const SWEEP_S: f64 = 4.0;
pub const SEGMENT_S: f64 = 5.5;
const SWEEP_F0: f64 = 25.0;
const SWEEP_F1: f64 = 20_000.0;
const AMPLITUDE: f32 = 0.5;
pub const MAX_CUT_DB: f32 = -9.0;
pub const MAX_BOOST_DB: f32 = 3.0;
/// Bands (indices into BANDS) that may be boosted: 63 Hz – 4 kHz.
const BOOSTABLE: std::ops::RangeInclusive<usize> = 1..=7;
/// Bands must sit this far above the noise floor to be corrected.
const MIN_BAND_SNR_DB: f64 = 10.0;
/// A low band this far below the reference is roll-off, not a room dip.
const ROLLOFF_DB: f64 = 12.0;

pub fn total_frames(outputs: usize, rate: u32) -> u64 {
    ((LEAD_IN_S + SEGMENT_S * outputs as f64) * f64::from(rate)) as u64
}

pub fn soloed_output(frame: u64, outputs: usize, rate: u32) -> Option<usize> {
    let t = frame as f64 / f64::from(rate) - LEAD_IN_S;
    if t < 0.0 {
        return None;
    }
    let k = (t / SEGMENT_S) as usize;
    (k < outputs).then_some(k)
}

/// Exponential sine sweep sample at time `t` seconds into a segment (silent
/// after the sweep), with 50 ms raised-cosine fades to avoid clicks.
pub fn sweep_at(t: f64) -> f32 {
    if !(0.0..SWEEP_S).contains(&t) {
        return 0.0;
    }
    let k = (SWEEP_F1 / SWEEP_F0).ln();
    let phase = 2.0 * PI * SWEEP_F0 * SWEEP_S / k * ((t * k / SWEEP_S).exp() - 1.0);
    let fade = 0.05;
    let gain = if t < fade {
        0.5 - 0.5 * (PI * t / fade).cos()
    } else if t > SWEEP_S - fade {
        0.5 - 0.5 * (PI * (SWEEP_S - t) / fade).cos()
    } else {
        1.0
    };
    (phase.sin() * gain) as f32 * AMPLITUDE
}

/// Track sample at an absolute track frame (lead-in, then one segment per output).
pub fn track_sample(frame: u64, rate: u32) -> f32 {
    let t = frame as f64 / f64::from(rate) - LEAD_IN_S;
    if t < 0.0 {
        return 0.0;
    }
    sweep_at(t % SEGMENT_S)
}

fn band_pass(center: f64, rate: f64) -> Option<Biquad> {
    if center >= rate * 0.45 {
        return None;
    }
    // RBJ band-pass, 0 dB peak, one-octave bandwidth (Q ≈ 1.41).
    let w = 2.0 * PI * center / rate;
    let alpha = w.sin() / (2.0 * std::f64::consts::SQRT_2);
    let a0 = 1.0 + alpha;
    Some(Biquad {
        b0: (alpha / a0) as f32,
        b1: 0.0,
        b2: (-alpha / a0) as f32,
        a1: (-2.0 * w.cos() / a0) as f32,
        a2: ((1.0 - alpha) / a0) as f32,
    })
}

/// Energy of `signal` in each octave band (NaN for bands above Nyquist).
pub fn band_energies(signal: &[f32], rate: u32) -> [f64; 10] {
    let mut out = [f64::NAN; 10];
    for (i, center) in BANDS.iter().enumerate() {
        let Some(bq) = band_pass(*center, f64::from(rate)) else {
            continue;
        };
        let (mut z1, mut z2) = (0.0f32, 0.0f32);
        let mut energy = 0.0f64;
        for &x in signal {
            let y = bq.b0 * x + z1;
            z1 = bq.b1 * x - bq.a1 * y + z2;
            z2 = bq.b2 * x - bq.a2 * y;
            energy += f64::from(y) * f64::from(y);
        }
        out[i] = energy;
    }
    out
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct RoomResult {
    /// Measured response per band relative to the 250 Hz–4 kHz median, dB
    /// (None = too noisy or above Nyquist).
    pub response_db: Vec<Option<f32>>,
    /// Correction to apply per band, dB.
    pub correction_db: [f32; 10],
    /// Signal-to-noise ratio of the whole sweep, dB.
    pub snr_db: f32,
}

/// Analyses one output's window of the recording.
/// `noise` is a stretch of the lead-in (room noise only).
pub fn analyze(window: &[f32], noise: &[f32], rate: u32, target: RoomTarget) -> RoomResult {
    let measured = band_energies(window, rate);
    let noise_e = band_energies(noise, rate);
    // Reference: the same bands of the dry sweep, over the same duration.
    let reference: Vec<f32> = (0..window.len())
        .map(|i| sweep_at(i as f64 / f64::from(rate)))
        .collect();
    let ref_e = band_energies(&reference, rate);
    // Noise scaled to the window length.
    let scale = window.len() as f64 / noise.len().max(1) as f64;

    let mut raw = [f64::NAN; 10];
    let mut usable = [false; 10];
    for i in 0..10 {
        if !(measured[i].is_finite() && ref_e[i] > 0.0 && measured[i] > 0.0) {
            continue;
        }
        raw[i] = 10.0 * (measured[i] / ref_e[i]).log10();
        let n = (noise_e[i] * scale).max(1e-20);
        usable[i] = 10.0 * (measured[i] / n).log10() >= MIN_BAND_SNR_DB;
    }
    // Normalise to the median of the mid bands (250 Hz–4 kHz).
    let mut mids: Vec<f64> = (3..=7).filter(|i| usable[*i]).map(|i| raw[i]).collect();
    mids.sort_by(|a, b| a.partial_cmp(b).unwrap());
    let level = if mids.is_empty() {
        0.0
    } else {
        mids[mids.len() / 2]
    };

    let mut response = [None; 10];
    for i in 0..10 {
        if usable[i] {
            response[i] = Some((raw[i] - level) as f32);
        }
    }
    let total: f64 = measured.iter().filter(|e| e.is_finite()).sum();
    let total_noise: f64 = noise_e.iter().filter(|e| e.is_finite()).sum::<f64>() * scale;
    RoomResult {
        response_db: response.to_vec(),
        correction_db: correction(&response, target),
        // Floors on both sides: a silent recording (e.g. a virtual cable picked
        // as the microphone) gives 0 dB, never -inf, which JSON can't carry.
        snr_db: (10.0 * (total.max(1e-20) / total_noise.max(1e-20)).log10()) as f32,
    }
}

/// What room correction aims for.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RoomTarget {
    /// Equal level in every band.
    Flat,
    /// Gently rising bass and falling treble, closer to how a good speaker in a
    /// room is usually preferred; flat-measured rooms tend to sound thin.
    #[default]
    Warm,
}

impl RoomTarget {
    /// Target level per band (31.5 Hz … 16 kHz) relative to the mids, dB.
    pub fn curve(self) -> [f32; 10] {
        match self {
            RoomTarget::Flat => [0.0; 10],
            RoomTarget::Warm => [3.0, 3.0, 2.0, 1.0, 0.0, 0.0, 0.0, -0.5, -1.5, -2.5],
        }
    }
}

/// Correction per band for a measured response (dB vs the mids) and a target.
pub fn correction(response: &[Option<f32>; 10], target: RoomTarget) -> [f32; 10] {
    let curve = target.curve();
    let mut out = [0.0f32; 10];
    for i in 0..10 {
        let Some(dev) = response[i] else {
            continue;
        };
        // Bass far below the mids is the speaker's natural roll-off: leave it.
        if i <= 2 && f64::from(dev) < -ROLLOFF_DB {
            continue;
        }
        // Correct half the deviation: rooms vary across seats and mics aren't flat.
        let mut c = (-(dev - curve[i]) * 0.5).clamp(MAX_CUT_DB, MAX_BOOST_DB);
        // Voice and headset microphones barely hear the extremes, so a "deficit"
        // there is usually the mic, not the room: only boost 63 Hz – 4 kHz.
        if !BOOSTABLE.contains(&i) {
            c = c.min(0.0);
        }
        if c.abs() < 0.5 {
            c = 0.0;
        }
        out[i] = (c * 10.0).round() / 10.0;
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    const RATE: u32 = 48_000;

    fn sweep(len_s: f64) -> Vec<f32> {
        (0..(len_s * f64::from(RATE)) as usize)
            .map(|i| sweep_at(i as f64 / f64::from(RATE)))
            .collect()
    }

    fn noise(len: usize, level: f32) -> Vec<f32> {
        let mut seed = 99u32;
        (0..len)
            .map(|_| {
                seed ^= seed << 13;
                seed ^= seed >> 17;
                seed ^= seed << 5;
                ((seed as f32 / u32::MAX as f32) - 0.5) * level
            })
            .collect()
    }

    fn filter(signal: &[f32], bq: Biquad) -> Vec<f32> {
        let (mut z1, mut z2) = (0.0f32, 0.0f32);
        signal
            .iter()
            .map(|&x| {
                let y = bq.b0 * x + z1;
                z1 = bq.b1 * x - bq.a1 * y + z2;
                z2 = bq.b2 * x - bq.a2 * y;
                y
            })
            .collect()
    }

    #[test]
    fn schedule_and_sweep_are_bounded() {
        assert_eq!(soloed_output(0, 2, RATE), None);
        assert_eq!(soloed_output(48_000, 2, RATE), Some(0));
        assert_eq!(soloed_output((48_000.0 * 6.6) as u64, 2, RATE), Some(1));
        assert_eq!(soloed_output(total_frames(2, RATE), 2, RATE), None);
        let s = sweep(SWEEP_S);
        assert!(s.iter().all(|x| x.abs() <= AMPLITUDE + 1e-6));
        assert_eq!(sweep_at(4.5), 0.0, "silent gap after the sweep");
    }

    #[test]
    fn flat_speaker_needs_no_correction() {
        let window = sweep(SEGMENT_S);
        let result = analyze(&window, &noise(48_000, 0.0001), RATE, RoomTarget::Flat);
        for (i, c) in result.correction_db.iter().enumerate() {
            assert!(c.abs() < 0.6, "band {} {c}", BANDS[i]);
        }
        assert!(result.snr_db > 30.0);
    }

    #[test]
    fn a_room_peak_is_cut_and_a_dip_gently_boosted() {
        // +8 dB room mode at 125 Hz and a −6 dB dip at 2 kHz.
        let dry = sweep(SEGMENT_S);
        let peaked = filter(&dry, Biquad::peaking(125.0, f64::from(RATE), 8.0, 1.4));
        let shaped = filter(
            &peaked,
            Biquad::peaking(2_000.0, f64::from(RATE), -6.0, 1.4),
        );
        let r = analyze(&shaped, &noise(48_000, 0.0001), RATE, RoomTarget::Flat);
        assert!(
            r.correction_db[2] <= -2.5,
            "125 Hz cut: {:?}",
            r.correction_db
        );
        assert!(
            r.correction_db[6] > 0.5 && r.correction_db[6] <= MAX_BOOST_DB,
            "2 kHz boost: {:?}",
            r.correction_db
        );
        assert!(r.correction_db[5].abs() < 1.5, "1 kHz roughly untouched");
    }

    #[test]
    fn extremes_are_only_ever_cut() {
        // Like a headset mic: weak deep bass and weak top end.
        let dry = sweep(SEGMENT_S);
        let dull = filter(&dry, Biquad::peaking(16_000.0, f64::from(RATE), -9.0, 1.4));
        let thin = filter(&dull, Biquad::peaking(31.5, f64::from(RATE), -8.0, 1.4));
        let r = analyze(&thin, &noise(48_000, 0.0001), RATE, RoomTarget::Flat);
        assert!(
            r.correction_db[9] <= 0.0,
            "no 16 kHz boost: {:?}",
            r.correction_db
        );
        assert!(
            r.correction_db[0] <= 0.0,
            "no 31.5 Hz boost: {:?}",
            r.correction_db
        );
        assert!(
            r.correction_db[8] <= 0.0,
            "no 8 kHz boost: {:?}",
            r.correction_db
        );
        // A peak at the top end is still cut.
        let bright = filter(&dry, Biquad::peaking(16_000.0, f64::from(RATE), 9.0, 1.4));
        let r = analyze(&bright, &noise(48_000, 0.0001), RATE, RoomTarget::Flat);
        assert!(
            r.correction_db[9] < -1.0,
            "16 kHz peak cut: {:?}",
            r.correction_db
        );
    }

    #[test]
    fn bass_rolloff_and_noisy_bands_are_left_alone() {
        // A small speaker: steep high-pass at 150 Hz.
        let dry = sweep(SEGMENT_S);
        let mut small = filter(&dry, Biquad::high_pass(150.0, f64::from(RATE)));
        small = filter(&small, Biquad::high_pass(150.0, f64::from(RATE)));
        let r = analyze(&small, &noise(48_000, 0.0001), RATE, RoomTarget::Flat);
        assert_eq!(r.correction_db[0], 0.0, "no boost at 31 Hz");
        assert_eq!(r.correction_db[1], 0.0, "no boost at 63 Hz");
        // Very loud room noise: nothing is trusted.
        let loud = analyze(&dry, &noise(48_000, 4.0), RATE, RoomTarget::Flat);
        assert!(
            loud.correction_db.iter().all(|c| *c == 0.0),
            "{:?}",
            loud.correction_db
        );
    }

    #[test]
    fn digital_silence_gives_a_finite_snr_that_survives_json() {
        // A virtual cable picked as the "microphone" records exact zeros.
        let silence = vec![0.0f32; (SEGMENT_S * f64::from(RATE)) as usize];
        let quiet_noise = vec![0.0f32; 48_000];
        let r = analyze(&silence, &quiet_noise, RATE, RoomTarget::Warm);
        assert!(r.snr_db.is_finite(), "snr {}", r.snr_db);
        assert!(r.snr_db < 20.0, "silence is never usable");
        assert_eq!(r.correction_db, [0.0; 10]);
        let json = serde_json::to_value(&r).unwrap();
        assert!(json["snr_db"].is_number(), "{json}");
    }

    #[test]
    fn warm_target_keeps_more_bass_and_less_treble_than_flat() {
        // A perfectly flat measurement: Flat changes nothing, Warm lifts the
        // low-mids a little and trims the top.
        let flat_room = [Some(0.0f32); 10];
        assert_eq!(correction(&flat_room, RoomTarget::Flat), [0.0; 10]);
        let warm = correction(&flat_room, RoomTarget::Warm);
        assert!(warm[1] > 1.0 && warm[1] <= MAX_BOOST_DB, "63 Hz {warm:?}");
        assert_eq!(warm[0], 0.0, "31 Hz is never boosted");
        assert!(warm[9] < -1.0, "16 kHz trimmed {warm:?}");
        assert_eq!(warm[5], 0.0, "1 kHz untouched");
        // Unheard bands stay uncorrected.
        let mut partial = flat_room;
        partial[9] = None;
        assert_eq!(correction(&partial, RoomTarget::Warm)[9], 0.0);
    }
}

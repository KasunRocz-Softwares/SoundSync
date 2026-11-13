//! Acoustic calibration: play a click track through each output alone, record
//! it with a microphone at the listening seat, and compute per-output delays so
//! every output is heard at the same moment.
//!
//! Track layout: 1 s lead-in, then `CLICKS_PER_OUTPUT` clicks (one per second)
//! per output, in order. Arrivals are found by folding each output's recording
//! window on the exact 1 s click period, which averages the clicks and rejects
//! sound not locked to them. Arrivals are relative between outputs (± 500 ms).
use serde::{Deserialize, Serialize};

pub const CLICK_PERIOD_S: u64 = 1;
pub const LEAD_IN_S: u64 = 1;
pub const CLICKS_PER_OUTPUT: u64 = 6;
/// Peak-to-median ratio below which an output counts as not heard.
pub const MIN_SNR: f32 = 4.0;
const BURST_MS: u64 = 5;
const AMPLITUDE: f32 = 0.9;

pub fn total_frames(outputs: usize, rate: u32) -> u64 {
    (LEAD_IN_S + CLICKS_PER_OUTPUT * outputs as u64) * u64::from(rate)
}

/// Which output (by order) plays at a track frame; None during the lead-in.
pub fn soloed_output(frame: u64, outputs: usize, rate: u32) -> Option<usize> {
    let second = frame / u64::from(rate);
    if second < LEAD_IN_S {
        return None;
    }
    let index = ((second - LEAD_IN_S) / CLICKS_PER_OUTPUT) as usize;
    (index < outputs).then_some(index)
}

/// Deterministic decaying noise burst at the start of every second.
pub fn click_sample(frame: u64, rate: u32) -> f32 {
    let period = u64::from(rate) * CLICK_PERIOD_S;
    let offset = frame % period;
    let burst = u64::from(rate) * BURST_MS / 1000;
    if frame < u64::from(rate) * LEAD_IN_S || offset >= burst {
        return 0.0;
    }
    // xorshift seeded by position within the burst: identical every click.
    let mut x = (offset as u32)
        .wrapping_mul(2_654_435_761)
        .wrapping_add(0x9E37_79B9);
    x ^= x << 13;
    x ^= x >> 17;
    x ^= x << 5;
    let noise = (x as f32 / u32::MAX as f32) * 2.0 - 1.0;
    noise * AMPLITUDE * (1.0 - offset as f32 / burst as f32)
}

#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct Arrival {
    /// Phase of the strongest arrival within the click period, ms.
    pub phase_ms: f64,
    /// Peak-to-median ratio of the folded envelope.
    pub snr: f32,
}

/// Finds one output's arrival in the mic recording. `track_start` is the mic
/// sample index at which track frame 0 was sent to the outputs.
pub fn analyze_output(
    recording: &[f32],
    rate: u32,
    track_start: i64,
    order: usize,
) -> Option<Arrival> {
    let period = (u64::from(rate) * CLICK_PERIOD_S) as usize;
    // Use the 4 middle seconds of the output's 6: skips the hand-over from the
    // previous output and allows up to ~1 s of output latency.
    let first_second = LEAD_IN_S + CLICKS_PER_OUTPUT * order as u64 + 1;
    let start = track_start + (first_second as i64) * period as i64;
    let periods = 4usize;
    if start < 1 || (start as usize) + periods * period > recording.len() {
        return None;
    }
    let start = start as usize;
    let mut fold = vec![0.0f32; period];
    for p in 0..periods {
        let base = start + p * period;
        for i in 0..period {
            fold[i] += (recording[base + i] - recording[base + i - 1]).abs();
        }
    }
    // 1 ms moving average, circular.
    let win = (rate / 1000).max(1) as usize;
    let mut env = vec![0.0f32; period];
    let mut acc: f32 = fold[period - win..].iter().sum();
    for i in 0..period {
        acc += fold[i] - fold[(i + period - win) % period];
        env[i] = acc;
    }
    let mut sorted = env.clone();
    sorted.sort_by(|a, b| a.partial_cmp(b).unwrap());
    let median = sorted[period / 2].max(1e-9);
    let (peak_index, peak) =
        env.iter().enumerate().fold(
            (0, 0.0f32),
            |best, (i, v)| if *v > best.1 { (i, *v) } else { best },
        );
    // The envelope peaks at the end of its 1 ms window; report the window centre.
    let index = (peak_index + period - win / 2) % period;
    Some(Arrival {
        phase_ms: index as f64 * 1000.0 / f64::from(rate),
        snr: peak / median,
    })
}

pub fn wrap_ms(ms: f64) -> f64 {
    let w = (ms + 500.0).rem_euclid(1000.0) - 500.0;
    if w == -500.0 {
        500.0
    } else {
        w
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct CalibratedOutput {
    pub endpoint_id: String,
    pub name: String,
    pub detected: bool,
    pub snr: f32,
    /// Arrival relative to the first detected output, ms (+ = later).
    pub relative_ms: Option<f64>,
    pub old_delay_ms: u32,
    pub new_delay_ms: u32,
}

/// New delays that make every detected output arrive with the latest one, then
/// remove the common part so the smallest delay is 0. Undetected outputs keep
/// their delay.
pub fn solve_delays(
    arrivals: &[Option<Arrival>],
    current_delays: &[u32],
) -> (Vec<Option<f64>>, Vec<u32>) {
    let reference = arrivals
        .iter()
        .flatten()
        .find(|a| a.snr >= MIN_SNR)
        .map(|a| a.phase_ms);
    let relative: Vec<Option<f64>> = arrivals
        .iter()
        .map(|a| match (a, reference) {
            (Some(a), Some(r)) if a.snr >= MIN_SNR => Some(wrap_ms(a.phase_ms - r)),
            _ => None,
        })
        .collect();
    let latest = relative.iter().flatten().cloned().fold(f64::MIN, f64::max);
    let raw: Vec<Option<f64>> = relative
        .iter()
        .zip(current_delays)
        .map(|(r, d)| r.map(|r| f64::from(*d) + (latest - r)))
        .collect();
    let floor = raw.iter().flatten().cloned().fold(f64::MAX, f64::min);
    let delays = raw
        .iter()
        .zip(current_delays)
        .map(|(r, d)| match r {
            Some(v) => (v - floor).round().clamp(0.0, 1000.0) as u32,
            None => *d,
        })
        .collect();
    (relative, delays)
}

#[cfg(test)]
mod tests {
    use super::*;

    const RATE: u32 = 48_000;

    /// Simulates the room: each output's click arrives `latency_ms` after it is
    /// sent, at `level`, plus background noise.
    fn simulate(latencies_ms: &[f64], levels: &[f32], lead_ms: i64) -> (Vec<f32>, i64) {
        let n = latencies_ms.len();
        let total = total_frames(n, RATE) as usize;
        let track_start = lead_ms * RATE as i64 / 1000;
        let mut rec = vec![0.0f32; track_start as usize + total + RATE as usize * 2];
        let mut seed = 7u32;
        for s in rec.iter_mut() {
            seed ^= seed << 13;
            seed ^= seed >> 17;
            seed ^= seed << 5;
            *s = ((seed as f32 / u32::MAX as f32) - 0.5) * 0.01;
        }
        for frame in 0..total as u64 {
            let Some(k) = soloed_output(frame, n, RATE) else {
                continue;
            };
            let x = click_sample(frame, RATE);
            if x == 0.0 {
                continue;
            }
            let at = track_start as usize
                + frame as usize
                + (latencies_ms[k] * RATE as f64 / 1000.0) as usize;
            rec[at] += x * levels[k];
        }
        (rec, track_start)
    }

    #[test]
    fn schedule_solos_each_output_for_six_seconds() {
        assert_eq!(soloed_output(0, 2, RATE), None);
        assert_eq!(soloed_output(48_000, 2, RATE), Some(0));
        assert_eq!(soloed_output(48_000 * 6 + 47_999, 2, RATE), Some(0));
        assert_eq!(soloed_output(48_000 * 7, 2, RATE), Some(1));
        assert_eq!(soloed_output(48_000 * 13, 2, RATE), None);
        assert_eq!(total_frames(2, RATE), 48_000 * 13);
        assert_eq!(click_sample(10, RATE), 0.0, "silent lead-in");
        assert_ne!(click_sample(48_000 + 10, RATE), 0.0);
        assert_eq!(
            click_sample(48_000 + 10, RATE),
            click_sample(96_000 + 10, RATE)
        );
    }

    #[test]
    fn finds_relative_arrivals_and_solves_delays() {
        // Realtek-like: 120 ms; Bluetooth-like: 470 ms (350 ms later).
        let (rec, start) = simulate(&[120.0, 470.0], &[0.5, 0.2], 37);
        let arrivals: Vec<Option<Arrival>> = (0..2)
            .map(|k| analyze_output(&rec, RATE, start, k))
            .collect();
        for a in &arrivals {
            assert!(a.unwrap().snr > MIN_SNR, "{a:?}");
        }
        let (relative, delays) = solve_delays(&arrivals, &[0, 0]);
        let rel = relative[1].unwrap();
        assert!((rel - 350.0).abs() < 2.0, "relative {rel}");
        assert!((i64::from(delays[0]) - 350).abs() <= 2, "{delays:?}");
        assert_eq!(delays[1], 0);
    }

    #[test]
    fn keeps_existing_delays_in_the_solution() {
        // Already delayed by 254 ms and now 10 ms early: needs 264 ms.
        let (rec, start) = simulate(&[340.0, 350.0], &[0.5, 0.3], 0);
        let arrivals: Vec<Option<Arrival>> = (0..2)
            .map(|k| analyze_output(&rec, RATE, start, k))
            .collect();
        let (_, delays) = solve_delays(&arrivals, &[254, 0]);
        assert!((i64::from(delays[0]) - 264).abs() <= 2, "{delays:?}");
        assert_eq!(delays[1], 0);
    }

    #[test]
    fn silent_output_is_reported_and_keeps_its_delay() {
        let (rec, start) = simulate(&[100.0, 300.0], &[0.5, 0.0], 0);
        let arrivals: Vec<Option<Arrival>> = (0..2)
            .map(|k| analyze_output(&rec, RATE, start, k))
            .collect();
        assert!(arrivals[1].unwrap().snr < MIN_SNR);
        let (relative, delays) = solve_delays(&arrivals, &[0, 40]);
        assert_eq!(relative[1], None);
        assert_eq!(delays, vec![0, 40]);
        assert!(analyze_output(&rec[..1000], RATE, start, 1).is_none());
    }

    #[test]
    fn wraps_relative_times_into_half_a_period() {
        assert_eq!(wrap_ms(-650.0), 350.0);
        assert_eq!(wrap_ms(700.0), -300.0);
        assert_eq!(wrap_ms(-500.0), 500.0);
    }
}

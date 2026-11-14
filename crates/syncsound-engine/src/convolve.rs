//! FIR convolution per output: load an impulse response (a WAV exported from
//! REW, Dirac, Acourate, rePhase…) and convolve that output with it. Uses a
//! uniformly partitioned FFT convolver that adds no latency and does not
//! allocate while processing.
//!
//! Convolvers are built on the control thread and handed to the render thread
//! through `ConvolverSlot`, a pair of atomic pointers: the render thread swaps
//! a new one in and parks the old one for the control thread to free, so it
//! never allocates or frees memory itself.
use fft_convolver::FFTConvolver;
use std::path::Path;
use std::sync::atomic::{AtomicPtr, Ordering};

/// Longest impulse response accepted.
pub const MAX_IR_SECONDS: f64 = 6.0;
const PARTITION: usize = 512;
const CHUNK: usize = 1024;

pub struct StereoConvolver {
    left: FFTConvolver<f32>,
    right: FFTConvolver<f32>,
    in_l: Vec<f32>,
    in_r: Vec<f32>,
    out_l: Vec<f32>,
    out_r: Vec<f32>,
    pub taps: usize,
}

impl std::fmt::Debug for StereoConvolver {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("StereoConvolver").field("taps", &self.taps).finish()
    }
}

impl StereoConvolver {
    pub fn new(ir_left: &[f32], ir_right: &[f32]) -> Result<Self, String> {
        if ir_left.is_empty() || ir_right.is_empty() {
            return Err("The impulse response is empty".into());
        }
        let mut left = FFTConvolver::default();
        let mut right = FFTConvolver::default();
        left.init(PARTITION, ir_left).map_err(|e| e.to_string())?;
        right.init(PARTITION, ir_right).map_err(|e| e.to_string())?;
        Ok(Self {
            left,
            right,
            in_l: vec![0.0; CHUNK],
            in_r: vec![0.0; CHUNK],
            out_l: vec![0.0; CHUNK],
            out_r: vec![0.0; CHUNK],
            taps: ir_left.len().max(ir_right.len()),
        })
    }

    /// Convolves interleaved stereo in place. Real-time safe.
    pub fn process_interleaved(&mut self, buffer: &mut [f32]) {
        for block in buffer.chunks_mut(CHUNK * 2) {
            let frames = block.len() / 2;
            for i in 0..frames {
                self.in_l[i] = block[2 * i];
                self.in_r[i] = block[2 * i + 1];
            }
            let ok = self
                .left
                .process(&self.in_l[..frames], &mut self.out_l[..frames])
                .is_ok()
                && self
                    .right
                    .process(&self.in_r[..frames], &mut self.out_r[..frames])
                    .is_ok();
            for i in 0..frames {
                let (l, r) = if ok {
                    (self.out_l[i], self.out_r[i])
                } else {
                    (0.0, 0.0)
                };
                block[2 * i] = if l.is_finite() { l } else { 0.0 };
                block[2 * i + 1] = if r.is_finite() { r } else { 0.0 };
            }
        }
    }
}

/// Reads a mono or stereo WAV impulse response and resamples it to `rate`.
/// Mono files are used for both channels; extra channels are ignored.
pub fn load_impulse_response(path: &Path, rate: u32) -> Result<(Vec<f32>, Vec<f32>), String> {
    let mut reader =
        hound::WavReader::open(path).map_err(|e| format!("Could not read the WAV file: {e}"))?;
    let spec = reader.spec();
    let channels = usize::from(spec.channels.max(1));
    let frames = reader.duration() as usize;
    if frames as f64 / f64::from(spec.sample_rate) > MAX_IR_SECONDS {
        return Err(format!(
            "The impulse response is longer than {MAX_IR_SECONDS} s"
        ));
    }
    let samples: Vec<f32> = match spec.sample_format {
        hound::SampleFormat::Float => reader
            .samples::<f32>()
            .collect::<Result<_, _>>()
            .map_err(|e| e.to_string())?,
        hound::SampleFormat::Int => {
            let scale = 1.0 / (1u64 << (spec.bits_per_sample.clamp(8, 32) - 1)) as f32;
            reader
                .samples::<i32>()
                .map(|s| s.map(|v| v as f32 * scale))
                .collect::<Result<_, _>>()
                .map_err(|e| e.to_string())?
        }
    };
    let channel = |c: usize| -> Vec<f32> {
        samples
            .iter()
            .skip(c.min(channels - 1))
            .step_by(channels)
            .copied()
            .collect()
    };
    let (l, r) = (channel(0), channel(1));
    if l.iter().chain(&r).any(|x| !x.is_finite()) {
        return Err("The impulse response contains invalid samples".into());
    }
    Ok((
        resample(&l, spec.sample_rate, rate),
        resample(&r, spec.sample_rate, rate),
    ))
}

/// Windowed-sinc-free linear resampling. Impulse responses are smooth enough
/// that this is fine for rate changes such as 44.1 ↔ 48 kHz.
pub fn resample(input: &[f32], from: u32, to: u32) -> Vec<f32> {
    if from == to || input.is_empty() {
        return input.to_vec();
    }
    let ratio = f64::from(from) / f64::from(to);
    let len = ((input.len() as f64) / ratio).round().max(1.0) as usize;
    // Keep the overall gain the same when the rate changes.
    let gain = (ratio as f32).min(1.0);
    (0..len)
        .map(|i| {
            let pos = i as f64 * ratio;
            let a = pos.floor() as usize;
            let frac = (pos - a as f64) as f32;
            let x0 = input.get(a).copied().unwrap_or(0.0);
            let x1 = input.get(a + 1).copied().unwrap_or(0.0);
            (x0 + (x1 - x0) * frac) * gain
        })
        .collect()
}

type Payload = Option<StereoConvolver>;

/// Lock-free hand-off of convolvers to one render thread.
#[derive(Debug)]
pub struct ConvolverSlot {
    pending: AtomicPtr<Payload>,
    retired: AtomicPtr<Payload>,
}

impl Default for ConvolverSlot {
    fn default() -> Self {
        Self {
            pending: AtomicPtr::new(std::ptr::null_mut()),
            retired: AtomicPtr::new(std::ptr::null_mut()),
        }
    }
}

fn free(ptr: *mut Payload) {
    if !ptr.is_null() {
        // SAFETY: every non-null pointer in a slot came from Box::into_raw and is
        // owned by exactly one place at a time (swap transfers ownership).
        drop(unsafe { Box::from_raw(ptr) });
    }
}

impl ConvolverSlot {
    /// Control thread: queue a new convolver (None = turn convolution off).
    pub fn offer(&self, convolver: Payload) {
        self.collect();
        let new = Box::into_raw(Box::new(convolver));
        free(self.pending.swap(new, Ordering::AcqRel));
    }

    /// Control thread: free convolvers the render thread has finished with.
    pub fn collect(&self) {
        free(self.retired.swap(std::ptr::null_mut(), Ordering::AcqRel));
    }

    /// Render thread: swap in a pending convolver, if any. Only succeeds while
    /// the retired slot is free, so `current` can always be parked without
    /// freeing memory here.
    pub fn swap_into(&self, current: &mut Box<Payload>) {
        if self.pending.load(Ordering::Acquire).is_null()
            || !self.retired.load(Ordering::Acquire).is_null()
        {
            return;
        }
        let new = self.pending.swap(std::ptr::null_mut(), Ordering::AcqRel);
        if new.is_null() {
            return;
        }
        // SAFETY: `new` came from Box::into_raw in `offer` and is now ours.
        let old = std::mem::replace(current, unsafe { Box::from_raw(new) });
        let parked = self.retired.swap(Box::into_raw(old), Ordering::AcqRel);
        debug_assert!(parked.is_null());
        free(parked);
    }
}

impl Drop for ConvolverSlot {
    fn drop(&mut self) {
        free(self.pending.swap(std::ptr::null_mut(), Ordering::AcqRel));
        self.collect();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn interleave(l: &[f32], r: &[f32]) -> Vec<f32> {
        l.iter().zip(r).flat_map(|(a, b)| [*a, *b]).collect()
    }

    #[test]
    fn identity_ir_passes_audio_with_no_latency() {
        let mut c = StereoConvolver::new(&[1.0], &[1.0]).unwrap();
        let l: Vec<f32> = (0..3000).map(|i| (i as f32 * 0.01).sin()).collect();
        let r: Vec<f32> = l.iter().map(|x| -x).collect();
        let mut buf = interleave(&l, &r);
        let expected = buf.clone();
        // Odd, varying block sizes, as WASAPI hands out.
        let mut at = 0;
        for size in [441usize, 480, 7, 1500, 3000].iter().cycle() {
            if at >= buf.len() {
                break;
            }
            let end = (at + size * 2).min(buf.len());
            c.process_interleaved(&mut buf[at..end]);
            at = end;
        }
        for (a, b) in buf.iter().zip(&expected) {
            assert!((a - b).abs() < 1e-4, "{a} vs {b}");
        }
    }

    #[test]
    fn delay_and_gain_irs_shift_and_scale_each_channel() {
        let mut ir_l = vec![0.0; 10];
        ir_l[3] = 0.5;
        let mut c = StereoConvolver::new(&ir_l, &[1.0]).unwrap();
        let mut buf = vec![0.0f32; 40];
        buf[0] = 1.0; // impulse on the left at frame 0
        buf[1] = 1.0; // and on the right
        c.process_interleaved(&mut buf);
        assert!((buf[2 * 3] - 0.5).abs() < 1e-5, "left delayed by 3 frames");
        assert!(buf[0].abs() < 1e-5);
        assert!((buf[1] - 1.0).abs() < 1e-5, "right unchanged");
    }

    #[test]
    fn slot_hands_over_without_the_render_side_freeing() {
        let slot = ConvolverSlot::default();
        let mut current: Box<Payload> = Box::new(None);
        slot.swap_into(&mut current);
        assert!(current.is_none(), "nothing pending");
        slot.offer(Some(StereoConvolver::new(&[1.0], &[1.0]).unwrap()));
        slot.swap_into(&mut current);
        assert!(current.is_some());
        // The render side parked the old payload instead of freeing it.
        assert!(!slot.retired.load(Ordering::Acquire).is_null());
        // While something is parked, a new pending one waits.
        let parked = slot.retired.swap(std::ptr::null_mut(), Ordering::AcqRel);
        slot.pending
            .store(Box::into_raw(Box::new(None)), Ordering::Release);
        slot.retired.store(parked, Ordering::Release);
        slot.swap_into(&mut current);
        assert!(current.is_some(), "retired slot still occupied");
        // offer() collects first, so the hand-over goes through.
        slot.offer(None);
        slot.swap_into(&mut current);
        assert!(current.is_none(), "turned off");
    }

    #[test]
    fn resampling_keeps_length_proportional() {
        let x = vec![1.0f32; 441];
        let y = resample(&x, 44_100, 48_000);
        assert_eq!(y.len(), 480);
        let z = resample(&x, 48_000, 48_000);
        assert_eq!(z.len(), 441);
    }
}

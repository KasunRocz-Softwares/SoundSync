//! Converts rendered stereo f32 audio at the engine rate into 20 ms chunks of
//! 48 kHz 16-bit stereo for network speakers. Real-time safe: all buffers are
//! fixed-size and nothing allocates after construction.

use super::{CHUNK_FRAMES, STREAM_RATE};

const CHUNK_SAMPLES: usize = CHUNK_FRAMES * 2;

pub struct StreamConverter {
    source_rate: u32,
    /// Source frames advanced per output frame.
    step: f64,
    /// Position of the next output frame, in source frames after `prev`.
    pos: f64,
    prev: [f32; 2],
    has_prev: bool,
    chunk: [i16; CHUNK_SAMPLES],
    filled: usize,
}

#[inline]
fn to_i16(x: f32) -> i16 {
    let v = (x * 32767.0).round();
    if v >= 32767.0 {
        32767
    } else if v <= -32768.0 {
        -32768
    } else {
        v as i16
    }
}

impl StreamConverter {
    pub fn new(source_rate: u32) -> Self {
        let source_rate = source_rate.max(1);
        Self {
            source_rate,
            step: source_rate as f64 / STREAM_RATE as f64,
            pos: 0.0,
            prev: [0.0; 2],
            has_prev: false,
            chunk: [0; CHUNK_SAMPLES],
            filled: 0,
        }
    }

    /// Drops any partial chunk and resampler history.
    pub fn reset(&mut self) {
        self.pos = 0.0;
        self.prev = [0.0; 2];
        self.has_prev = false;
        self.filled = 0;
    }

    #[inline]
    fn emit(&mut self, l: f32, r: f32, out: &mut impl FnMut(&[i16; CHUNK_SAMPLES])) {
        self.chunk[self.filled] = to_i16(l);
        self.chunk[self.filled + 1] = to_i16(r);
        self.filled += 2;
        if self.filled == CHUNK_SAMPLES {
            out(&self.chunk);
            self.filled = 0;
        }
    }

    /// `stereo` is interleaved L,R at the source rate. `out` is called once per
    /// completed 960-frame chunk; a partial chunk carries over to the next push.
    pub fn push(&mut self, stereo: &[f32], out: &mut impl FnMut(&[i16; CHUNK_FRAMES * 2])) {
        if self.source_rate == STREAM_RATE {
            for f in stereo.chunks_exact(2) {
                self.emit(f[0], f[1], out);
            }
            return;
        }
        for f in stereo.chunks_exact(2) {
            let cur = [f[0], f[1]];
            if !self.has_prev {
                self.prev = cur;
                self.has_prev = true;
                continue;
            }
            while self.pos < 1.0 {
                let t = self.pos as f32;
                let l = self.prev[0] + (cur[0] - self.prev[0]) * t;
                let r = self.prev[1] + (cur[1] - self.prev[1]) * t;
                self.emit(l, r, out);
                self.pos += self.step;
            }
            self.pos -= 1.0;
            self.prev = cur;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn run(rate: u32, input: &[f32]) -> Vec<i16> {
        let mut c = StreamConverter::new(rate);
        let mut all = Vec::new();
        c.push(input, &mut |ch| all.extend_from_slice(ch));
        all
    }

    fn sine(rate: u32, hz: f32, frames: usize) -> Vec<f32> {
        let mut v = Vec::with_capacity(frames * 2);
        for i in 0..frames {
            let s = (2.0 * std::f32::consts::PI * hz * i as f32 / rate as f32).sin() * 0.5;
            v.push(s);
            v.push(s);
        }
        v
    }

    #[test]
    fn passes_48k_through_as_full_scale_i16() {
        let input: Vec<f32> = (0..960).flat_map(|_| [0.5, -0.5]).collect();
        let out = run(48_000, &input);
        assert_eq!(out.len(), CHUNK_FRAMES * 2);
        for p in out.chunks(2) {
            assert!((p[0] as i32 - 16384).abs() <= 1);
            assert!((p[1] as i32 + 16384).abs() <= 1);
        }
    }

    #[test]
    fn converts_96k_to_48k_at_correct_pitch() {
        let out = run(96_000, &sine(96_000, 1000.0, 96_000));
        let chunks = out.len() / (CHUNK_FRAMES * 2);
        assert!((49..=51).contains(&chunks), "chunks {chunks}");
        let left: Vec<i16> = out.chunks(2).map(|p| p[0]).collect();
        let crossings = left
            .windows(2)
            .filter(|w| w[0] < 0 && w[1] >= 0)
            .count();
        let secs = left.len() as f32 / 48_000.0;
        let hz = crossings as f32 / secs;
        assert!((hz - 1000.0).abs() <= 5.0, "hz {hz}");
    }

    #[test]
    fn converts_44k1_to_48k() {
        let out = run(44_100, &sine(44_100, 440.0, 44_100));
        let chunks = out.len() / (CHUNK_FRAMES * 2);
        assert!((49..=51).contains(&chunks), "chunks {chunks}");
    }

    #[test]
    fn clamps_overrange() {
        let mut input = Vec::new();
        for _ in 0..960 {
            input.extend_from_slice(&[2.0, -2.0]);
        }
        let out = run(48_000, &input);
        assert_eq!(out[0], 32767);
        assert_eq!(out[1], -32768);
    }

    #[test]
    fn reset_drops_partial_chunk() {
        let mut c = StreamConverter::new(48_000);
        let mut chunks: Vec<Vec<i16>> = Vec::new();
        let first: Vec<f32> = (0..500).flat_map(|_| [0.1, 0.1]).collect();
        c.push(&first, &mut |ch| chunks.push(ch.to_vec()));
        assert!(chunks.is_empty());
        c.reset();
        let second: Vec<f32> = (0..960).flat_map(|_| [0.5, 0.5]).collect();
        c.push(&second, &mut |ch| chunks.push(ch.to_vec()));
        assert_eq!(chunks.len(), 1);
        assert!((chunks[0][0] as i32 - 16384).abs() <= 1);
    }
}

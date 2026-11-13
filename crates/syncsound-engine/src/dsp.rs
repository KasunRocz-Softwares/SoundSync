#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DspError {
    InvalidInput,
    UnsupportedChannels,
    OutputTooSmall,
}

/// Converts only explicitly supported layouts. Buffers are caller-owned.
pub fn map_channels(
    input: &[f32],
    input_channels: u16,
    output_channels: u16,
    output: &mut [f32],
) -> Result<usize, DspError> {
    if !(1..=2).contains(&input_channels) || !(1..=2).contains(&output_channels) {
        return Err(DspError::UnsupportedChannels);
    }
    let from = usize::from(input_channels);
    let to = usize::from(output_channels);
    if input.len() % from != 0 {
        return Err(DspError::InvalidInput);
    }
    let frames = input.len() / from;
    if output.len() < frames * to {
        return Err(DspError::OutputTooSmall);
    }
    for frame in 0..frames {
        let src = frame * from;
        let dst = frame * to;
        match (from, to) {
            (1, 1) => output[dst] = input[src],
            (1, 2) => {
                output[dst] = input[src];
                output[dst + 1] = input[src];
            }
            (2, 1) => output[dst] = (input[src] + input[src + 1]) * 0.5,
            (2, 2) => output[dst..dst + 2].copy_from_slice(&input[src..src + 2]),
            _ => unreachable!(),
        }
    }
    Ok(frames)
}

/// Linear interpolation of interleaved input. `ratio` is source frames per output
/// frame, including any small clock correction. `start_phase` is a fractional source
/// frame offset. The caller supplies contiguous source windows with one extra frame
/// for the final interpolation and maintains phase across windows. Returns frames
/// produced, without allocating or changing either buffer's capacity.
pub fn resample_interleaved(
    input: &[f32],
    channels: u16,
    ratio: f64,
    start_phase: f64,
    output: &mut [f32],
) -> Result<usize, DspError> {
    let channels = usize::from(channels);
    if channels == 0
        || input.len() % channels != 0
        || output.len() % channels != 0
        || !ratio.is_finite()
        || ratio <= 0.0
        || !start_phase.is_finite()
        || start_phase < 0.0
    {
        return Err(DspError::InvalidInput);
    }
    let input_frames = input.len() / channels;
    let output_frames = output.len() / channels;
    let mut produced = 0;
    while produced < output_frames {
        let position = start_phase + produced as f64 * ratio;
        if position >= input_frames.saturating_sub(1) as f64 {
            break;
        }
        let left = position as usize;
        let fraction = (position - left as f64) as f32;
        for channel in 0..channels {
            let a = input[left * channels + channel];
            let b = input[(left + 1) * channels + channel];
            output[produced * channels + channel] = a + (b - a) * fraction;
        }
        produced += 1;
    }
    Ok(produced)
}

/// Fixed delay in complete frames; constructed before entering the audio path.
pub struct DelayLine {
    channels: usize,
    storage: Vec<f32>,
    cursor: usize,
}

impl DelayLine {
    pub fn new(delay_frames: usize, channels: u16) -> Result<Self, DspError> {
        if channels == 0 {
            return Err(DspError::InvalidInput);
        }
        let samples = delay_frames
            .checked_mul(usize::from(channels))
            .ok_or(DspError::InvalidInput)?;
        Ok(Self {
            channels: usize::from(channels),
            storage: vec![0.0; samples],
            cursor: 0,
        })
    }

    pub fn storage_capacity_samples(&self) -> usize {
        self.storage.capacity()
    }

    pub fn reset(&mut self) {
        self.storage.fill(0.0);
        self.cursor = 0;
    }

    pub fn process(&mut self, input: &[f32], output: &mut [f32]) -> Result<(), DspError> {
        if input.len() % self.channels != 0 {
            return Err(DspError::InvalidInput);
        }
        if output.len() < input.len() {
            return Err(DspError::OutputTooSmall);
        }
        if self.storage.is_empty() {
            output[..input.len()].copy_from_slice(input);
            return Ok(());
        }
        for (source, destination) in input.iter().zip(output.iter_mut()) {
            *destination = self.storage[self.cursor];
            self.storage[self.cursor] = *source;
            self.cursor = (self.cursor + 1) % self.storage.len();
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::{map_channels, resample_interleaved, DelayLine, DspError};

    #[test]
    fn maps_mono_and_stereo_explicitly() {
        let mut stereo = [0.; 4];
        assert_eq!(map_channels(&[1., -1.], 1, 2, &mut stereo), Ok(2));
        assert_eq!(stereo, [1., 1., -1., -1.]);
        let mut mono = [0.; 2];
        assert_eq!(map_channels(&[1., 3., -1., 1.], 2, 1, &mut mono), Ok(2));
        assert_eq!(mono, [2., 0.]);
        assert_eq!(
            map_channels(&[1., 2., 3.], 3, 2, &mut stereo),
            Err(DspError::UnsupportedChannels)
        );
    }

    #[test]
    fn resamples_44100_48000_and_96000_to_48000() {
        for rate in [44_100, 48_000, 96_000] {
            let input: Vec<f32> = (0..=rate / 100).map(|i| i as f32).collect();
            let mut output = [0.; 480];
            let n =
                resample_interleaved(&input, 1, rate as f64 / 48_000.0, 0.0, &mut output).unwrap();
            assert_eq!(n, 480, "rate {rate}");
            assert!((output[100] - 100.0 * rate as f32 / 48_000.0).abs() < 0.001);
        }
    }

    #[test]
    fn interpolation_uses_both_channels_without_cross_talk() {
        let mut output = [0.; 4];
        assert_eq!(
            resample_interleaved(&[0., 10., 2., 12., 4., 14.], 2, 0.5, 0.0, &mut output),
            Ok(2)
        );
        assert_eq!(output, [0., 10., 1., 11.]);
    }

    #[test]
    fn delay_compensates_and_reset_discards_history() {
        let mut delay = DelayLine::new(2, 1).unwrap();
        assert_eq!(delay.storage_capacity_samples(), 2);
        let mut out = [0.; 3];
        assert_eq!(delay.process(&[1., 2., 3.], &mut out), Ok(()));
        assert_eq!(out, [0., 0., 1.]);
        assert_eq!(delay.process(&[4., 5., 6.], &mut out), Ok(()));
        assert_eq!(out, [2., 3., 4.]);
        delay.reset();
        assert_eq!(delay.process(&[7., 8., 9.], &mut out), Ok(()));
        assert_eq!(out, [0., 0., 7.]);
        assert_eq!(delay.storage_capacity_samples(), 2);
    }
}

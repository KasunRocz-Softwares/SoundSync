use serde::Serialize;

/// Interleaved float32 PCM layout; frame positions are independent of sample count.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
pub struct PcmFormat {
    pub sample_rate: u32,
    pub channels: u16,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct PcmBlock {
    pub first_frame: u64,
    pub format: PcmFormat,
    pub samples: Vec<f32>,
}

impl PcmBlock {
    pub fn frames(&self) -> Option<usize> {
        let channels = usize::from(self.format.channels);
        if self.format.sample_rate == 0 || channels == 0 || self.samples.len() % channels != 0 {
            return None;
        }
        Some(self.samples.len() / channels)
    }
}

#[cfg(test)]
mod tests {
    use super::{PcmBlock, PcmFormat};

    #[test]
    fn frame_count_uses_interleaved_channels() {
        let block = PcmBlock {
            first_frame: 42,
            format: PcmFormat {
                sample_rate: 48_000,
                channels: 2,
            },
            samples: vec![1.0, 2.0, 3.0, 4.0],
        };
        assert_eq!(block.frames(), Some(2));
    }

    #[test]
    fn incomplete_frame_is_rejected() {
        let block = PcmBlock {
            first_frame: 0,
            format: PcmFormat {
                sample_rate: 48_000,
                channels: 2,
            },
            samples: vec![1.0],
        };
        assert_eq!(block.frames(), None);
    }
}

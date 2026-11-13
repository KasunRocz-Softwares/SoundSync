//! SyncSound audio engine v0.1: endpoint discovery and synchronized test-signal output.
//! System-audio WASAPI loopback capture is not yet implemented. Do not claim system mirroring works.
pub mod audio_format;
pub mod dsp;
pub mod metrics;
pub mod ring_buffer;
pub mod timeline;

pub use audio_format::{PcmBlock, PcmFormat};
pub use dsp::{map_channels, resample_interleaved, DelayLine};
pub use ring_buffer::{BoundedPcmQueue, PcmConsumer, PcmProducer, QueueStatus};
pub use timeline::{ClockEstimator, MasterTimeline};
use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use cpal::{SampleFormat, Stream};
use serde::Serialize;
use std::{collections::HashMap, f32::consts::TAU, sync::{Arc, Mutex}};

#[derive(Clone, Debug, Serialize)]
pub struct AudioDevice { pub id: String, pub name: String, pub is_default: bool, pub channels: u16, pub sample_rate: u32 }

#[derive(Clone, Copy, Debug, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum AudioMode { Gaming, Balanced, Synchronized }

#[derive(Debug, thiserror::Error)]
pub enum EngineError {
    #[error("Audio device error: {0}")] Device(String),
    #[error("Unsupported output format: {0}")] Format(String),
}

struct Output { _stream: Stream, gain: Arc<Mutex<f32>> }

pub struct Engine { outputs: HashMap<String, Output> }

impl Default for Engine { fn default() -> Self { Self { outputs: HashMap::new() } } }

impl Engine {
    pub fn devices() -> Result<Vec<AudioDevice>, EngineError> {
        let host = cpal::default_host();
        let default_name = host.default_output_device().and_then(|d| d.name().ok());
        let devices = host.output_devices().map_err(|e| EngineError::Device(e.to_string()))?;
        let mut result = Vec::new();
        for (index, device) in devices.enumerate() {
            let name = device.name().unwrap_or_else(|_| format!("Output {index}"));
            let (channels, sample_rate) = device.default_output_config().map(|c| (c.channels(), c.sample_rate().0)).unwrap_or((0,0));
            // v0.1 identifiers are session-local; persistent Windows endpoint IDs are future work.
            result.push(AudioDevice { id: index.to_string(), is_default: default_name.as_deref() == Some(name.as_str()), name, channels, sample_rate });
        }
        Ok(result)
    }

    /// Plays a 440 Hz diagnostic signal on one output. NOT system-audio capture.
    /// Independent device clocks mean this is NOT sample-accurate synchronized playback.
    pub fn start_test_tone(&mut self, id: &str) -> Result<(), EngineError> {
        if self.outputs.contains_key(id) { return Ok(()); }
        let index: usize = id.parse().map_err(|_| EngineError::Device("Invalid device ID".into()))?;
        let device = cpal::default_host().output_devices().map_err(|e| EngineError::Device(e.to_string()))?
            .nth(index).ok_or_else(|| EngineError::Device("Device not found; refresh device list".into()))?;
        let supported = device.default_output_config().map_err(|e| EngineError::Device(e.to_string()))?;
        let sample_format = supported.sample_format();
        let config: cpal::StreamConfig = supported.into();
        let rate = config.sample_rate.0 as f32;
        let channels = config.channels as usize;
        let gain = Arc::new(Mutex::new(0.12_f32));
        let gain_for_stream = Arc::clone(&gain);
        let mut phase = 0.0_f32;
        let mut render = move |buffer: &mut [f32]| {
            // Diagnostic implementation; production callback must avoid mutexes.
            let volume = gain_for_stream.try_lock().map(|v| *v).unwrap_or(0.0);
            for frame in buffer.chunks_mut(channels) {
                let sample = phase.sin() * volume;
                phase = (phase + TAU * 440.0 / rate) % TAU;
                for s in frame { *s = sample; }
            }
        };
        let err_fn = |err| eprintln!("SyncSound output error: {err}");
        let stream = match sample_format {
            SampleFormat::F32 => device.build_output_stream(&config, move |data: &mut [f32], _| render(data), err_fn, None),
            _ => return Err(EngineError::Format(format!("{sample_format:?}; select a float32-capable endpoint for this prototype"))),
        }.map_err(|e| EngineError::Device(e.to_string()))?;
        stream.play().map_err(|e| EngineError::Device(e.to_string()))?;
        self.outputs.insert(id.to_string(), Output { _stream: stream, gain });
        Ok(())
    }
    pub fn stop(&mut self, id: &str) { self.outputs.remove(id); }
    pub fn stop_all(&mut self) { self.outputs.clear(); }
    pub fn set_gain(&mut self, id: &str, gain: f32) -> Result<(), EngineError> {
        let output = self.outputs.get(id).ok_or_else(|| EngineError::Device("Device is not playing".into()))?;
        *output.gain.lock().map_err(|_| EngineError::Device("Gain lock poisoned".into()))? = gain.clamp(0.0, 1.0);
        Ok(())
    }
    pub fn active_ids(&self) -> Vec<String> { self.outputs.keys().cloned().collect() }
}

//! SyncSound audio engine: Windows WASAPI loopback capture routed to multiple
//! independently buffered WASAPI render endpoints, with drift correction, delay,
//! gain/mute, per-output recovery, telemetry, and versioned profiles.
//!
//! Synchronization is estimated from Windows-reported buffer and stream latency; it
//! is not an acoustic measurement and never claims zero latency.
pub mod audio_format;
pub mod capture;
pub mod commands;
pub mod diagnostics;
pub mod dsp;
pub mod eq;
pub mod metrics;
pub mod output_state;
pub mod placement;
pub mod platform;
pub mod profile;
pub mod render;
pub mod ring_buffer;
pub mod routing;
pub mod spatial;
pub mod timeline;

pub use audio_format::{PcmBlock, PcmFormat};
#[cfg(windows)]
pub use capture::LoopbackCapture;
pub use capture::{AudioEndpoint, CaptureConfig, CaptureSource, CaptureState};
pub use dsp::{map_channels, resample_interleaved, DelayLine};
pub use output_state::{OutputLifecycle, OutputLifecycleState};
#[cfg(windows)]
pub use platform::windows_engine::{AudioEngine, EngineError};
pub use render::{AudioMode, ModePolicy, OutputControls, OutputMetrics, RenderPipeline};
pub use ring_buffer::{BoundedPcmQueue, PcmConsumer, PcmProducer, QueueStatus};
pub use routing::{EngineSnapshot, OutputConfig, OutputStatus};
pub use timeline::{ClockEstimator, MasterTimeline};

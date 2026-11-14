//! Portable routing model: output configuration, published engine snapshots,
//! and Synchronized-mode alignment policy.
use crate::capture::{AudioEndpoint, CaptureSource, CaptureState};
use crate::eq::EqSettings;
use crate::output_state::OutputLifecycleState;
use crate::placement::SpeakerPosition;
use crate::render::{frames_to_ms, AudioMode, ChannelRole, OutputMetricsSnapshot, MAX_ALIGN_MS};
use crate::spatial::{SpatialConfig, StereoSpread};
use crate::PcmFormat;
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct OutputConfig {
    pub endpoint_id: String,
    pub enabled: bool,
    pub gain: f32,
    pub muted: bool,
    pub delay_ms: u32,
    #[serde(default)]
    pub role: ChannelRole,
    #[serde(default)]
    pub position: SpeakerPosition,
    /// Distance from the listening position, if the user entered one.
    #[serde(default)]
    pub distance_m: Option<f32>,
    /// Level trim in dB (≤ 0), e.g. −3 dB for rear speakers.
    #[serde(default)]
    pub trim_db: f32,
    /// Exact angle on the room map (0 = front, + = right). None = from `position`.
    #[serde(default)]
    pub azimuth_deg: Option<f32>,
    #[serde(default)]
    pub spread: StereoSpread,
    /// Receives the low-passed LFE feed in spatial mode instead of the panned mix.
    #[serde(default)]
    pub subwoofer: bool,
    #[serde(default)]
    pub eq: EqSettings,
}

impl OutputConfig {
    pub fn new(endpoint_id: impl Into<String>) -> Self {
        Self {
            endpoint_id: endpoint_id.into(),
            enabled: false,
            gain: 1.0,
            muted: false,
            delay_ms: 0,
            role: ChannelRole::Stereo,
            position: SpeakerPosition::Unassigned,
            distance_m: None,
            trim_db: 0.0,
            azimuth_deg: None,
            spread: StereoSpread::Point,
            subwoofer: false,
            eq: EqSettings::default(),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct OutputStatus {
    pub endpoint_id: String,
    pub name: String,
    pub endpoint_available: bool,
    pub config: OutputConfig,
    pub state: OutputLifecycleState,
    pub last_error: Option<String>,
    pub retry_in_ms: Option<u64>,
    /// Automatic Synchronized-mode delay added on top of the manual delay.
    pub align_ms: f64,
    /// Delay added to compensate for speaker distance.
    #[serde(default)]
    pub placement_delay_ms: f64,
    /// Linear level from distance compensation × trim.
    #[serde(default = "unity")]
    pub placement_level: f32,
    /// Angle used for spatial rendering (explicit map angle or the position's angle).
    #[serde(default)]
    pub resolved_azimuth: Option<f32>,
    pub buffered_ms: f64,
    /// Windows-reported endpoint buffer + stream latency. Excludes Bluetooth/HDMI
    /// transport and acoustic delays Windows does not report.
    pub device_latency_ms: f64,
    pub estimated_latency_ms: f64,
    pub metrics: OutputMetricsSnapshot,
    /// Bluetooth: Windows does not report codec/radio latency for this output.
    #[serde(default)]
    pub bluetooth: bool,
    pub warning: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct CalibrationProgress {
    pub elapsed_s: f64,
    pub total_s: f64,
    /// Output currently playing clicks.
    pub playing: Option<String>,
    /// True for a room (frequency-sweep) measurement.
    #[serde(default)]
    pub measuring_room: bool,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct RoomOutput {
    pub endpoint_id: String,
    pub name: String,
    /// Correction was stored in this output's EQ.
    pub applied: bool,
    pub result: Option<crate::room::RoomResult>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct RoomReport {
    pub microphone: String,
    pub message: String,
    pub outputs: Vec<RoomOutput>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct CalibrationReport {
    pub microphone: String,
    /// Delays were changed (at least two outputs were heard clearly).
    pub applied: bool,
    pub message: String,
    pub outputs: Vec<crate::calibration::CalibratedOutput>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct OutputSpectrum {
    pub endpoint_id: String,
    /// Octave bands 31.5 Hz … 16 kHz, dBFS.
    pub levels_db: [f32; 10],
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct AppSourceStatus {
    pub exe: String,
    /// None = not running / no audio session yet (retried every 3 s).
    pub pid: Option<u32>,
    pub capturing: bool,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ErrorRecord {
    pub at_ms: u64,
    pub scope: String,
    pub message: String,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct EngineSnapshot {
    pub running: bool,
    pub mode: AudioMode,
    pub source: CaptureSource,
    pub capture_state: CaptureState,
    pub capture_format: PcmFormat,
    pub outputs: Vec<OutputStatus>,
    pub distance_compensation: bool,
    pub spatial: SpatialConfig,
    /// Tilt, loudness, crossover, normalize, night mode.
    pub sound: crate::enhance::SoundSettings,
    /// The listening mode the user's tone settings match (Custom if none).
    pub listening_mode: crate::enhance::ListeningMode,
    /// Apps that switch the listening mode while they play.
    pub app_modes: Vec<crate::enhance::AppMode>,
    /// The playing app whose mode is overriding the tone right now.
    pub auto_mode: Option<crate::enhance::AppMode>,
    /// Per-app routing status (source = Apps).
    pub app_sources: Vec<AppSourceStatus>,
    /// Present while a microphone calibration is running.
    pub calibration: Option<CalibrationProgress>,
    pub endpoints: Vec<AudioEndpoint>,
    pub recent_errors: Vec<ErrorRecord>,
    pub updated_at_ms: u64,
}

impl EngineSnapshot {
    pub fn idle(format: PcmFormat) -> Self {
        Self {
            running: false,
            mode: AudioMode::default(),
            source: CaptureSource::default(),
            capture_state: CaptureState::Stopped,
            capture_format: format,
            outputs: Vec::new(),
            distance_compensation: true,
            spatial: SpatialConfig::default(),
            sound: crate::enhance::SoundSettings::default(),
            listening_mode: crate::enhance::ListeningMode::Custom,
            app_modes: Vec::new(),
            auto_mode: None,
            app_sources: Vec::new(),
            calibration: None,
            endpoints: Vec::new(),
            recent_errors: Vec::new(),
            updated_at_ms: 0,
        }
    }
}

fn unity() -> f32 {
    1.0
}

/// Synchronized mode: delay every output to the slowest *estimated* device latency.
/// Unknown latencies get no alignment. Result is clamped to `MAX_ALIGN_MS`.
pub fn alignment_frames(device_latency_frames: &[Option<u64>], sample_rate: u32) -> Vec<u32> {
    let max_align = u64::from(MAX_ALIGN_MS) * u64::from(sample_rate) / 1_000;
    let slowest = device_latency_frames.iter().flatten().copied().max();
    device_latency_frames
        .iter()
        .map(|latency| match (latency, slowest) {
            (Some(latency), Some(slowest)) => (slowest - latency).min(max_align) as u32,
            _ => 0,
        })
        .collect()
}

/// Gaming mode warning when an output's estimated latency exceeds the budget.
/// Gaming mode warning when the latency SyncSound itself adds (buffer + endpoint)
/// exceeds the budget. Delay the user dialled in on purpose, and automatic
/// alignment delay, are excluded: they are intentional, not a fault.
pub fn latency_warning(
    mode: AudioMode,
    estimated_ms: f64,
    intentional_delay_ms: f64,
) -> Option<String> {
    let budget = mode.policy().latency_warning_ms?;
    let added = (estimated_ms - intentional_delay_ms).max(0.0);
    (added > f64::from(budget)).then(|| {
        format!(
            "SyncSound adds an estimated {added:.0} ms here (not counting your delay), over the Gaming budget of {budget} ms"
        )
    })
}

pub fn metrics_ms(metrics: &OutputMetricsSnapshot, rate: u32) -> (f64, f64, f64) {
    (
        frames_to_ms(metrics.buffered_frames, rate),
        frames_to_ms(metrics.device_latency_frames(), rate),
        frames_to_ms(metrics.estimated_latency_frames(), rate),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn aligns_faster_outputs_to_slowest_estimate() {
        let aligned = alignment_frames(&[Some(480), Some(4_800), None, Some(960)], 48_000);
        assert_eq!(aligned, [4_320, 0, 0, 3_840]);
    }

    #[test]
    fn alignment_is_clamped_and_empty_safe() {
        assert_eq!(alignment_frames(&[], 48_000), Vec::<u32>::new());
        assert_eq!(alignment_frames(&[None, None], 48_000), [0, 0]);
        let aligned = alignment_frames(&[Some(0), Some(96_000)], 48_000);
        assert_eq!(aligned, [24_000, 0]);
    }

    #[test]
    fn gaming_budget_warns_only_in_gaming_mode() {
        assert!(latency_warning(AudioMode::Gaming, 75.0, 0.0).is_some());
        assert!(latency_warning(AudioMode::Gaming, 30.0, 0.0).is_none());
        assert!(latency_warning(AudioMode::Synchronized, 500.0, 0.0).is_none());
    }

    #[test]
    fn deliberate_delay_does_not_trigger_gaming_warning() {
        // Realtek case: 254 ms manual delay + ~30 ms SyncSound latency.
        assert!(latency_warning(AudioMode::Gaming, 284.0, 254.0).is_none());
        assert!(latency_warning(AudioMode::Gaming, 340.0, 254.0).is_some());
        assert!(latency_warning(AudioMode::Gaming, 10.0, 254.0).is_none());
    }
}

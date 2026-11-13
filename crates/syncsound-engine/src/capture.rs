//! Portable capture policy and bounded packet publication. No COM types live here.
use crate::{BoundedPcmQueue, MasterTimeline, PcmConsumer, PcmFormat, PcmProducer, QueueStatus};
use serde::Serialize;

pub const EXCLUSION_SCOPE_REASON: &str = "Same-endpoint routing is blocked: Windows process exclusion is system-wide, not endpoint-scoped";

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
pub enum EndpointState {
    Active,
    Unavailable,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct AudioEndpoint {
    /// Opaque IMMDevice::GetId endpoint ID, never an enumeration index or friendly name.
    pub id: String,
    pub name: String,
    pub is_default: bool,
    pub state: EndpointState,
    pub mix_format: Option<PcmFormat>,
    pub bits_per_sample: Option<u16>,
    pub channel_mask: Option<u32>,
    /// Best-effort Windows form-factor label; None means unknown, not inferred from name.
    pub connection_type: Option<String>,
    /// PnP enumerator of the endpoint's device (e.g. "BTHENUM", "USB", "HDAUDIO").
    #[serde(default)]
    pub bus: Option<String>,
}

impl AudioEndpoint {
    /// Bluetooth transport (codec + radio) latency is not included in what
    /// Windows reports for the endpoint, so estimates for these are optimistic.
    pub fn is_bluetooth(&self) -> bool {
        self.bus.as_deref().is_some_and(|bus| {
            bus.eq_ignore_ascii_case("BTHENUM") || bus.eq_ignore_ascii_case("BTHLEDEVICE")
        })
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize)]
pub enum CaptureSource {
    #[default]
    DefaultRender,
    Endpoint(String),
    /// Explicit opt-in: captures all endpoints except this process and its children.
    /// Windows cannot combine this API with a selected endpoint filter.
    SystemExcludingSelf,
}

#[derive(Clone, Debug)]
pub struct CaptureConfig {
    pub source: CaptureSource,
    pub format: PcmFormat,
    pub queue_blocks: usize,
    pub max_block_frames: usize,
    /// Complete set of SyncSound render endpoints. Update BEFORE starting a render stream.
    /// IDs must be resolved endpoints, not a dynamic "default output" alias.
    pub output_endpoint_ids: Vec<String>,
}

impl Default for CaptureConfig {
    fn default() -> Self {
        Self {
            source: CaptureSource::DefaultRender,
            format: PcmFormat {
                sample_rate: 48_000,
                channels: 2,
            },
            queue_blocks: 8,
            max_block_frames: 480,
            output_endpoint_ids: Vec::new(),
        }
    }
}

impl CaptureConfig {
    pub fn excluded_process_id(&self) -> Option<u32> {
        matches!(self.source, CaptureSource::SystemExcludingSelf).then(std::process::id)
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub enum CaptureTarget {
    Endpoint(String),
    SystemExcludingSelf,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub enum CaptureState {
    Stopped,
    Starting(CaptureTarget),
    Capturing(CaptureTarget),
    Unavailable { endpoint_id: Option<String> },
    Blocked { reason: String },
    Recoverable { reason: String },
}

/// Control-thread policy. A returned target must be activated before `started` is called.
pub struct CaptureController {
    config: CaptureConfig,
    state: CaptureState,
}

impl CaptureController {
    pub fn new(config: CaptureConfig) -> Self {
        Self {
            config,
            state: CaptureState::Stopped,
        }
    }
    pub fn state(&self) -> &CaptureState {
        &self.state
    }
    pub fn config(&self) -> &CaptureConfig {
        &self.config
    }
    pub fn set_outputs(&mut self, ids: Vec<String>) {
        self.config.output_endpoint_ids = ids;
    }

    pub fn reconcile(&mut self, endpoints: &[AudioEndpoint]) -> Option<CaptureTarget> {
        let target = match &self.config.source {
            CaptureSource::SystemExcludingSelf => CaptureTarget::SystemExcludingSelf,
            source => {
                let endpoint = endpoints.iter().find(|endpoint| {
                    endpoint.state == EndpointState::Active
                        && match source {
                            CaptureSource::DefaultRender => endpoint.is_default,
                            CaptureSource::Endpoint(id) => endpoint.id == *id,
                            CaptureSource::SystemExcludingSelf => false,
                        }
                });
                let Some(endpoint) = endpoint else {
                    self.state = CaptureState::Unavailable {
                        endpoint_id: match source {
                            CaptureSource::Endpoint(id) => Some(id.clone()),
                            _ => None,
                        },
                    };
                    return None;
                };
                if self.config.output_endpoint_ids.contains(&endpoint.id) {
                    self.state = CaptureState::Blocked {
                        reason: EXCLUSION_SCOPE_REASON.into(),
                    };
                    return None;
                }
                CaptureTarget::Endpoint(endpoint.id.clone())
            }
        };
        if matches!(&self.state, CaptureState::Capturing(current) | CaptureState::Starting(current) if *current == target)
        {
            return None;
        }
        self.state = CaptureState::Starting(target.clone());
        Some(target)
    }

    pub fn started(&mut self) {
        if let CaptureState::Starting(target) = &self.state {
            self.state = CaptureState::Capturing(target.clone());
        }
    }
    pub fn failed(&mut self, reason: impl Into<String>) {
        self.state = CaptureState::Recoverable {
            reason: reason.into(),
        };
    }
    pub fn stop(&mut self) {
        self.state = CaptureState::Stopped;
    }
}

/// Borrowed WASAPI packet. QPC is already converted by GetBuffer to 100 ns units.
/// Data is interleaved IEEE float32 little-endian; shared-mode conversion is negotiated at open.
#[derive(Default)]
pub struct Packet<'a> {
    pub data: &'a [u8],
    pub frames: usize,
    pub device_position: u64,
    pub qpc_100ns: u64,
    pub silent: bool,
    pub discontinuity: bool,
    pub timestamp_error: bool,
}

/// Preallocated audio path. It never constructs a heap-owning PcmBlock: Task 2's
/// producer copies borrowed samples into the same timestamped block slots.
pub struct PacketPublisher {
    producer: PcmProducer,
    format: PcmFormat,
    scratch: Vec<f32>,
    max_frames: usize,
    timeline: MasterTimeline,
    next: Option<(u64, u64)>,
}

impl PacketPublisher {
    pub fn new(config: &CaptureConfig) -> Result<(Self, PcmConsumer), QueueStatus> {
        // Bound control-thread allocations and Win32 format arithmetic, too.
        if config.format.channels == 0
            || config.format.channels > 32
            || !(8_000..=384_000).contains(&config.format.sample_rate)
            || !(1..=1024).contains(&config.queue_blocks)
            || !(1..=65_536).contains(&config.max_block_frames)
            || config
                .queue_blocks
                .saturating_mul(config.max_block_frames)
                .saturating_mul(config.format.channels as usize)
                > 16_777_216
        {
            return Err(QueueStatus::InvalidFormat);
        }
        let (producer, consumer) =
            BoundedPcmQueue::new(config.format, config.queue_blocks, config.max_block_frames)?
                .into_spsc();
        Ok((
            Self {
                producer,
                format: config.format,
                scratch: vec![0.0; config.max_block_frames * config.format.channels as usize],
                max_frames: config.max_block_frames,
                timeline: MasterTimeline::new(config.format.sample_rate, 10_000_000, 0, 0).unwrap(),
                next: None,
            },
            consumer,
        ))
    }

    pub fn reset(&mut self) {
        self.producer.reset();
        self.next = None;
    }

    pub fn publish(&mut self, packet: Packet<'_>) -> QueueStatus {
        let channels = self.format.channels as usize;
        let bytes = packet
            .frames
            .checked_mul(channels)
            .and_then(|n| n.checked_mul(4));
        if packet.timestamp_error
            || packet.frames == 0
            || bytes.is_none()
            || (!packet.silent && bytes != Some(packet.data.len()))
        {
            self.reset();
            return QueueStatus::InvalidFormat;
        }
        if packet.discontinuity
            || self
                .next
                .is_some_and(|(position, _)| position != packet.device_position)
        {
            self.reset();
        }
        let first = self
            .next
            .map(|(_, frame)| frame)
            .unwrap_or_else(|| self.timeline.frame_at(packet.qpc_100ns));
        let Some(next_frame) = first.checked_add(packet.frames as u64) else {
            self.reset();
            return QueueStatus::InvalidFormat;
        };
        let Some(next_position) = packet.device_position.checked_add(packet.frames as u64) else {
            self.reset();
            return QueueStatus::InvalidFormat;
        };
        for offset in (0..packet.frames).step_by(self.max_frames) {
            let frames = self.max_frames.min(packet.frames - offset);
            let samples = &mut self.scratch[..frames * channels];
            if packet.silent {
                samples.fill(0.0);
            } else {
                let start = offset * channels * 4;
                for (sample, bytes) in samples
                    .iter_mut()
                    .zip(packet.data[start..start + frames * channels * 4].chunks_exact(4))
                {
                    let value = f32::from_le_bytes(bytes.try_into().unwrap());
                    *sample = if value.is_finite() { value } else { 0.0 };
                }
            }
            match self.producer.push_samples(first + offset as u64, samples) {
                QueueStatus::Ok | QueueStatus::Discontinuity => {}
                status => {
                    self.reset();
                    return status;
                }
            }
        }
        self.next = Some((next_position, next_frame));
        QueueStatus::Ok
    }
}

#[cfg(windows)]
pub use crate::platform::windows::LoopbackCapture;

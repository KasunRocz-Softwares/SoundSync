//! Engine control thread. Owns the thread-affine loopback capture and endpoint
//! catalog, fans captured PCM into one bounded queue per output, supervises render
//! workers, and publishes throttled snapshots. Commands arrive over a channel; the
//! handle is Send + Sync so Tauri can share it.
use super::windows::{CaptureError, EndpointCatalog, LoopbackCapture};
use super::windows_apps::list_audio_apps;
use super::windows_mic::{list_microphones, MicRecorder, Microphone};
use super::windows_render::RenderWorker;
use crate::calibration::{
    analyze_output, click_sample, recorded_silence, silent_mic_message, soloed_output,
    solve_delays, solve_levels, total_frames, CalibratedOutput,
};
use crate::capture::AudioApp;
use crate::capture::{AudioEndpoint, CaptureConfig, CaptureSource, CaptureState, EndpointState};
use crate::enhance::{self, AppMode, ListeningMode, SoundSettings};
use crate::routing::OutputSpectrum;
use crate::eq::{design, EqSettings};
use crate::mixer::TimelineMixer;
use crate::net::server::{NetworkServer, ServerEvent};
use crate::net::worker::NetworkWorker;
use crate::net::{
    endpoint_id, key_of, merge_network_endpoints, qr_svg, NetworkDevice, NetworkStatus,
    DEFAULT_PORT,
};
use crate::output_state::{OutputLifecycle, OutputLifecycleState};
use crate::placement::{
    apply_preset, compute_placement, SoundPreset, SpeakerPosition, MIN_TRIM_DB,
};
use crate::render::{
    ms_to_frames, AudioMode, ChannelRole, OutputControls, OutputMetrics, RenderPipeline,
    WorkerPoll, MAX_ENABLED_OUTPUTS,
};
use crate::room;
use crate::routing::AppSourceStatus;
use crate::routing::{
    alignment_frames, latency_warning, metrics_ms, EngineSnapshot, ErrorRecord, OutputConfig,
    OutputStatus,
};
use crate::routing::{CalibrationProgress, CalibrationReport, RoomOutput, RoomReport};
use crate::spatial::{
    nearest_position, position_azimuth, wrap_deg, SpatialConfig, SpatialShared, StereoSpread,
    MAX_POINTS,
};
use crate::{BoundedPcmQueue, PcmConsumer, PcmFormat, PcmProducer, QueueStatus};
use std::collections::{BTreeMap, VecDeque};
use std::sync::atomic::Ordering;
use std::sync::mpsc::{self, Receiver, Sender, SyncSender};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

const TICK: Duration = Duration::from_millis(2);
const SNAPSHOT_INTERVAL: Duration = Duration::from_millis(100);
const ALIGN_INTERVAL: Duration = Duration::from_millis(500);
const OUTPUT_QUEUE_BLOCKS: usize = 64;
const MAX_ERRORS: usize = 50;

#[derive(Clone, Debug, thiserror::Error, serde::Serialize)]
pub enum EngineError {
    #[error("Unknown or unavailable endpoint: {0}")]
    UnknownEndpoint(String),
    #[error("At most {MAX_ENABLED_OUTPUTS} outputs can be enabled")]
    TooManyOutputs,
    #[error("Audio engine thread is not running")]
    Disconnected,
    #[error("{0}")]
    Platform(String),
}

type Reply<T> = SyncSender<Result<T, EngineError>>;

enum Command {
    Start(Reply<()>),
    Stop(Reply<()>),
    SetSource(CaptureSource, Reply<()>),
    SetMode(AudioMode, Reply<()>),
    Enable(String, Reply<()>),
    Disable(String, Reply<()>),
    SetGain(String, f32, Reply<()>),
    SetMute(String, bool, Reply<()>),
    SetDelay(String, u32, Reply<()>),
    ApplyOutputs(Vec<OutputConfig>, Reply<()>),
    SetPlacement(String, SpeakerPosition, Option<f32>, Reply<()>),
    SetRole(String, ChannelRole, Reply<()>),
    SetTrim(String, f32, Reply<()>),
    SetCompensation(bool, Reply<()>),
    SetMapPlacement(String, f32, Option<f32>, Reply<()>),
    SetSpread(String, StereoSpread, Reply<()>),
    SetSubwoofer(String, bool, Reply<()>),
    SetSpatial(SpatialConfig, Reply<()>),
    SetEq(String, EqSettings, Reply<()>),
    SetSound(SoundSettings, Reply<()>),
    ApplyListeningMode(ListeningMode, Reply<()>),
    SetAppModes(Vec<AppMode>, Reply<()>),
    Spectrum(Reply<Vec<OutputSpectrum>>),
    SetFormat(PcmFormat, Reply<()>),
    SetOutputAdvanced(String, crate::routing::OutputAdvanced, Reply<()>),
    ListApps(Reply<Vec<AudioApp>>),
    ListMics(Reply<Vec<Microphone>>),
    Calibrate(String, Reply<CalibrationReport>),
    MeasureRoom(String, Reply<RoomReport>),
    ApplyPreset(SoundPreset, Reply<()>),
    Refresh(Reply<Vec<AudioEndpoint>>),
    SetNetwork(bool, Vec<NetworkDevice>, Reply<NetworkStatus>),
    AllowNetworkDevice(String, Reply<()>),
    DenyNetworkDevice(String, Reply<()>),
    RenameNetworkDevice(String, String, Reply<()>),
    ForgetNetworkDevice(String, Reply<()>),
    TakeNetworkJoins(Reply<Vec<NetworkDevice>>),
    Shutdown,
}

/// Thread-safe handle to the engine control thread.
pub struct AudioEngine {
    commands: Mutex<Sender<Command>>,
    snapshot: Arc<Mutex<EngineSnapshot>>,
    thread: Mutex<Option<JoinHandle<()>>>,
}

impl AudioEngine {
    pub fn spawn() -> Self {
        let format = PcmFormat {
            sample_rate: 48_000,
            channels: 2,
        };
        let snapshot = Arc::new(Mutex::new(EngineSnapshot::idle(format)));
        let (sender, receiver) = mpsc::channel();
        let shared = snapshot.clone();
        let thread = std::thread::Builder::new()
            .name("syncsound-engine".into())
            .spawn(move || match Core::new(format, shared.clone()) {
                Ok(mut core) => core.run(receiver),
                Err(error) => {
                    if let Ok(mut snapshot) = shared.lock() {
                        snapshot.recent_errors.push(ErrorRecord {
                            at_ms: 0,
                            scope: "engine".into(),
                            message: format!("Audio engine failed to initialise: {error}"),
                        });
                    }
                }
            })
            .ok();
        Self {
            commands: Mutex::new(sender),
            snapshot,
            thread: Mutex::new(thread),
        }
    }

    fn call<T>(&self, make: impl FnOnce(Reply<T>) -> Command) -> Result<T, EngineError> {
        self.call_timeout(make, Duration::from_secs(10))
    }

    fn call_timeout<T>(
        &self,
        make: impl FnOnce(Reply<T>) -> Command,
        timeout: Duration,
    ) -> Result<T, EngineError> {
        let (reply, result) = mpsc::sync_channel(1);
        self.commands
            .lock()
            .map_err(|_| EngineError::Disconnected)?
            .send(make(reply))
            .map_err(|_| EngineError::Disconnected)?;
        result
            .recv_timeout(timeout)
            .map_err(|_| EngineError::Disconnected)?
    }

    /// Latest throttled snapshot (at most ~100 ms old while the engine runs).
    pub fn snapshot(&self) -> EngineSnapshot {
        self.snapshot
            .lock()
            .map(|s| s.clone())
            .unwrap_or_else(|p| p.into_inner().clone())
    }
    pub fn refresh_endpoints(&self) -> Result<Vec<AudioEndpoint>, EngineError> {
        self.call(Command::Refresh)
    }
    pub fn start(&self) -> Result<(), EngineError> {
        self.call(Command::Start)
    }
    pub fn stop(&self) -> Result<(), EngineError> {
        self.call(Command::Stop)
    }
    pub fn set_source(&self, source: CaptureSource) -> Result<(), EngineError> {
        self.call(|r| Command::SetSource(source, r))
    }
    pub fn set_mode(&self, mode: AudioMode) -> Result<(), EngineError> {
        self.call(|r| Command::SetMode(mode, r))
    }
    pub fn enable(&self, id: String) -> Result<(), EngineError> {
        self.call(|r| Command::Enable(id, r))
    }
    pub fn disable(&self, id: String) -> Result<(), EngineError> {
        self.call(|r| Command::Disable(id, r))
    }
    pub fn set_gain(&self, id: String, gain: f32) -> Result<(), EngineError> {
        self.call(|r| Command::SetGain(id, gain, r))
    }
    pub fn set_mute(&self, id: String, muted: bool) -> Result<(), EngineError> {
        self.call(|r| Command::SetMute(id, muted, r))
    }
    pub fn set_delay(&self, id: String, delay_ms: u32) -> Result<(), EngineError> {
        self.call(|r| Command::SetDelay(id, delay_ms, r))
    }
    /// Replace the whole output set (profile load). Validated by the caller.
    pub fn apply_outputs(&self, outputs: Vec<OutputConfig>) -> Result<(), EngineError> {
        self.call(|r| Command::ApplyOutputs(outputs, r))
    }
    /// Position and distance from the listening seat (validated by the caller).
    pub fn set_placement(
        &self,
        id: String,
        position: SpeakerPosition,
        distance_m: Option<f32>,
    ) -> Result<(), EngineError> {
        self.call(|r| Command::SetPlacement(id, position, distance_m, r))
    }
    pub fn set_role(&self, id: String, role: ChannelRole) -> Result<(), EngineError> {
        self.call(|r| Command::SetRole(id, role, r))
    }
    pub fn set_trim(&self, id: String, trim_db: f32) -> Result<(), EngineError> {
        self.call(|r| Command::SetTrim(id, trim_db, r))
    }
    /// Room-map drag: exact angle and distance; the named position snaps to the nearest one.
    pub fn set_map_placement(
        &self,
        id: String,
        azimuth: f32,
        distance_m: Option<f32>,
    ) -> Result<(), EngineError> {
        self.call(|r| Command::SetMapPlacement(id, azimuth, distance_m, r))
    }
    pub fn set_spread(&self, id: String, spread: StereoSpread) -> Result<(), EngineError> {
        self.call(|r| Command::SetSpread(id, spread, r))
    }
    pub fn set_subwoofer(&self, id: String, on: bool) -> Result<(), EngineError> {
        self.call(|r| Command::SetSubwoofer(id, on, r))
    }
    pub fn list_microphones(&self) -> Result<Vec<Microphone>, EngineError> {
        self.call(Command::ListMics)
    }
    /// Plays a click track through each enabled output in turn, records it with
    /// `mic_id` and applies per-output delays. Blocks for about 6 s per output.
    pub fn calibrate(&self, mic_id: String) -> Result<CalibrationReport, EngineError> {
        self.call_timeout(|r| Command::Calibrate(mic_id, r), Duration::from_secs(90))
    }
    /// Plays a frequency sweep through each enabled output in turn, records it
    /// with `mic_id` and applies a room-correction EQ. About 5.5 s per output.
    pub fn measure_room(&self, mic_id: String) -> Result<RoomReport, EngineError> {
        self.call_timeout(|r| Command::MeasureRoom(mic_id, r), Duration::from_secs(90))
    }
    /// Applications that currently have an audio session.
    pub fn list_apps(&self) -> Result<Vec<AudioApp>, EngineError> {
        self.call(Command::ListApps)
    }
    pub fn set_eq(&self, id: String, settings: EqSettings) -> Result<(), EngineError> {
        self.call(|r| Command::SetEq(id, settings, r))
    }
    pub fn set_spatial(&self, config: SpatialConfig) -> Result<(), EngineError> {
        self.call(|r| Command::SetSpatial(config, r))
    }
    /// Tilt, loudness, crossover, normalize, night mode and level matching.
    pub fn set_sound(&self, settings: SoundSettings) -> Result<(), EngineError> {
        self.call(|r| Command::SetSound(settings, r))
    }
    /// Sets the tone controls to a listening mode's values.
    pub fn apply_listening_mode(&self, mode: ListeningMode) -> Result<(), EngineError> {
        self.call(|r| Command::ApplyListeningMode(mode, r))
    }
    /// Apps that switch the listening mode while they play.
    pub fn set_app_modes(&self, modes: Vec<AppMode>) -> Result<(), EngineError> {
        self.call(|r| Command::SetAppModes(modes, r))
    }
    /// Octave-band levels of what each enabled output plays. Metering runs
    /// only while this is called (stops ~2 s after the last call).
    pub fn spectrum(&self) -> Result<Vec<OutputSpectrum>, EngineError> {
        self.call(Command::Spectrum)
    }
    /// Capture sample rate and channel count (validated by the caller).
    /// Restarts routing if it is running.
    pub fn set_format(&self, format: PcmFormat) -> Result<(), EngineError> {
        self.call_timeout(|r| Command::SetFormat(format, r), Duration::from_secs(20))
    }
    /// Source channels, impulse response and exclusive mode for one output.
    pub fn set_output_advanced(
        &self,
        id: String,
        advanced: crate::routing::OutputAdvanced,
    ) -> Result<(), EngineError> {
        self.call_timeout(
            |r| Command::SetOutputAdvanced(id, advanced, r),
            Duration::from_secs(20),
        )
    }
    pub fn set_distance_compensation(&self, enabled: bool) -> Result<(), EngineError> {
        self.call(|r| Command::SetCompensation(enabled, r))
    }
    pub fn apply_preset(&self, preset: SoundPreset) -> Result<(), EngineError> {
        self.call(|r| Command::ApplyPreset(preset, r))
    }
    /// Turns the network speaker server on or off. `allowed` are the devices
    /// saved by the app; while the server is already on, only their names
    /// are updated (the server keeps running).
    pub fn set_network(
        &self,
        enabled: bool,
        allowed: Vec<NetworkDevice>,
    ) -> Result<NetworkStatus, EngineError> {
        self.call(|r| Command::SetNetwork(enabled, allowed, r))
    }
    /// Lets a device waiting to join start playing.
    pub fn allow_network_device(&self, key: String) -> Result<(), EngineError> {
        self.call(|r| Command::AllowNetworkDevice(key, r))
    }
    /// Refuses a device waiting to join.
    pub fn deny_network_device(&self, key: String) -> Result<(), EngineError> {
        self.call(|r| Command::DenyNetworkDevice(key, r))
    }
    /// Renames an allowed network device (1 to 40 characters).
    pub fn rename_network_device(&self, key: String, name: String) -> Result<(), EngineError> {
        self.call(|r| Command::RenameNetworkDevice(key, name, r))
    }
    /// Forgets an allowed network device: it is disconnected, its output is
    /// removed, and it has to be allowed again to rejoin.
    pub fn forget_network_device(&self, key: String) -> Result<(), EngineError> {
        self.call(|r| Command::ForgetNetworkDevice(key, r))
    }
    /// Devices newly allowed (by QR code or Allow) since the last call, for
    /// the app to save. Each device is returned once.
    pub fn take_network_joins(&self) -> Vec<NetworkDevice> {
        self.call(Command::TakeNetworkJoins).unwrap_or_default()
    }
}

impl Drop for AudioEngine {
    fn drop(&mut self) {
        if let Ok(sender) = self.commands.lock() {
            let _ = sender.send(Command::Shutdown);
        }
        if let Some(thread) = self.thread.lock().ok().and_then(|mut t| t.take()) {
            let _ = thread.join();
        }
    }
}

/// The thread playing one output: a WASAPI render stream or a network speaker.
enum OutputWorker {
    Wasapi(RenderWorker),
    Network(NetworkWorker),
}

impl OutputWorker {
    fn poll(&self) -> WorkerPoll {
        match self {
            OutputWorker::Wasapi(worker) => worker.poll(),
            OutputWorker::Network(worker) => worker.poll(),
        }
    }
}

/// Network speaker state owned by the control thread.
#[derive(Default)]
struct NetState {
    server: Option<NetworkServer>,
    /// Allowed devices (key and display name); the source of names.
    devices: Vec<NetworkDevice>,
    /// Newly allowed devices not yet handed to the app.
    joins: Vec<NetworkDevice>,
    /// Last QR code: the URL it encodes and its SVG.
    qr: Option<(String, String)>,
    error: Option<String>,
}

impl NetState {
    fn connected(&self, key: &str) -> bool {
        self.server
            .as_ref()
            .and_then(|s| s.link(key))
            .is_some_and(|link| link.connected())
    }

    /// Records a newly allowed device once; known devices keep their name.
    fn learn(&mut self, key: &str, name: &str) -> bool {
        if self.devices.iter().any(|d| d.key == key) {
            return false;
        }
        let device = NetworkDevice {
            key: key.into(),
            name: name.into(),
        };
        self.devices.push(device.clone());
        self.joins.push(device);
        true
    }

    /// Status for the snapshot; regenerates the QR code only when its URL changed.
    fn status(&mut self) -> NetworkStatus {
        let Some(server) = &self.server else {
            self.qr = None;
            return NetworkStatus {
                error: self.error.clone(),
                ..NetworkStatus::default()
            };
        };
        let qr_url = server.qr_url();
        if self.qr.as_ref().map(|(url, _)| url) != Some(&qr_url) {
            self.qr = qr_svg(&qr_url).map(|svg| (qr_url, svg));
        }
        NetworkStatus {
            enabled: true,
            url: Some(server.url()),
            qr_svg: self.qr.as_ref().map(|(_, svg)| svg.clone()),
            pending: server.pending(),
            error: self.error.clone(),
        }
    }
}

struct OutputSlot {
    config: OutputConfig,
    controls: Arc<OutputControls>,
    metrics: Arc<OutputMetrics>,
    lifecycle: OutputLifecycle,
    producer: Option<PcmProducer>,
    worker: Option<OutputWorker>,
    published_eq: Option<EqSettings>,
    published_enhance: Option<(EqSettings, SoundSettings)>,
    /// Last impulse-response load: taps, or the error to show.
    convolution: Option<Result<usize, String>>,
}

struct CalibrationRun {
    order: Vec<String>,
    names: Vec<String>,
    recorder: MicRecorder,
    mic_name: String,
    started: Instant,
    generated: u64,
    total: u64,
    frame_base: u64,
    reply: RunReply,
}

/// What a microphone run measures, and where its result goes.
enum RunReply {
    /// Click track: per-output arrival time → delays.
    Delay(Reply<CalibrationReport>),
    /// Log sweep: per-output frequency response → room-correction EQ.
    Room(Reply<RoomReport>),
}

impl RunReply {
    fn fail(self, error: EngineError) {
        match self {
            RunReply::Delay(r) => {
                let _ = r.send(Err(error));
            }
            RunReply::Room(r) => {
                let _ = r.send(Err(error));
            }
        }
    }
    fn is_room(&self) -> bool {
        matches!(self, RunReply::Room(_))
    }
}

struct AppCapture {
    exe: String,
    pid: u32,
    capture: LoopbackCapture,
    consumer: PcmConsumer,
}

struct Core {
    format: PcmFormat,
    mode: AudioMode,
    source: CaptureSource,
    distance_compensation: bool,
    spatial_config: SpatialConfig,
    spatial: Arc<SpatialShared>,
    sound: SoundSettings,
    app_modes: Vec<AppMode>,
    /// The app (and its mode) currently overriding the tone, if any.
    auto_mode: Option<AppMode>,
    last_mode_check: Instant,
    last_spectrum: Option<Instant>,
    running: bool,
    endpoints: Vec<AudioEndpoint>,
    outputs: BTreeMap<String, OutputSlot>,
    capture: Option<(LoopbackCapture, PcmConsumer)>,
    /// Per-app captures (source = Apps): exe, resolved pid, capture, queue.
    app_captures: Vec<AppCapture>,
    unresolved_apps: Vec<String>,
    mixer: TimelineMixer,
    mix_out: Vec<f32>,
    last_app_check: Instant,
    calibration: Option<CalibrationRun>,
    click_block: Vec<f32>,
    silent_block: Vec<f32>,
    scratch: Vec<f32>,
    errors: VecDeque<ErrorRecord>,
    started: Instant,
    last_snapshot: Instant,
    last_align: Instant,
    shared: Arc<Mutex<EngineSnapshot>>,
    net: NetState,
    // Declared last: dropped after captures/workers so its COM apartment outlives them.
    catalog: EndpointCatalog,
}

impl Core {
    fn new(format: PcmFormat, shared: Arc<Mutex<EngineSnapshot>>) -> Result<Self, CaptureError> {
        let catalog = EndpointCatalog::new()?;
        let endpoints = catalog.enumerate()?;
        catalog.changed.store(false, Ordering::Release);
        let now = Instant::now();
        Ok(Self {
            format,
            mode: AudioMode::default(),
            source: CaptureSource::default(),
            distance_compensation: true,
            spatial_config: SpatialConfig::default(),
            spatial: Arc::new(SpatialShared::default()),
            sound: SoundSettings::default(),
            app_modes: Vec::new(),
            auto_mode: None,
            last_mode_check: now,
            last_spectrum: None,
            running: false,
            endpoints,
            outputs: BTreeMap::new(),
            capture: None,
            app_captures: Vec::new(),
            unresolved_apps: Vec::new(),
            mixer: TimelineMixer::new(usize::from(format.channels), 24_000, 960),
            mix_out: vec![0.0; 480 * usize::from(format.channels)],
            last_app_check: now,
            calibration: None,
            click_block: vec![0.0; 480 * usize::from(format.channels)],
            silent_block: vec![0.0; 480 * usize::from(format.channels)],
            scratch: vec![
                0.0;
                CaptureConfig::default().max_block_frames * usize::from(format.channels)
            ],
            errors: VecDeque::new(),
            started: now,
            last_snapshot: now - SNAPSHOT_INTERVAL,
            last_align: now,
            shared,
            net: NetState::default(),
            catalog,
        })
    }

    fn record(&mut self, scope: &str, message: impl Into<String>) {
        if self.errors.len() == MAX_ERRORS {
            self.errors.pop_front();
        }
        self.errors.push_back(ErrorRecord {
            at_ms: self.started.elapsed().as_millis() as u64,
            scope: scope.into(),
            message: message.into(),
        });
    }

    fn run(&mut self, commands: Receiver<Command>) {
        loop {
            match commands.recv_timeout(TICK) {
                Ok(Command::Shutdown) => break,
                Ok(command) => {
                    self.handle(command);
                    self.recompute_placement();
                    self.publish(true);
                }
                Err(mpsc::RecvTimeoutError::Timeout) => {}
                Err(mpsc::RecvTimeoutError::Disconnected) => break,
            }
            self.tick();
        }
        self.stop_all();
    }

    fn endpoint_active(&self, id: &str) -> bool {
        self.endpoints
            .iter()
            .any(|e| e.id == id && e.state == EndpointState::Active)
    }

    fn enabled_ids(&self) -> Vec<String> {
        self.outputs
            .values()
            .filter(|s| s.config.enabled)
            .map(|s| s.config.endpoint_id.clone())
            .collect()
    }

    /// Enabled outputs that are local render endpoints. Network speakers stay
    /// out of the feedback guard: loopback capture never hears them.
    fn render_ids(&self) -> Vec<String> {
        self.enabled_ids()
            .into_iter()
            .filter(|id| key_of(id).is_none())
            .collect()
    }

    fn slot(&mut self, id: &str) -> Result<&mut OutputSlot, EngineError> {
        if !self.outputs.contains_key(id) {
            if !self.endpoints.iter().any(|e| e.id == id) {
                return Err(EngineError::UnknownEndpoint(id.into()));
            }
            let policy = self.mode.policy();
            self.outputs.insert(
                id.into(),
                OutputSlot {
                    config: OutputConfig::new(id),
                    controls: Arc::new(OutputControls::new(
                        1.0,
                        false,
                        0,
                        policy,
                        self.format.sample_rate,
                    )),
                    metrics: Arc::new(OutputMetrics::default()),
                    lifecycle: OutputLifecycle::default(),
                    producer: None,
                    worker: None,
                    published_eq: None,
                    published_enhance: None,
                    convolution: None,
                },
            );
        }
        Ok(self.outputs.get_mut(id).unwrap())
    }

    fn handle(&mut self, command: Command) {
        match command {
            Command::Start(reply) => {
                let _ = reply.send(self.start());
            }
            Command::Stop(reply) => {
                self.stop_all();
                let _ = reply.send(Ok(()));
            }
            Command::SetSource(source, reply) => {
                if let CaptureSource::Endpoint(id) = &source {
                    // A network speaker only plays; there is nothing to capture from it.
                    if key_of(id).is_some() || !self.endpoints.iter().any(|e| &e.id == id) {
                        let _ = reply.send(Err(EngineError::UnknownEndpoint(id.clone())));
                        return;
                    }
                }
                self.source = source;
                if self.running {
                    self.restart_capture();
                }
                let _ = reply.send(Ok(()));
            }
            Command::SetMode(mode, reply) => {
                self.mode = mode;
                let policy = mode.policy();
                let rate = self.format.sample_rate;
                for slot in self.outputs.values() {
                    slot.controls.apply_policy(policy, rate);
                    if !policy.align_outputs {
                        slot.controls.set_align_frames(0);
                    }
                }
                // Render buffer size is fixed at Initialize: reopen running outputs.
                if self.running {
                    let ids = self.enabled_ids();
                    for id in ids {
                        self.stop_output(&id);
                        self.start_output(&id);
                    }
                }
                let _ = reply.send(Ok(()));
            }
            Command::Enable(id, reply) => {
                let _ = reply.send(self.enable(&id));
            }
            Command::Disable(id, reply) => {
                if let Some(slot) = self.outputs.get_mut(&id) {
                    slot.config.enabled = false;
                }
                self.stop_output(&id);
                self.sync_capture_routes();
                let _ = reply.send(Ok(()));
            }
            Command::SetGain(id, gain, reply) => {
                let _ = reply.send(self.slot(&id).map(|s| {
                    s.config.gain = gain.clamp(0.0, 1.0);
                    s.controls.set_gain(s.config.gain);
                }));
            }
            Command::SetMute(id, muted, reply) => {
                let _ = reply.send(self.slot(&id).map(|s| {
                    s.config.muted = muted;
                    s.controls.set_muted(muted);
                }));
            }
            Command::SetDelay(id, delay_ms, reply) => {
                let rate = self.format.sample_rate;
                let _ = reply.send(self.slot(&id).map(|s| {
                    s.config.delay_ms = delay_ms;
                    s.controls.set_delay_frames(ms_to_frames(delay_ms, rate));
                }));
            }
            Command::ApplyOutputs(configs, reply) => {
                let _ = reply.send(self.apply_outputs(configs));
            }
            Command::SetPlacement(id, position, distance_m, reply) => {
                let _ = reply.send(self.slot(&id).map(|s| {
                    s.config.position = position;
                    s.config.distance_m = distance_m;
                }));
            }
            Command::SetRole(id, role, reply) => {
                let _ = reply.send(self.slot(&id).map(|s| s.config.role = role));
            }
            Command::SetTrim(id, trim_db, reply) => {
                let _ = reply.send(self.slot(&id).map(|s| s.config.trim_db = trim_db));
            }
            Command::SetMapPlacement(id, azimuth, distance_m, reply) => {
                let _ = reply.send(self.slot(&id).map(|s| {
                    s.config.azimuth_deg = Some(azimuth);
                    s.config.position = nearest_position(azimuth);
                    s.config.distance_m = distance_m;
                }));
            }
            Command::SetSpread(id, spread, reply) => {
                let _ = reply.send(self.slot(&id).map(|s| s.config.spread = spread));
            }
            Command::SetSubwoofer(id, on, reply) => {
                let _ = reply.send(self.slot(&id).map(|s| s.config.subwoofer = on));
            }
            Command::ListMics(reply) => {
                let _ = reply
                    .send(list_microphones().map_err(|e| EngineError::Platform(e.to_string())));
            }
            Command::Calibrate(mic_id, reply) => {
                if let Err(error) = self.start_calibration(mic_id, RunReply::Delay(reply.clone())) {
                    let _ = reply.send(Err(error));
                }
            }
            Command::MeasureRoom(mic_id, reply) => {
                if let Err(error) = self.start_calibration(mic_id, RunReply::Room(reply.clone())) {
                    let _ = reply.send(Err(error));
                }
            }
            Command::ListApps(reply) => {
                let _ =
                    reply.send(list_audio_apps().map_err(|e| EngineError::Platform(e.to_string())));
            }
            Command::SetEq(id, settings, reply) => {
                let _ = reply.send(self.slot(&id).map(|s| s.config.eq = settings));
            }
            Command::SetSpatial(config, reply) => {
                self.spatial_config = config;
                let _ = reply.send(Ok(()));
            }
            Command::SetSound(settings, reply) => {
                if settings.room_target != self.sound.room_target {
                    // Re-aim stored room measurements at the new target.
                    for slot in self.outputs.values_mut() {
                        let eq = &mut slot.config.eq;
                        if eq.room_response.iter().any(Option::is_some) {
                            eq.correction_db = room::correction(&eq.room_response, settings.room_target);
                        }
                    }
                }
                self.sound = settings;
                let _ = reply.send(Ok(()));
            }
            Command::SetFormat(format, reply) => {
                let _ = reply.send(self.set_format(format));
            }
            Command::SetOutputAdvanced(id, advanced, reply) => {
                let running = self.running;
                let network = key_of(&id).is_some();
                let result = self.slot(&id).map(|slot| {
                    // Exclusive mode means nothing for a network speaker.
                    let reopen = !network && slot.config.exclusive != advanced.exclusive;
                    let reload = slot.config.convolution != advanced.convolution;
                    slot.config.source_channels = advanced.source_channels;
                    slot.config.exclusive = advanced.exclusive;
                    slot.config.convolution = advanced.convolution;
                    (reopen && running && slot.config.enabled, reload)
                });
                if let Ok((reopen, reload)) = result {
                    if reopen {
                        // Exclusive vs shared is fixed when the device opens.
                        self.stop_output(&id);
                        self.start_output(&id);
                    } else if reload {
                        self.load_convolver(&id);
                    }
                }
                let _ = reply.send(result.map(|_| ()));
            }
            Command::ApplyListeningMode(mode, reply) => {
                self.sound = mode.apply(self.sound);
                let _ = reply.send(Ok(()));
            }
            Command::SetAppModes(modes, reply) => {
                self.app_modes = modes;
                self.last_mode_check = Instant::now() - Duration::from_secs(10);
                self.service_app_modes();
                let _ = reply.send(Ok(()));
            }
            Command::Spectrum(reply) => {
                self.last_spectrum = Some(Instant::now());
                let spectra = self
                    .outputs
                    .values()
                    .filter(|s| s.config.enabled)
                    .map(|s| {
                        s.controls.spectrum.watching.store(true, Ordering::Relaxed);
                        OutputSpectrum {
                            endpoint_id: s.config.endpoint_id.clone(),
                            levels_db: s.controls.spectrum.levels_db(),
                            levels: s.controls.levels.take(),
                        }
                    })
                    .collect();
                let _ = reply.send(Ok(spectra));
            }
            Command::SetCompensation(enabled, reply) => {
                self.distance_compensation = enabled;
                let _ = reply.send(Ok(()));
            }
            Command::ApplyPreset(preset, reply) => {
                let mut configs: Vec<OutputConfig> =
                    self.outputs.values().map(|s| s.config.clone()).collect();
                let result = apply_preset(preset, &mut configs)
                    .map(|()| {
                        for config in configs {
                            if let Some(slot) = self.outputs.get_mut(&config.endpoint_id) {
                                slot.config = config;
                            }
                        }
                    })
                    .map_err(|e| EngineError::Platform(e.message));
                let _ = reply.send(result);
            }
            Command::Refresh(reply) => {
                self.refresh_endpoints();
                let _ = reply.send(Ok(self.endpoints.clone()));
            }
            Command::SetNetwork(enabled, allowed, reply) => {
                self.set_network(enabled, allowed);
                let _ = reply.send(Ok(self.net.status()));
            }
            Command::AllowNetworkDevice(key, reply) => {
                let result = match &self.net.server {
                    Some(server) => {
                        match server.pending().into_iter().find(|p| p.key == key) {
                            Some(p) => {
                                server.allow(&key);
                                Ok(Some(p.name))
                            }
                            // Gone (left, denied or already allowed): say so.
                            None => Err(EngineError::UnknownEndpoint(endpoint_id(&key))),
                        }
                    }
                    None => Err(EngineError::Platform("Network speakers are off".into())),
                };
                // Remember it now so it is saved even if it leaves before playing.
                if let Ok(Some(name)) = &result {
                    if self.net.learn(&key, name) {
                        self.add_network_output(&key);
                    }
                    self.merge_network();
                }
                let _ = reply.send(result.map(|_| ()));
            }
            Command::DenyNetworkDevice(key, reply) => {
                let result = match &self.net.server {
                    Some(server) if server.pending().iter().any(|p| p.key == key) => {
                        server.deny(&key);
                        Ok(())
                    }
                    Some(_) => Err(EngineError::UnknownEndpoint(endpoint_id(&key))),
                    None => Err(EngineError::Platform("Network speakers are off".into())),
                };
                let _ = reply.send(result);
            }
            Command::RenameNetworkDevice(key, name, reply) => {
                let name = name.trim().to_string();
                let result = if name.is_empty() || name.chars().count() > 40 {
                    Err(EngineError::Platform("A name needs 1 to 40 characters".into()))
                } else if let Some(device) = self.net.devices.iter_mut().find(|d| d.key == key) {
                    device.name = name;
                    self.merge_network();
                    Ok(())
                } else {
                    Err(EngineError::UnknownEndpoint(endpoint_id(&key)))
                };
                let _ = reply.send(result);
            }
            Command::ForgetNetworkDevice(key, reply) => {
                let _ = reply.send(self.forget_network_device(&key));
            }
            Command::TakeNetworkJoins(reply) => {
                let _ = reply.send(Ok(std::mem::take(&mut self.net.joins)));
            }
            Command::Shutdown => {}
        }
    }

    /// Switches the capture rate / channel count. Everything rate-dependent is
    /// rebuilt; routing restarts if it was running.
    fn set_format(&mut self, format: PcmFormat) -> Result<(), EngineError> {
        if format == self.format {
            return Ok(());
        }
        if self.calibration.is_some() {
            return Err(EngineError::Platform(
                "Wait for the calibration to finish".into(),
            ));
        }
        let was_running = self.running;
        self.stop_all();
        self.format = format;
        let ch = usize::from(format.channels);
        self.mixer = TimelineMixer::new(ch, 24_000, 960);
        self.mix_out = vec![0.0; 480 * ch];
        self.click_block = vec![0.0; 480 * ch];
        self.silent_block = vec![0.0; 480 * ch];
        self.scratch = vec![0.0; CaptureConfig::default().max_block_frames * ch];
        let policy = self.mode.policy();
        for slot in self.outputs.values_mut() {
            slot.published_eq = None;
            slot.published_enhance = None;
            slot.controls
                .set_delay_frames(ms_to_frames(slot.config.delay_ms, format.sample_rate));
            slot.controls.apply_policy(policy, format.sample_rate);
        }
        self.recompute_placement();
        if was_running {
            self.start()
        } else {
            Ok(())
        }
    }

    /// The user's sound settings with the playing app's listening mode on top.
    fn effective_sound(&self) -> SoundSettings {
        self.auto_mode
            .as_ref()
            .map_or(self.sound, |a| a.mode.apply(self.sound))
    }

    /// Every 3 s: switch to the listening mode of the first listed app that is
    /// playing sound, or back to the user's own settings.
    fn service_app_modes(&mut self) {
        if self.last_mode_check.elapsed() < Duration::from_secs(3) {
            return;
        }
        self.last_mode_check = Instant::now();
        let next = if self.app_modes.is_empty() {
            None
        } else {
            match list_audio_apps() {
                Ok(apps) => self
                    .app_modes
                    .iter()
                    .find(|m| {
                        apps.iter()
                            .any(|a| a.active && a.exe.eq_ignore_ascii_case(&m.exe))
                    })
                    .cloned(),
                Err(_) => self.auto_mode.clone(),
            }
        };
        if next != self.auto_mode {
            self.auto_mode = next;
            self.recompute_placement();
        }
    }

    /// Stops spectrum metering on the render threads once nobody is watching.
    fn service_spectrum(&mut self) {
        if self
            .last_spectrum
            .is_some_and(|t| t.elapsed() > Duration::from_secs(2))
        {
            self.last_spectrum = None;
            for slot in self.outputs.values() {
                slot.controls.spectrum.watching.store(false, Ordering::Relaxed);
            }
        }
    }

    /// Pushes role, trim and distance compensation into every output's atomics.
    fn recompute_placement(&mut self) {
        let configs: Vec<OutputConfig> = self.outputs.values().map(|s| s.config.clone()).collect();
        let placements = compute_placement(
            &configs,
            self.distance_compensation,
            self.format.sample_rate,
        );
        let rate = self.format.sample_rate;
        let sound = self.effective_sound();
        for (slot, placement) in self.outputs.values_mut().zip(placements) {
            slot.controls.set_role(slot.config.role);
            slot.controls
                .set_source_channels(slot.config.source_channels);
            if slot.published_eq != Some(slot.config.eq) {
                slot.controls
                    .eq
                    .publish(&design(&slot.config.eq, rate), slot.config.eq.headroom());
                slot.published_eq = Some(slot.config.eq);
            }
            if slot.published_enhance != Some((slot.config.eq, sound)) {
                let eq = slot.config.eq;
                slot.controls.enhance.publish(&enhance::design(
                    eq.bass_role,
                    eq.virtual_bass,
                    &sound,
                    rate,
                ));
                slot.published_enhance = Some((eq, sound));
            }
            slot.controls.set_trim(placement.level);
            slot.controls
                .set_placement_delay_frames(placement.delay_frames);
        }
        // Spatial field: every enabled, non-subwoofer output gets a slot and two
        // points (its left and right channel) around its map angle.
        let mut points = Vec::with_capacity(MAX_POINTS);
        let mut next = 0;
        for slot in self.outputs.values() {
            let config = &slot.config;
            let azimuth = config
                .azimuth_deg
                .or_else(|| position_azimuth(config.position));
            slot.controls
                .set_subwoofer(config.enabled && config.subwoofer);
            match azimuth {
                Some(az)
                    if config.enabled && !config.subwoofer && points.len() + 2 <= MAX_POINTS =>
                {
                    let half = config.spread.half_angle();
                    points.push(wrap_deg(az - half));
                    points.push(wrap_deg(az + half));
                    slot.controls.set_spatial_slot(Some(next));
                    next += 1;
                }
                _ => slot.controls.set_spatial_slot(None),
            }
        }
        self.spatial.set(self.spatial_config, &points);
    }

    fn enable(&mut self, id: &str) -> Result<(), EngineError> {
        let already = self.outputs.get(id).is_some_and(|s| s.config.enabled);
        if !already && self.enabled_ids().len() >= MAX_ENABLED_OUTPUTS {
            return Err(EngineError::TooManyOutputs);
        }
        let slot = self.slot(id)?;
        slot.config.enabled = true;
        if self.running {
            // Explicit enable also retries a Failed output.
            self.stop_output(id);
            self.sync_capture_routes();
            self.start_output(id);
        }
        Ok(())
    }

    fn apply_outputs(&mut self, configs: Vec<OutputConfig>) -> Result<(), EngineError> {
        if configs.iter().filter(|c| c.enabled).count() > MAX_ENABLED_OUTPUTS {
            return Err(EngineError::TooManyOutputs);
        }
        if let Some(bad) = configs
            .iter()
            .find(|c| !self.endpoints.iter().any(|e| e.id == c.endpoint_id))
        {
            return Err(EngineError::UnknownEndpoint(bad.endpoint_id.clone()));
        }
        let rate = self.format.sample_rate;
        // Same speakers on as now (undo, A/B, re-loading a profile): update the
        // settings in place so nothing restarts and the audio never drops out.
        let mut now_on = self.enabled_ids();
        now_on.sort();
        let mut next_on: Vec<String> = configs
            .iter()
            .filter(|c| c.enabled)
            .map(|c| c.endpoint_id.clone())
            .collect();
        next_on.sort();
        if now_on == next_on && configs.iter().all(|c| self.outputs.contains_key(&c.endpoint_id)) {
            for config in configs {
                let slot = self.outputs.get_mut(&config.endpoint_id).unwrap();
                slot.controls.set_gain(config.gain);
                slot.controls.set_muted(config.muted);
                slot.controls
                    .set_delay_frames(ms_to_frames(config.delay_ms, rate));
                slot.config = config;
            }
            self.sync_capture_routes();
            return Ok(());
        }
        let ids: Vec<String> = self.outputs.keys().cloned().collect();
        for id in ids {
            self.stop_output(&id);
        }
        self.outputs.clear();
        for config in configs {
            let slot = self.slot(&config.endpoint_id)?;
            slot.controls.set_gain(config.gain);
            slot.controls.set_muted(config.muted);
            slot.controls
                .set_delay_frames(ms_to_frames(config.delay_ms, rate));
            slot.config = config;
        }
        self.sync_capture_routes();
        if self.running {
            for id in self.enabled_ids() {
                self.start_output(&id);
            }
        }
        Ok(())
    }

    fn start(&mut self) -> Result<(), EngineError> {
        if self.running {
            return Ok(());
        }
        self.running = true;
        self.refresh_endpoints();
        self.restart_capture();
        for id in self.enabled_ids() {
            self.start_output(&id);
        }
        Ok(())
    }

    fn stop_all(&mut self) {
        self.running = false;
        let ids: Vec<String> = self.outputs.keys().cloned().collect();
        for id in ids {
            self.stop_output(&id);
        }
        self.capture = None;
        self.app_captures.clear();
        if let Some(run) = self.calibration.take() {
            run.reply.fail(EngineError::Platform(
                "Calibration was cancelled because routing stopped".into(),
            ));
        }
    }

    fn restart_capture(&mut self) {
        self.capture = None;
        self.app_captures.clear();
        self.unresolved_apps.clear();
        self.mixer.reset();
        if let CaptureSource::Apps(exes) = self.source.clone() {
            let apps = list_audio_apps().unwrap_or_default();
            for exe in exes {
                let Some(app) = apps.iter().find(|a| a.exe.eq_ignore_ascii_case(&exe)) else {
                    self.unresolved_apps.push(exe);
                    continue;
                };
                let config = CaptureConfig {
                    source: CaptureSource::App {
                        exe: exe.clone(),
                        pid: app.pid,
                    },
                    format: self.format,
                    output_endpoint_ids: self.render_ids(),
                    ..CaptureConfig::default()
                };
                match LoopbackCapture::new(config) {
                    Ok((mut capture, consumer)) => {
                        if let Err(error) = capture.start() {
                            self.record("capture", format!("{exe}: {error}"));
                        }
                        self.app_captures.push(AppCapture {
                            exe,
                            pid: app.pid,
                            capture,
                            consumer,
                        });
                    }
                    Err(error) => self.record("capture", format!("{exe}: {error}")),
                }
            }
            self.last_app_check = Instant::now();
            return;
        }
        let config = CaptureConfig {
            source: self.source.clone(),
            format: self.format,
            output_endpoint_ids: self.render_ids(),
            ..CaptureConfig::default()
        };
        match LoopbackCapture::new(config) {
            Ok((mut capture, consumer)) => {
                if let Err(error) = capture.start() {
                    // Blocked/unavailable is reported in capture state; poll keeps retrying.
                    self.record("capture", error.to_string());
                }
                self.capture = Some((capture, consumer));
            }
            Err(error) => self.record("capture", error.to_string()),
        }
    }

    /// Feedback guard: the capture must know every output BEFORE a render stream opens.
    fn sync_capture_routes(&mut self) {
        let ids = self.render_ids();
        for app in &mut self.app_captures {
            let _ = app.capture.set_output_endpoints(ids.clone());
        }
        if let Some((capture, _)) = &mut self.capture {
            if let Err(error) = capture.set_output_endpoints(ids) {
                let message = error.to_string();
                self.record("capture", message);
            }
        }
    }

    fn start_output(&mut self, id: &str) {
        let format = self.format;
        let policy = self.mode.policy();
        let active = self.endpoint_active(id);
        let Some(slot) = self.outputs.get_mut(id) else {
            return;
        };
        if !slot.config.enabled {
            return;
        }
        if !matches!(
            slot.lifecycle.state(),
            OutputLifecycleState::Retrying | OutputLifecycleState::Starting
        ) {
            slot.lifecycle.enable();
        }
        let network = key_of(id).is_some();
        let link = if network {
            match key_of(id).and_then(|key| self.net.server.as_ref()?.link(key)) {
                Some(link) => Some(link),
                None => {
                    slot.lifecycle
                        .failed(Instant::now(), "Network speakers are off");
                    return;
                }
            }
        } else {
            None
        };
        if !active {
            let error = if network {
                "Disconnected"
            } else {
                "Endpoint is unplugged or disabled"
            };
            slot.lifecycle.failed(Instant::now(), error);
            return;
        }
        let max_block = CaptureConfig::default().max_block_frames;
        let queue = match BoundedPcmQueue::new(format, OUTPUT_QUEUE_BLOCKS, max_block) {
            Ok(queue) => queue,
            Err(_) => {
                slot.lifecycle
                    .failed(Instant::now(), "Invalid output queue");
                return;
            }
        };
        let (producer, consumer) = queue.into_spsc();
        slot.controls.apply_policy(policy, format.sample_rate);
        let pipeline = match RenderPipeline::new(
            consumer,
            format,
            max_block,
            slot.controls.clone(),
            slot.metrics.clone(),
        ) {
            Ok(pipeline) => pipeline.with_spatial(self.spatial.clone()),
            Err(_) => {
                slot.lifecycle
                    .failed(Instant::now(), "Invalid render pipeline");
                return;
            }
        };
        slot.producer = Some(producer);
        let render_format = PcmFormat {
            sample_rate: format.sample_rate,
            channels: crate::render::OUTPUT_CHANNELS as u16,
        };
        let exclusive = slot.config.exclusive;
        slot.worker = Some(match link {
            Some(link) => OutputWorker::Network(NetworkWorker::spawn(
                pipeline,
                format.sample_rate,
                link,
                slot.metrics.clone(),
            )),
            None => OutputWorker::Wasapi(RenderWorker::spawn(
                id.into(),
                pipeline,
                render_format,
                policy.render_buffer_hns,
                slot.metrics.clone(),
                exclusive,
            )),
        });
        // A new pipeline starts without a convolver: hand it one again.
        self.load_convolver(id);
    }

    /// Loads this output's impulse response (if any) and hands it to its render thread.
    fn load_convolver(&mut self, id: &str) {
        let rate = self.format.sample_rate;
        let Some(slot) = self.outputs.get_mut(id) else {
            return;
        };
        let Some(path) = slot.config.convolution.clone() else {
            slot.controls.convolver.offer(None);
            slot.convolution = None;
            return;
        };
        let loaded = crate::convolve::load_impulse_response(std::path::Path::new(&path), rate)
            .and_then(|(l, r)| crate::convolve::StereoConvolver::new(&l, &r));
        match loaded {
            Ok(conv) => {
                slot.convolution = Some(Ok(conv.taps));
                slot.controls.convolver.offer(Some(conv));
            }
            Err(error) => {
                slot.convolution = Some(Err(error.clone()));
                slot.controls.convolver.offer(None);
                self.record("convolution", error);
            }
        }
    }

    fn stop_output(&mut self, id: &str) {
        if let Some(slot) = self.outputs.get_mut(id) {
            slot.producer = None;
            slot.worker = None; // Drop joins the worker thread.
            slot.metrics.set_device_padding(0);
            slot.controls.set_align_frames(0);
            if !slot.config.enabled || !self.running {
                slot.lifecycle.disable();
            }
        }
    }

    /// Rebuilds the `net:` endpoints from the allowed devices and their links.
    fn merge_network(&mut self) {
        let net = &self.net;
        merge_network_endpoints(&mut self.endpoints, &net.devices, &|key: &str| net.connected(key));
    }

    /// Gives a device that has no output yet (first join) one, switched on
    /// when there is room. An existing output (from a profile, or one the
    /// user turned off) is left as it is.
    fn add_network_output(&mut self, key: &str) {
        let id = endpoint_id(key);
        if self.outputs.contains_key(&id) {
            return;
        }
        let room = self.enabled_ids().len() < MAX_ENABLED_OUTPUTS;
        self.merge_network();
        if let Ok(slot) = self.slot(&id) {
            slot.config.enabled = room;
        }
        if room {
            self.recompute_placement();
        }
    }

    fn set_network(&mut self, enabled: bool, allowed: Vec<NetworkDevice>) {
        // The app's list wins for names; devices learned since stay listed.
        for device in allowed {
            match self.net.devices.iter_mut().find(|d| d.key == device.key) {
                Some(known) => known.name = device.name,
                None => self.net.devices.push(device),
            }
        }
        if enabled && self.net.server.is_none() {
            match NetworkServer::start(self.net.devices.clone(), DEFAULT_PORT) {
                Ok(server) => {
                    self.net.server = Some(server);
                    self.net.error = None;
                }
                Err(error) => {
                    let message = format!("Could not start network speakers: {error}");
                    self.record("network", message.clone());
                    self.net.error = Some(message);
                }
            }
        } else if !enabled {
            // Also clears an error left by a failed start.
            self.net.error = None;
        }
        if !enabled && self.net.server.is_some() {
            let ids: Vec<String> = self
                .outputs
                .keys()
                .filter(|id| key_of(id).is_some())
                .cloned()
                .collect();
            for id in &ids {
                self.stop_output(id);
            }
            self.net.server = None; // Drop stops the server and closes every socket.
            self.net.qr = None;
            self.net.error = None;
            let now = Instant::now();
            for id in &ids {
                if let Some(slot) = self.outputs.get_mut(id) {
                    slot.lifecycle.failed(now, "Network speakers are off");
                }
            }
        }
        self.merge_network();
    }

    /// Removes an allowed device everywhere: server, device list, output slot
    /// and endpoint. Works while the server is off too (saved devices).
    fn forget_network_device(&mut self, key: &str) -> Result<(), EngineError> {
        let id = endpoint_id(key);
        let known = self.net.devices.iter().any(|d| d.key == key);
        let pending = self
            .net
            .server
            .as_ref()
            .is_some_and(|s| s.pending().iter().any(|p| p.key == key));
        if !known && !pending {
            return Err(EngineError::UnknownEndpoint(id));
        }
        if self.calibration.is_some() && self.outputs.contains_key(&id) {
            return Err(EngineError::Platform(
                "Wait for calibration to finish before forgetting a speaker".into(),
            ));
        }
        if let Some(server) = &self.net.server {
            server.forget(key);
        }
        self.net.devices.retain(|d| d.key != key);
        self.net.joins.retain(|d| d.key != key);
        if self.outputs.contains_key(&id) {
            self.stop_output(&id);
            self.outputs.remove(&id);
            self.sync_capture_routes();
            self.recompute_placement();
        }
        self.merge_network();
        Ok(())
    }

    /// Applies server events: joins become outputs (and start while routing
    /// runs), leaves mark the output disconnected. Other outputs are untouched.
    fn service_network(&mut self) {
        let Some(server) = &self.net.server else {
            return;
        };
        let events = server.events();
        if events.is_empty() {
            return;
        }
        // Endpoint states first, so start_output sees who is connected now.
        let mut joined = Vec::new();
        for event in &events {
            if let ServerEvent::Joined { key, name } = event {
                self.net.learn(key, name);
                joined.push(key.clone());
            }
        }
        self.merge_network();
        for key in joined {
            self.add_network_output(&key);
        }
        for event in events {
            match event {
                ServerEvent::Joined { key, .. } => {
                    let id = endpoint_id(&key);
                    if self.running && self.outputs.get(&id).is_some_and(|s| s.config.enabled) {
                        self.stop_output(&id);
                        self.start_output(&id);
                    }
                }
                ServerEvent::Left { key } => {
                    let id = endpoint_id(&key);
                    if self.outputs.contains_key(&id) {
                        self.stop_output(&id);
                        if let Some(slot) = self.outputs.get_mut(&id) {
                            slot.lifecycle.failed(Instant::now(), "Disconnected");
                        }
                    }
                }
                ServerEvent::Pending(_) => {}
            }
        }
    }

    fn refresh_endpoints(&mut self) {
        self.catalog.changed.store(false, Ordering::Release);
        match self.catalog.enumerate() {
            Ok(endpoints) => self.endpoints = endpoints,
            Err(error) => self.record("endpoints", error.to_string()),
        }
        self.merge_network();
        let now = Instant::now();
        let active: Vec<String> = self
            .endpoints
            .iter()
            .filter(|e| e.state == EndpointState::Active)
            .map(|e| e.id.clone())
            .collect();
        for slot in self.outputs.values_mut() {
            if active.contains(&slot.config.endpoint_id) {
                slot.lifecycle.device_returned(now);
            }
        }
    }

    fn tick(&mut self) {
        if self.catalog.changed.load(Ordering::Acquire) {
            self.refresh_endpoints();
        }
        self.service_network();
        self.service_spectrum();
        // Free convolvers the render threads have swapped out.
        for slot in self.outputs.values() {
            slot.controls.convolver.collect();
        }
        if self.running {
            self.service_app_modes();
        }
        if !self.running {
            self.publish(false);
            return;
        }
        // 1. Service capture and fan out PCM. Full output queues drop only for that output.
        let mut capture_error = None;
        let calibrating = self.calibration.is_some();
        if let Some((capture, consumer)) = &mut self.capture {
            if let Err(error) = capture.poll() {
                capture_error = Some(error.to_string());
            }
            loop {
                match consumer.pop_into(&mut self.scratch) {
                    Ok(_) if calibrating => {}
                    Ok((first, frames)) => {
                        let samples = &self.scratch[..frames * usize::from(self.format.channels)];
                        for slot in self.outputs.values_mut() {
                            if let Some(producer) = &mut slot.producer {
                                if producer.push_samples(first, samples) == QueueStatus::Full {
                                    slot.metrics.record_overrun();
                                }
                            }
                        }
                    }
                    Err(_) => break,
                }
            }
        }
        if let Some(message) = capture_error {
            self.record("capture", message);
        }
        self.service_app_captures();
        self.service_calibration();

        // 2. Supervise workers; one failure never touches other outputs.
        let now = Instant::now();
        let mut failures = Vec::new();
        let mut retries = Vec::new();
        for (id, slot) in &mut self.outputs {
            if let Some(worker) = &slot.worker {
                match worker.poll() {
                    WorkerPoll::Running => slot.lifecycle.started(),
                    WorkerPoll::Failed(error) => {
                        slot.worker = None;
                        slot.producer = None;
                        slot.lifecycle.failed(now, error.clone());
                        failures.push((id.clone(), error));
                    }
                    WorkerPoll::Starting | WorkerPoll::Stopped => {}
                }
            } else if slot.config.enabled && slot.lifecycle.poll_retry(now) {
                retries.push(id.clone());
            }
        }
        for (id, error) in failures {
            let name = self.display_name(&id);
            self.record("output", format!("{name}: {error}"));
        }
        for id in retries {
            self.start_output(&id);
        }

        // 3. Synchronized mode: align to the slowest Windows-reported device latency.
        if self.mode.policy().align_outputs && now.duration_since(self.last_align) >= ALIGN_INTERVAL
        {
            self.last_align = now;
            let running: Vec<&OutputSlot> = self
                .outputs
                .values()
                .filter(|s| s.lifecycle.state() == OutputLifecycleState::Running)
                .collect();
            let latencies: Vec<Option<u64>> = running
                .iter()
                .map(|s| {
                    let m = s.metrics.snapshot();
                    (m.device_buffer_frames > 0).then(|| m.device_latency_frames())
                })
                .collect();
            let aligned = alignment_frames(&latencies, self.format.sample_rate);
            let threshold = ms_to_frames(2, self.format.sample_rate);
            for (slot, frames) in running.iter().zip(aligned) {
                if slot.controls.align_frames().abs_diff(frames) > threshold {
                    slot.controls.set_align_frames(frames);
                }
            }
        }
        self.publish(false);
    }

    /// Polls every app capture, mixes them on the shared frame timeline and fans
    /// the mix out. Every 3 s, re-resolves apps that were missing or restarted.
    fn service_app_captures(&mut self) {
        if !matches!(self.source, CaptureSource::Apps(_)) {
            return;
        }
        let ch = usize::from(self.format.channels);
        let mut errors = Vec::new();
        for app in &mut self.app_captures {
            if let Err(error) = app.capture.poll() {
                errors.push(format!("{}: {error}", app.exe));
            }
            while let Ok((first, frames)) = app.consumer.pop_into(&mut self.scratch) {
                self.mixer.add(first, &self.scratch[..frames * ch]);
            }
        }
        for e in errors {
            self.record("capture", e);
        }
        while let Some(first) = self.mixer.pop(&mut self.mix_out) {
            if self.calibration.is_some() {
                continue;
            }
            for slot in self.outputs.values_mut() {
                if let Some(producer) = &mut slot.producer {
                    if producer.push_samples(first, &self.mix_out) == QueueStatus::Full {
                        slot.metrics.record_overrun();
                    }
                }
            }
        }
        if self.last_app_check.elapsed() >= Duration::from_secs(3) {
            self.last_app_check = Instant::now();
            let apps = list_audio_apps().unwrap_or_default();
            let moved = self.app_captures.iter().any(|c| {
                apps.iter()
                    .find(|a| a.exe.eq_ignore_ascii_case(&c.exe))
                    .is_some_and(|a| a.pid != c.pid && a.active)
            });
            let appeared = self
                .unresolved_apps
                .iter()
                .any(|exe| apps.iter().any(|a| a.exe.eq_ignore_ascii_case(exe)));
            if moved || appeared {
                self.restart_capture();
            }
        }
    }

    fn start_calibration(&mut self, mic_id: String, reply: RunReply) -> Result<(), EngineError> {
        let fail = |m: &str| EngineError::Platform(m.into());
        if self.calibration.is_some() {
            return Err(fail("Calibration is already running"));
        }
        if !self.running {
            return Err(fail("Start routing first, then calibrate"));
        }
        let order: Vec<String> = self
            .outputs
            .values()
            .filter(|s| {
                s.config.enabled
                    && !s.config.muted
                    && s.lifecycle.state() == OutputLifecycleState::Running
            })
            .map(|s| s.config.endpoint_id.clone())
            .collect();
        let needed = if reply.is_room() { 1 } else { 2 };
        if order.len() < needed {
            return Err(fail(if needed == 1 {
                "Room measurement needs at least one playing, unmuted output"
            } else {
                "Calibration needs at least two playing, unmuted outputs"
            }));
        }
        let mics = list_microphones().map_err(|e| fail(&e.to_string()))?;
        let mic = mics
            .iter()
            .find(|m| m.id == mic_id)
            .ok_or_else(|| fail("Microphone not found"))?;
        let recorder =
            MicRecorder::start(mic.id.clone(), self.format.sample_rate).map_err(|e| fail(&e))?;
        // Spatial off so each click reaches only the output being measured. Gains
        // stay as the user set them: never louder than what they normally hear.
        let mut config = self.spatial_config;
        config.enabled = false;
        self.spatial.set(config, &[]);
        let rate = self.format.sample_rate;
        let names = order.iter().map(|id| self.display_name(id)).collect();
        self.calibration = Some(CalibrationRun {
            total: if reply.is_room() {
                room::total_frames(order.len(), rate)
            } else {
                total_frames(order.len(), rate)
            },
            order,
            names,
            recorder,
            mic_name: mic.name.clone(),
            started: Instant::now(),
            generated: 0,
            frame_base: 1 << 40,
            reply,
        });
        Ok(())
    }

    /// Generates the click track in real time and finishes the run.
    fn service_calibration(&mut self) {
        let Some(run) = &mut self.calibration else {
            return;
        };
        let rate = self.format.sample_rate;
        let ch = usize::from(self.format.channels);
        let block = (self.click_block.len() / ch) as u64;
        let due = (run.started.elapsed().as_secs_f64() * f64::from(rate)) as u64;
        while run.generated + block <= due.min(run.total) {
            let room_run = run.reply.is_room();
            let soloed = if room_run {
                room::soloed_output(run.generated, run.order.len(), rate)
            } else {
                soloed_output(run.generated, run.order.len(), rate)
            };
            for i in 0..block {
                let x = if room_run {
                    room::track_sample(run.generated + i, rate)
                } else {
                    click_sample(run.generated + i, rate)
                };
                self.click_block[i as usize * ch..(i as usize + 1) * ch].fill(x);
            }
            let first = run.frame_base + run.generated;
            for (index, id) in run.order.iter().enumerate() {
                if let Some(producer) = self.outputs.get_mut(id).and_then(|s| s.producer.as_mut()) {
                    let samples = if soloed == Some(index) {
                        &self.click_block
                    } else {
                        &self.silent_block
                    };
                    let _ = producer.push_samples(first, samples);
                }
            }
            run.generated += block;
        }
        // Let the last clicks reach the microphone (outputs may lag ~1 s).
        let tail = Duration::from_millis(1_500);
        let total_time = Duration::from_secs_f64(run.total as f64 / f64::from(rate));
        if run.generated < run.total || run.started.elapsed() < total_time + tail {
            return;
        }
        let run = self.calibration.take().unwrap();
        match run.reply {
            RunReply::Delay(reply) => {
                let report = self.finish_calibration(
                    run.order,
                    run.names,
                    run.recorder,
                    run.mic_name,
                    run.started,
                );
                self.recompute_placement();
                let _ = reply.send(report);
            }
            RunReply::Room(reply) => {
                let report = self.finish_room(
                    run.order,
                    run.names,
                    run.recorder,
                    run.mic_name,
                    run.started,
                );
                // Restores the spatial field and publishes the new EQ.
                self.recompute_placement();
                let _ = reply.send(report);
            }
        }
    }

    fn finish_room(
        &mut self,
        order: Vec<String>,
        names: Vec<String>,
        recorder: MicRecorder,
        mic_name: String,
        started: Instant,
    ) -> Result<RoomReport, EngineError> {
        let rate = self.format.sample_rate;
        let fs = f64::from(rate);
        let mic_started = recorder.started_at;
        let recording = recorder.finish().map_err(EngineError::Platform)?;
        let offset = started.saturating_duration_since(mic_started).as_secs_f64()
            - mic_started.saturating_duration_since(started).as_secs_f64();
        let track_start = (offset * fs).round() as i64;
        let slice = |from_s: f64, len_s: f64| -> Option<&[f32]> {
            let a = track_start + (from_s * fs) as i64;
            let b = a + (len_s * fs) as i64;
            (a >= 0 && (b as usize) <= recording.len()).then(|| &recording[a as usize..b as usize])
        };
        let noise = slice(0.1, room::LEAD_IN_S - 0.2)
            .ok_or_else(|| EngineError::Platform("Recording is too short".into()))?;
        let mut outputs = Vec::new();
        let mut applied = 0;
        for (k, id) in order.iter().enumerate() {
            let window = slice(
                room::LEAD_IN_S + room::SEGMENT_S * k as f64,
                room::SEGMENT_S,
            );
            let target = self.sound.room_target;
            let result = window.map(|w| room::analyze(w, noise, rate, target));
            let usable = result.as_ref().is_some_and(|r| r.snr_db >= 20.0);
            if usable {
                if let (Some(slot), Some(r)) = (self.outputs.get_mut(id), &result) {
                    slot.config.eq.room_correction = true;
                    slot.config.eq.correction_db = r.correction_db;
                    for (stored, measured) in
                        slot.config.eq.room_response.iter_mut().zip(&r.response_db)
                    {
                        *stored = *measured;
                    }
                    applied += 1;
                }
            }
            outputs.push(RoomOutput {
                endpoint_id: id.clone(),
                name: names[k].clone(),
                applied: usable,
                result,
            });
        }
        let message = if recorded_silence(&recording) {
            silent_mic_message(&mic_name)
        } else if applied == order.len() {
            format!("Measured {applied} output(s) with {mic_name} and applied room correction.")
        } else {
            format!(
                "Corrected {applied} of {} outputs. The others were too quiet or the room too noisy; turn them up and try again.",
                order.len()
            )
        };
        Ok(RoomReport {
            microphone: mic_name,
            message,
            outputs,
        })
    }

    fn finish_calibration(
        &mut self,
        order: Vec<String>,
        names: Vec<String>,
        recorder: MicRecorder,
        mic_name: String,
        started: Instant,
    ) -> Result<CalibrationReport, EngineError> {
        let rate = self.format.sample_rate;
        let mic_started = recorder.started_at;
        let recording = recorder.finish().map_err(EngineError::Platform)?;
        let offset = started.saturating_duration_since(mic_started).as_secs_f64()
            - mic_started.saturating_duration_since(started).as_secs_f64();
        let track_start = (offset * f64::from(rate)).round() as i64;
        let arrivals: Vec<_> = (0..order.len())
            .map(|k| analyze_output(&recording, rate, track_start, k))
            .collect();
        let current: Vec<u32> = order
            .iter()
            .map(|id| self.outputs.get(id).map_or(0, |s| s.config.delay_ms))
            .collect();
        let (relative, delays) = solve_delays(&arrivals, &current);
        let detected = relative.iter().filter(|r| r.is_some()).count();
        let applied = detected >= 2;
        if applied {
            for (id, delay) in order.iter().zip(&delays) {
                if let Some(slot) = self.outputs.get_mut(id) {
                    slot.config.delay_ms = *delay;
                    slot.controls.set_delay_frames(ms_to_frames(*delay, rate));
                }
            }
        }
        let old_trims: Vec<f32> = order
            .iter()
            .map(|id| self.outputs.get(id).map_or(0.0, |s| s.config.trim_db))
            .collect();
        let new_trims = if applied && self.sound.match_levels {
            solve_levels(&arrivals, &old_trims, MIN_TRIM_DB)
        } else {
            None
        };
        if let Some(trims) = &new_trims {
            for (id, trim) in order.iter().zip(trims) {
                if let Some(slot) = self.outputs.get_mut(id) {
                    slot.config.trim_db = *trim;
                }
            }
        }
        let outputs = order
            .iter()
            .enumerate()
            .map(|(k, id)| CalibratedOutput {
                endpoint_id: id.clone(),
                name: names[k].clone(),
                detected: relative[k].is_some(),
                snr: arrivals[k].map_or(0.0, |a| a.snr),
                relative_ms: relative[k],
                old_delay_ms: current[k],
                new_delay_ms: if applied { delays[k] } else { current[k] },
                old_trim_db: old_trims[k],
                new_trim_db: new_trims.as_ref().map_or(old_trims[k], |t| t[k]),
            })
            .collect();
        let message = if recorded_silence(&recording) {
            silent_mic_message(&mic_name)
        } else if applied && new_trims.is_some() {
            format!("Aligned and level-matched {detected} outputs using {mic_name}.")
        } else if applied {
            format!("Aligned {detected} outputs using {mic_name}.")
        } else {
            format!(
                "Heard {detected} of {} outputs clearly — no delays changed. Turn the speakers up, keep the room quiet and hold the microphone at your seat.",
                order.len()
            )
        };
        Ok(CalibrationReport {
            microphone: mic_name,
            applied,
            message,
            outputs,
        })
    }

    fn display_name(&self, id: &str) -> String {
        self.endpoints
            .iter()
            .find(|e| e.id == id)
            .map(|e| e.name.clone())
            .unwrap_or_else(|| id.into())
    }

    fn publish(&mut self, force: bool) {
        let now = Instant::now();
        if !force && now.duration_since(self.last_snapshot) < SNAPSHOT_INTERVAL {
            return;
        }
        self.last_snapshot = now;
        let rate = self.format.sample_rate;
        let outputs = self
            .outputs
            .values()
            .map(|slot| {
                let metrics = slot.metrics.snapshot();
                let (buffered_ms, device_latency_ms, estimated_latency_ms) =
                    metrics_ms(&metrics, rate);
                let running = slot.lifecycle.state() == OutputLifecycleState::Running;
                OutputStatus {
                    endpoint_id: slot.config.endpoint_id.clone(),
                    name: self.display_name(&slot.config.endpoint_id),
                    endpoint_available: self.endpoint_active(&slot.config.endpoint_id),
                    config: slot.config.clone(),
                    state: slot.lifecycle.state(),
                    last_error: slot.lifecycle.last_error().map(str::to_string),
                    retry_in_ms: slot
                        .lifecycle
                        .retry_at()
                        .map(|at| at.saturating_duration_since(now).as_millis() as u64),
                    align_ms: f64::from(slot.controls.align_frames()) * 1_000.0 / f64::from(rate),
                    placement_delay_ms: f64::from(slot.controls.placement_delay_frames()) * 1_000.0
                        / f64::from(rate),
                    placement_level: slot.controls.trim(),
                    resolved_azimuth: slot
                        .config
                        .azimuth_deg
                        .or_else(|| position_azimuth(slot.config.position)),
                    buffered_ms,
                    device_latency_ms,
                    estimated_latency_ms,
                    bluetooth: self
                        .endpoints
                        .iter()
                        .any(|e| e.id == slot.config.endpoint_id && e.is_bluetooth()),
                    warning: if running {
                        latency_warning(
                            self.mode,
                            estimated_latency_ms,
                            f64::from(slot.config.delay_ms)
                                + f64::from(slot.controls.align_frames()) * 1_000.0
                                    / f64::from(rate),
                        )
                    } else {
                        None
                    },
                    metrics,
                    exclusive_active: running
                        && slot.metrics.exclusive_active.load(Ordering::Relaxed),
                    convolution_taps: slot.convolution.as_ref().and_then(|r| r.as_ref().ok().copied()),
                    convolution_error: slot.convolution.as_ref().and_then(|r| r.as_ref().err().cloned()),
                }
            })
            .collect();
        let capture_state = if matches!(self.source, CaptureSource::Apps(_)) && self.running {
            let capturing = self
                .app_captures
                .iter()
                .find(|a| matches!(a.capture.state(), CaptureState::Capturing(_)));
            match (capturing, self.app_captures.first()) {
                (Some(app), _) => app.capture.state().clone(),
                (None, Some(app)) => app.capture.state().clone(),
                (None, None) => CaptureState::Unavailable { endpoint_id: None },
            }
        } else {
            self.capture
                .as_ref()
                .map(|(c, _)| c.state().clone())
                .unwrap_or(CaptureState::Stopped)
        };
        let app_sources = match &self.source {
            CaptureSource::Apps(exes) => exes
                .iter()
                .map(|exe| {
                    let capture = self.app_captures.iter().find(|a| &a.exe == exe);
                    AppSourceStatus {
                        exe: exe.clone(),
                        pid: capture.map(|a| a.pid),
                        capturing: capture.is_some_and(|a| {
                            matches!(a.capture.state(), CaptureState::Capturing(_))
                        }),
                    }
                })
                .collect(),
            _ => Vec::new(),
        };
        let snapshot = EngineSnapshot {
            running: self.running,
            mode: self.mode,
            source: self.source.clone(),
            capture_state,
            capture_format: self.format,
            outputs,
            distance_compensation: self.distance_compensation,
            spatial: self.spatial_config,
            sound: self.sound,
            listening_mode: ListeningMode::matching(&self.sound),
            app_modes: self.app_modes.clone(),
            auto_mode: self.auto_mode.clone(),
            app_sources,
            calibration: self.calibration.as_ref().map(|run| {
                let rate = f64::from(self.format.sample_rate);
                let soloed = soloed_output(run.generated, run.order.len(), self.format.sample_rate);
                CalibrationProgress {
                    elapsed_s: run.started.elapsed().as_secs_f64(),
                    total_s: run.total as f64 / rate + 1.5,
                    measuring_room: run.reply.is_room(),
                    playing: soloed.map(|k| run.names[k].clone()),
                }
            }),
            endpoints: self.endpoints.clone(),
            recent_errors: self.errors.iter().cloned().collect(),
            updated_at_ms: self.started.elapsed().as_millis() as u64,
            network: self.net.status(),
        };
        if let Ok(mut shared) = self.shared.lock() {
            *shared = snapshot;
        }
    }
}

//! Engine control thread. Owns the thread-affine loopback capture and endpoint
//! catalog, fans captured PCM into one bounded queue per output, supervises render
//! workers, and publishes throttled snapshots. Commands arrive over a channel; the
//! handle is Send + Sync so Tauri can share it.
use super::windows::{CaptureError, EndpointCatalog, LoopbackCapture};
use super::windows_apps::list_audio_apps;
use super::windows_render::{RenderWorker, WorkerPoll};
use crate::capture::AudioApp;
use crate::capture::{AudioEndpoint, CaptureConfig, CaptureSource, CaptureState, EndpointState};
use crate::eq::{design, EqSettings};
use crate::mixer::TimelineMixer;
use crate::output_state::{OutputLifecycle, OutputLifecycleState};
use crate::placement::{apply_preset, compute_placement, SoundPreset, SpeakerPosition};
use crate::render::{
    ms_to_frames, AudioMode, ChannelRole, OutputControls, OutputMetrics, RenderPipeline,
    MAX_ENABLED_OUTPUTS,
};
use crate::routing::AppSourceStatus;
use crate::routing::{
    alignment_frames, latency_warning, metrics_ms, EngineSnapshot, ErrorRecord, OutputConfig,
    OutputStatus,
};
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
    ListApps(Reply<Vec<AudioApp>>),
    ApplyPreset(SoundPreset, Reply<()>),
    Refresh(Reply<Vec<AudioEndpoint>>),
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
        let (reply, result) = mpsc::sync_channel(1);
        self.commands
            .lock()
            .map_err(|_| EngineError::Disconnected)?
            .send(make(reply))
            .map_err(|_| EngineError::Disconnected)?;
        result
            .recv_timeout(Duration::from_secs(10))
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
    pub fn set_distance_compensation(&self, enabled: bool) -> Result<(), EngineError> {
        self.call(|r| Command::SetCompensation(enabled, r))
    }
    pub fn apply_preset(&self, preset: SoundPreset) -> Result<(), EngineError> {
        self.call(|r| Command::ApplyPreset(preset, r))
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

struct OutputSlot {
    config: OutputConfig,
    controls: Arc<OutputControls>,
    metrics: Arc<OutputMetrics>,
    lifecycle: OutputLifecycle,
    producer: Option<PcmProducer>,
    worker: Option<RenderWorker>,
    published_eq: Option<EqSettings>,
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
    scratch: Vec<f32>,
    errors: VecDeque<ErrorRecord>,
    started: Instant,
    last_snapshot: Instant,
    last_align: Instant,
    shared: Arc<Mutex<EngineSnapshot>>,
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
            running: false,
            endpoints,
            outputs: BTreeMap::new(),
            capture: None,
            app_captures: Vec::new(),
            unresolved_apps: Vec::new(),
            mixer: TimelineMixer::new(usize::from(format.channels), 24_000, 960),
            mix_out: vec![0.0; 480 * usize::from(format.channels)],
            last_app_check: now,
            scratch: vec![
                0.0;
                CaptureConfig::default().max_block_frames * usize::from(format.channels)
            ],
            errors: VecDeque::new(),
            started: now,
            last_snapshot: now - SNAPSHOT_INTERVAL,
            last_align: now,
            shared,
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
                    if !self.endpoints.iter().any(|e| &e.id == id) {
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
            Command::Shutdown => {}
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
        for (slot, placement) in self.outputs.values_mut().zip(placements) {
            slot.controls.set_role(slot.config.role);
            if slot.published_eq != Some(slot.config.eq) {
                slot.controls
                    .eq
                    .publish(&design(&slot.config.eq, rate), slot.config.eq.headroom());
                slot.published_eq = Some(slot.config.eq);
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
        let ids: Vec<String> = self.outputs.keys().cloned().collect();
        for id in ids {
            self.stop_output(&id);
        }
        self.outputs.clear();
        let rate = self.format.sample_rate;
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
                    output_endpoint_ids: self.enabled_ids(),
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
            output_endpoint_ids: self.enabled_ids(),
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
        let ids = self.enabled_ids();
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
        if !active {
            slot.lifecycle
                .failed(Instant::now(), "Endpoint is unplugged or disabled");
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
        slot.worker = Some(RenderWorker::spawn(
            id.into(),
            pipeline,
            format,
            policy.render_buffer_hns,
            slot.metrics.clone(),
        ));
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

    fn refresh_endpoints(&mut self) {
        self.catalog.changed.store(false, Ordering::Release);
        match self.catalog.enumerate() {
            Ok(endpoints) => self.endpoints = endpoints,
            Err(error) => self.record("endpoints", error.to_string()),
        }
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
        if !self.running {
            self.publish(false);
            return;
        }
        // 1. Service capture and fan out PCM. Full output queues drop only for that output.
        let mut capture_error = None;
        if let Some((capture, consumer)) = &mut self.capture {
            if let Err(error) = capture.poll() {
                capture_error = Some(error.to_string());
            }
            loop {
                match consumer.pop_into(&mut self.scratch) {
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
            app_sources,
            endpoints: self.endpoints.clone(),
            recent_errors: self.errors.iter().cloned().collect(),
            updated_at_ms: self.started.elapsed().as_millis() as u64,
        };
        if let Ok(mut shared) = self.shared.lock() {
            *shared = snapshot;
        }
    }
}

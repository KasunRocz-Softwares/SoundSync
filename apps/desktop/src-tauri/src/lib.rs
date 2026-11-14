//! Tauri control plane. Every command validates untrusted input against the
//! latest engine snapshot before touching the engine; rejected input changes nothing.
use serde::Serialize;
use std::path::PathBuf;
use syncsound_engine::capture::{AudioApp, CaptureSource};
use syncsound_engine::capture::{AudioEndpoint, CaptureState};
use syncsound_engine::commands::MAX_ENDPOINT_ID_LEN;
use syncsound_engine::commands::{
    parse_mode, parse_source, validate_delay, validate_enable, validate_endpoint_id,
    validate_volume, CommandError,
};
use syncsound_engine::eq::EqSettings;
use syncsound_engine::placement::{validate_distance, validate_trim, SoundPreset, SpeakerPosition};
use syncsound_engine::platform::windows_mic::Microphone;
use syncsound_engine::profile::{MissingOutput, Profile, ProfileStore};
use syncsound_engine::render::{AudioMode, ChannelRole};
use syncsound_engine::routing::{CalibrationReport, RoomReport};
use syncsound_engine::spatial::{
    validate_azimuth, validate_spatial, SourceLayout, SpatialConfig, StereoSpread,
};
use syncsound_engine::{diagnostics, AudioEngine, EngineError, EngineSnapshot, OutputStatus};
use tauri::{Manager, State};

mod system;

pub(crate) struct AppState {
    pub(crate) engine: AudioEngine,
    pub(crate) profiles: ProfileStore,
    diagnostics_dir: PathBuf,
    pub(crate) data_dir: PathBuf,
    pub(crate) settings: std::sync::Mutex<system::AppSettings>,
    /// Set by the tray's Quit so the close-to-tray handler lets the app exit.
    pub(crate) quitting: std::sync::atomic::AtomicBool,
}

/// Applies a profile to the engine: mode, compensation, spatial, source, outputs.
pub(crate) fn apply_profile(
    engine: &AudioEngine,
    profile: &Profile,
) -> CmdResult<ProfileLoadReport> {
    let endpoints = engine.refresh_endpoints().map_err(engine_error)?;
    let plan = profile.plan_apply(&endpoints);
    engine.set_mode(plan.mode).map_err(engine_error)?;
    engine
        .set_distance_compensation(plan.distance_compensation)
        .map_err(engine_error)?;
    engine.set_spatial(plan.spatial).map_err(engine_error)?;
    engine.set_sound(plan.sound).map_err(engine_error)?;
    engine.set_app_modes(plan.app_modes.clone()).map_err(engine_error)?;
    if let Some(source) = plan.source.clone() {
        engine.set_source(source).map_err(engine_error)?;
    }
    let applied_outputs = plan.outputs.len();
    engine.apply_outputs(plan.outputs).map_err(engine_error)?;
    Ok(ProfileLoadReport {
        name: profile.name.clone(),
        mode: plan.mode,
        applied_outputs,
        missing: plan.missing,
        missing_source: plan.missing_source,
    })
}

type CmdResult<T> = Result<T, CommandError>;

fn engine_error(error: EngineError) -> CommandError {
    let code = match error {
        EngineError::UnknownEndpoint(_) => "unknown_device",
        EngineError::TooManyOutputs => "too_many_outputs",
        EngineError::Disconnected => "engine_unavailable",
        EngineError::Platform(_) => "platform",
    };
    CommandError::new(code, error.to_string())
}

fn enabled_ids(snapshot: &EngineSnapshot) -> Vec<String> {
    snapshot
        .outputs
        .iter()
        .filter(|o| o.config.enabled)
        .map(|o| o.endpoint_id.clone())
        .collect()
}

#[tauri::command]
async fn get_audio_devices(state: State<'_, AppState>) -> CmdResult<Vec<AudioEndpoint>> {
    state.engine.refresh_endpoints().map_err(engine_error)
}

#[tauri::command]
async fn get_engine_status(state: State<'_, AppState>) -> CmdResult<EngineSnapshot> {
    Ok(state.engine.snapshot())
}

#[tauri::command]
async fn start_audio_engine(
    app: tauri::AppHandle,
    state: State<'_, AppState>,
) -> CmdResult<EngineSnapshot> {
    state.engine.start().map_err(engine_error)?;
    system::refresh_tray(&app);
    Ok(state.engine.snapshot())
}

#[tauri::command]
async fn stop_audio_engine(
    app: tauri::AppHandle,
    state: State<'_, AppState>,
) -> CmdResult<EngineSnapshot> {
    state.engine.stop().map_err(engine_error)?;
    system::refresh_tray(&app);
    Ok(state.engine.snapshot())
}

#[tauri::command]
async fn select_capture_source(
    kind: String,
    endpoint_id: Option<String>,
    state: State<'_, AppState>,
) -> CmdResult<()> {
    let snapshot = state.engine.snapshot();
    let source = parse_source(&kind, endpoint_id.as_deref(), &snapshot.endpoints)?;
    state.engine.set_source(source).map_err(engine_error)
}

#[tauri::command]
async fn enable_device(id: String, state: State<'_, AppState>) -> CmdResult<()> {
    let snapshot = state.engine.snapshot();
    validate_endpoint_id(&id, &snapshot.endpoints)?;
    validate_enable(&id, &enabled_ids(&snapshot))?;
    state.engine.enable(id).map_err(engine_error)
}

#[tauri::command]
async fn disable_device(id: String, state: State<'_, AppState>) -> CmdResult<()> {
    let snapshot = state.engine.snapshot();
    validate_endpoint_id(&id, &snapshot.endpoints)?;
    state.engine.disable(id).map_err(engine_error)
}

#[tauri::command]
async fn set_device_volume(id: String, volume: f64, state: State<'_, AppState>) -> CmdResult<()> {
    let snapshot = state.engine.snapshot();
    validate_endpoint_id(&id, &snapshot.endpoints)?;
    let volume = validate_volume(volume)?;
    state.engine.set_gain(id, volume).map_err(engine_error)
}

#[tauri::command]
async fn set_device_mute(id: String, muted: bool, state: State<'_, AppState>) -> CmdResult<()> {
    let snapshot = state.engine.snapshot();
    validate_endpoint_id(&id, &snapshot.endpoints)?;
    state.engine.set_mute(id, muted).map_err(engine_error)
}

#[tauri::command]
async fn set_device_delay(id: String, delay_ms: i64, state: State<'_, AppState>) -> CmdResult<()> {
    let snapshot = state.engine.snapshot();
    validate_endpoint_id(&id, &snapshot.endpoints)?;
    let delay = validate_delay(delay_ms)?;
    state.engine.set_delay(id, delay).map_err(engine_error)
}

#[tauri::command]
async fn set_audio_mode(mode: String, state: State<'_, AppState>) -> CmdResult<()> {
    let mode = parse_mode(&mode)?;
    state.engine.set_mode(mode).map_err(engine_error)
}

/// Parses a snake_case enum name sent by the UI; unknown names are rejected.
fn parse_enum<T: serde::de::DeserializeOwned>(value: &str, code: &str) -> CmdResult<T> {
    serde_json::from_value(serde_json::Value::String(value.into()))
        .map_err(|_| CommandError::new(code, format!("Unknown value: {value}")))
}

#[tauri::command]
async fn set_device_placement(
    id: String,
    position: String,
    distance_m: Option<f64>,
    state: State<'_, AppState>,
) -> CmdResult<()> {
    let snapshot = state.engine.snapshot();
    validate_endpoint_id(&id, &snapshot.endpoints)?;
    let position: SpeakerPosition = parse_enum(&position, "invalid_position")?;
    let distance = distance_m.map(validate_distance).transpose()?;
    state
        .engine
        .set_placement(id, position, distance)
        .map_err(engine_error)
}

#[tauri::command]
async fn set_device_role(id: String, role: String, state: State<'_, AppState>) -> CmdResult<()> {
    let snapshot = state.engine.snapshot();
    validate_endpoint_id(&id, &snapshot.endpoints)?;
    let role: ChannelRole = parse_enum(&role, "invalid_role")?;
    state.engine.set_role(id, role).map_err(engine_error)
}

#[tauri::command]
async fn set_device_trim(id: String, trim_db: f64, state: State<'_, AppState>) -> CmdResult<()> {
    let snapshot = state.engine.snapshot();
    validate_endpoint_id(&id, &snapshot.endpoints)?;
    let trim = validate_trim(trim_db)?;
    state.engine.set_trim(id, trim).map_err(engine_error)
}

#[tauri::command]
async fn set_distance_compensation(enabled: bool, state: State<'_, AppState>) -> CmdResult<()> {
    state
        .engine
        .set_distance_compensation(enabled)
        .map_err(engine_error)
}

#[tauri::command]
async fn apply_sound_preset(preset: String, state: State<'_, AppState>) -> CmdResult<()> {
    let preset: SoundPreset = parse_enum(&preset, "invalid_preset")?;
    state
        .engine
        .apply_preset(preset)
        .map_err(|e| CommandError::new("invalid_preset", e.to_string()))
}

/// Room-map drag: angle (0 = front, + = right) and optional distance.
#[tauri::command]
async fn set_device_map_placement(
    id: String,
    azimuth_deg: f64,
    distance_m: Option<f64>,
    state: State<'_, AppState>,
) -> CmdResult<()> {
    let snapshot = state.engine.snapshot();
    validate_endpoint_id(&id, &snapshot.endpoints)?;
    let azimuth = validate_azimuth(azimuth_deg)?;
    let distance = distance_m.map(validate_distance).transpose()?;
    state
        .engine
        .set_map_placement(id, azimuth, distance)
        .map_err(engine_error)
}

#[tauri::command]
async fn set_device_spread(
    id: String,
    spread: String,
    state: State<'_, AppState>,
) -> CmdResult<()> {
    let snapshot = state.engine.snapshot();
    validate_endpoint_id(&id, &snapshot.endpoints)?;
    let spread: StereoSpread = parse_enum(&spread, "invalid_spread")?;
    state.engine.set_spread(id, spread).map_err(engine_error)
}

#[tauri::command]
async fn set_device_subwoofer(id: String, on: bool, state: State<'_, AppState>) -> CmdResult<()> {
    let snapshot = state.engine.snapshot();
    validate_endpoint_id(&id, &snapshot.endpoints)?;
    state.engine.set_subwoofer(id, on).map_err(engine_error)
}

/// Spatial mode: room-map panning, stereo or 7.1 upmix, optional 8D rotation.
#[tauri::command]
async fn set_spatial(
    enabled: bool,
    layout: String,
    rotation_s: Option<f64>,
    clockwise: bool,
    state: State<'_, AppState>,
) -> CmdResult<()> {
    let layout: SourceLayout = parse_enum(&layout, "invalid_layout")?;
    let config = SpatialConfig {
        enabled,
        layout,
        rotation_s: rotation_s.map(|s| s as f32),
        clockwise,
    };
    validate_spatial(&config)?;
    state.engine.set_spatial(config).map_err(engine_error)
}

#[tauri::command]
async fn get_app_settings(state: State<'_, AppState>) -> CmdResult<system::AppSettings> {
    Ok(state.settings.lock().unwrap().clone())
}

/// Updates the user-editable settings (the virtual-speaker field is managed separately).
#[tauri::command]
async fn set_app_settings(
    restore_last_session: bool,
    auto_start_routing: bool,
    close_to_tray: bool,
    state: State<'_, AppState>,
) -> CmdResult<system::AppSettings> {
    let mut settings = state.settings.lock().unwrap();
    settings.restore_last_session = restore_last_session;
    settings.auto_start_routing = auto_start_routing;
    settings.close_to_tray = close_to_tray;
    system::save_settings(&state.data_dir, &settings)?;
    Ok(settings.clone())
}

#[tauri::command]
async fn virtual_speaker_status(
    state: State<'_, AppState>,
) -> CmdResult<system::VirtualSpeakerStatus> {
    Ok(system::virtual_speaker_status(&state))
}

#[tauri::command]
async fn enable_virtual_speaker(
    state: State<'_, AppState>,
) -> CmdResult<system::VirtualSpeakerStatus> {
    system::enable_virtual_speaker(&state)?;
    Ok(system::virtual_speaker_status(&state))
}

#[tauri::command]
async fn disable_virtual_speaker(
    state: State<'_, AppState>,
) -> CmdResult<system::VirtualSpeakerStatus> {
    system::disable_virtual_speaker(&state, true)?;
    Ok(system::virtual_speaker_status(&state))
}

/// Plays a frequency sweep through each playing output, measures it with the
/// microphone and stores a room-correction EQ (about 5.5 s per output).
#[tauri::command]
async fn measure_room(mic_id: String, state: State<'_, AppState>) -> CmdResult<RoomReport> {
    if mic_id.is_empty()
        || mic_id.len() > MAX_ENDPOINT_ID_LEN
        || mic_id.chars().any(char::is_control)
    {
        return Err(CommandError::new(
            "invalid_id",
            "Microphone ID is malformed",
        ));
    }
    state
        .engine
        .measure_room(mic_id)
        .map_err(|e| CommandError::new("calibration", e.to_string()))
}

#[tauri::command]
async fn list_microphones(state: State<'_, AppState>) -> CmdResult<Vec<Microphone>> {
    state.engine.list_microphones().map_err(engine_error)
}

/// Runs the click-track calibration (about 6 s per output) and applies delays.
#[tauri::command]
async fn calibrate(mic_id: String, state: State<'_, AppState>) -> CmdResult<CalibrationReport> {
    if mic_id.is_empty()
        || mic_id.len() > MAX_ENDPOINT_ID_LEN
        || mic_id.chars().any(char::is_control)
    {
        return Err(CommandError::new(
            "invalid_id",
            "Microphone ID is malformed",
        ));
    }
    state
        .engine
        .calibrate(mic_id)
        .map_err(|e| CommandError::new("calibration", e.to_string()))
}

#[tauri::command]
async fn list_audio_apps(state: State<'_, AppState>) -> CmdResult<Vec<AudioApp>> {
    state.engine.list_apps().map_err(engine_error)
}

/// Route only these applications (executable names, e.g. "Spotify.exe").
#[tauri::command]
async fn select_app_sources(exes: Vec<String>, state: State<'_, AppState>) -> CmdResult<()> {
    if exes.is_empty() || exes.len() > 8 {
        return Err(CommandError::new(
            "invalid_apps",
            "Choose between 1 and 8 apps",
        ));
    }
    for exe in &exes {
        let valid = (5..=260).contains(&exe.len())
            && exe.to_ascii_lowercase().ends_with(".exe")
            && !exe.contains(['\\', '/', ':'])
            && !exe.chars().any(char::is_control);
        if !valid {
            return Err(CommandError::new(
                "invalid_apps",
                format!("Not an app name: {exe}"),
            ));
        }
        if exe.eq_ignore_ascii_case("syncsound-desktop.exe") {
            return Err(CommandError::new(
                "invalid_apps",
                "SyncSound cannot route its own audio (that would loop back)",
            ));
        }
    }
    state
        .engine
        .set_source(CaptureSource::Apps(exes))
        .map_err(engine_error)
}

#[tauri::command]
async fn set_device_eq(id: String, eq: EqSettings, state: State<'_, AppState>) -> CmdResult<()> {
    let snapshot = state.engine.snapshot();
    validate_endpoint_id(&id, &snapshot.endpoints)?;
    eq.validate()?;
    state.engine.set_eq(id, eq).map_err(engine_error)
}

#[tauri::command]
async fn set_sound(
    settings: syncsound_engine::enhance::SoundSettings,
    state: State<'_, AppState>,
) -> CmdResult<()> {
    settings.validate()?;
    state.engine.set_sound(settings).map_err(engine_error)
}

#[tauri::command]
async fn apply_listening_mode(
    mode: syncsound_engine::enhance::ListeningMode,
    state: State<'_, AppState>,
) -> CmdResult<()> {
    state.engine.apply_listening_mode(mode).map_err(engine_error)
}

#[tauri::command]
async fn set_app_modes(
    modes: Vec<syncsound_engine::enhance::AppMode>,
    state: State<'_, AppState>,
) -> CmdResult<()> {
    syncsound_engine::enhance::validate_app_modes(&modes)?;
    if modes
        .iter()
        .any(|m| m.exe.eq_ignore_ascii_case("syncsound-desktop.exe"))
    {
        return Err(CommandError::new(
            "invalid_app_modes",
            "SyncSound itself can't switch the listening mode",
        ));
    }
    state.engine.set_app_modes(modes).map_err(engine_error)
}

#[tauri::command]
async fn get_spectrum(
    state: State<'_, AppState>,
) -> CmdResult<Vec<syncsound_engine::routing::OutputSpectrum>> {
    state.engine.spectrum().map_err(engine_error)
}

#[derive(Serialize)]
struct SyncMetrics {
    running: bool,
    mode: AudioMode,
    capture_state: CaptureState,
    outputs: Vec<OutputStatus>,
    updated_at_ms: u64,
}

/// Served from the engine's throttled (~100 ms) snapshot; never blocks the audio path.
#[tauri::command]
async fn get_sync_metrics(state: State<'_, AppState>) -> CmdResult<SyncMetrics> {
    let s = state.engine.snapshot();
    Ok(SyncMetrics {
        running: s.running,
        mode: s.mode,
        capture_state: s.capture_state,
        outputs: s.outputs,
        updated_at_ms: s.updated_at_ms,
    })
}

#[tauri::command]
async fn save_profile(
    name: String,
    app: tauri::AppHandle,
    state: State<'_, AppState>,
) -> CmdResult<String> {
    if name == system::LAST_SESSION_NAME {
        return Err(CommandError::new(
            "invalid_profile_name",
            "That name is reserved",
        ));
    }
    let profile = Profile::from_snapshot(&name, &state.engine.snapshot())?;
    let path = state.profiles.save(&profile)?;
    system::refresh_tray(&app);
    Ok(path.display().to_string())
}

#[tauri::command]
async fn list_profiles(state: State<'_, AppState>) -> CmdResult<Vec<String>> {
    Ok(state.profiles.list())
}

#[derive(Serialize)]
struct ProfileLoadReport {
    name: String,
    mode: AudioMode,
    applied_outputs: usize,
    missing: Vec<MissingOutput>,
    missing_source: Option<String>,
}

#[tauri::command]
async fn load_profile(
    name: String,
    app: tauri::AppHandle,
    state: State<'_, AppState>,
) -> CmdResult<ProfileLoadReport> {
    let profile = state.profiles.load(&name)?;
    let report = apply_profile(&state.engine, &profile)?;
    system::refresh_tray(&app);
    Ok(report)
}

/// Explicit, user-confirmed rebind of a missing saved device to a present endpoint.
#[tauri::command]
async fn rebind_profile_device(
    name: String,
    from_id: String,
    to_id: String,
    state: State<'_, AppState>,
) -> CmdResult<()> {
    let snapshot = state.engine.snapshot();
    let target = validate_endpoint_id(&to_id, &snapshot.endpoints)?.clone();
    let mut profile = state.profiles.load(&name)?;
    profile.rebind(&from_id, &target)?;
    state.profiles.save(&profile)?;
    Ok(())
}

#[tauri::command]
async fn export_diagnostics(
    app: tauri::AppHandle,
    state: State<'_, AppState>,
) -> CmdResult<String> {
    let snapshot = state.engine.snapshot();
    let version = app.package_info().version.to_string();
    let bundle = diagnostics::build_bundle(&snapshot, &version, std::env::consts::OS);
    let io = |e: std::io::Error| CommandError::new("diagnostics_io", e.to_string());
    std::fs::create_dir_all(&state.diagnostics_dir).map_err(io)?;
    let stamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let path = state
        .diagnostics_dir
        .join(format!("syncsound-diagnostics-{stamp}.json"));
    let text = serde_json::to_string_pretty(&bundle)
        .map_err(|e| CommandError::new("diagnostics_io", e.to_string()))?;
    std::fs::write(&path, text).map_err(io)?;
    Ok(path.display().to_string())
}

pub fn run() {
    let app = tauri::Builder::default()
        // Must be registered first. A second launch would start a second engine
        // mirroring the same audio, so it focuses the existing window and exits.
        .plugin(tauri_plugin_single_instance::init(|app, _args, _cwd| {
            if let Some(window) = app.get_webview_window("main") {
                let _ = window.unminimize();
                let _ = window.show();
                let _ = window.set_focus();
            }
        }))
        // Signed auto-update from GitHub Releases (public key in tauri.conf.json).
        .plugin(tauri_plugin_updater::Builder::new().build())
        .plugin(tauri_plugin_process::init())
        // Start with Windows (toggled from the UI); launched hidden in the tray.
        .plugin(tauri_plugin_autostart::init(
            tauri_plugin_autostart::MacosLauncher::LaunchAgent,
            Some(vec!["--minimized"]),
        ))
        .setup(|app| {
            // Profiles, settings and diagnostics live in per-user app data, never the install dir.
            let data = app.path().app_data_dir()?;
            app.manage(AppState {
                engine: AudioEngine::spawn(),
                profiles: ProfileStore::new(data.join("profiles")),
                diagnostics_dir: data.join("diagnostics"),
                settings: std::sync::Mutex::new(system::load_settings(&data)),
                data_dir: data,
                quitting: std::sync::atomic::AtomicBool::new(false),
            });
            if std::env::args().any(|a| a == "--minimized") {
                if let Some(window) = app.get_webview_window("main") {
                    let _ = window.hide();
                }
            }
            system::create_tray(app.handle())?;
            // Restore off the UI thread; the engine call can take a moment.
            let handle = app.handle().clone();
            std::thread::spawn(move || {
                if let Some(state) = handle.try_state::<AppState>() {
                    system::restore_last_session(&state);
                }
                system::refresh_tray(&handle);
            });
            system::spawn_session_saver(app.handle().clone());
            Ok(())
        })
        .on_window_event(|window, event| {
            if let tauri::WindowEvent::CloseRequested { api, .. } = event {
                let app = window.app_handle();
                if let Some(state) = app.try_state::<AppState>() {
                    let to_tray = state.settings.lock().unwrap().close_to_tray;
                    if to_tray && !state.quitting.load(std::sync::atomic::Ordering::Acquire) {
                        api.prevent_close();
                        let _ = window.hide();
                    }
                }
            }
        })
        .invoke_handler(tauri::generate_handler![
            get_audio_devices,
            get_engine_status,
            start_audio_engine,
            stop_audio_engine,
            select_capture_source,
            enable_device,
            disable_device,
            set_device_volume,
            set_device_mute,
            set_device_delay,
            set_audio_mode,
            get_sync_metrics,
            save_profile,
            load_profile,
            list_profiles,
            rebind_profile_device,
            export_diagnostics,
            set_device_placement,
            set_device_role,
            set_device_trim,
            set_distance_compensation,
            apply_sound_preset,
            set_device_map_placement,
            set_device_spread,
            set_device_subwoofer,
            set_spatial,
            set_device_eq,
            set_sound,
            apply_listening_mode,
            set_app_modes,
            get_spectrum,
            list_audio_apps,
            select_app_sources,
            list_microphones,
            calibrate,
            measure_room,
            get_app_settings,
            set_app_settings,
            virtual_speaker_status,
            enable_virtual_speaker,
            disable_virtual_speaker
        ])
        .build(tauri::generate_context!())
        .expect("error while building SyncSound");
    app.run(|handle, event| {
        if let tauri::RunEvent::Exit = event {
            if let Some(state) = handle.try_state::<AppState>() {
                system::save_last_session(&state);
                // Never leave Windows pointed at a silent virtual cable.
                let _ = system::disable_virtual_speaker(&state, false);
                let _ = state.engine.stop();
            }
        }
    });
}

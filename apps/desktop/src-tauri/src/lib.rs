//! Tauri control plane. Every command validates untrusted input against the
//! latest engine snapshot before touching the engine; rejected input changes nothing.
use serde::Serialize;
use std::path::PathBuf;
use syncsound_engine::capture::{AudioEndpoint, CaptureState};
use syncsound_engine::commands::{
    parse_mode, parse_source, validate_delay, validate_enable, validate_endpoint_id,
    validate_volume, CommandError,
};
use syncsound_engine::profile::{MissingOutput, Profile, ProfileStore};
use syncsound_engine::render::AudioMode;
use syncsound_engine::{diagnostics, AudioEngine, EngineError, EngineSnapshot, OutputStatus};
use tauri::{Manager, State};

struct AppState {
    engine: AudioEngine,
    profiles: ProfileStore,
    diagnostics_dir: PathBuf,
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
async fn start_audio_engine(state: State<'_, AppState>) -> CmdResult<EngineSnapshot> {
    state.engine.start().map_err(engine_error)?;
    Ok(state.engine.snapshot())
}

#[tauri::command]
async fn stop_audio_engine(state: State<'_, AppState>) -> CmdResult<EngineSnapshot> {
    state.engine.stop().map_err(engine_error)?;
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
async fn save_profile(name: String, state: State<'_, AppState>) -> CmdResult<String> {
    let profile = Profile::from_snapshot(&name, &state.engine.snapshot())?;
    let path = state.profiles.save(&profile)?;
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
async fn load_profile(name: String, state: State<'_, AppState>) -> CmdResult<ProfileLoadReport> {
    let profile = state.profiles.load(&name)?;
    let endpoints = state.engine.refresh_endpoints().map_err(engine_error)?;
    let plan = profile.plan_apply(&endpoints);
    state.engine.set_mode(plan.mode).map_err(engine_error)?;
    if let Some(source) = plan.source.clone() {
        state.engine.set_source(source).map_err(engine_error)?;
    }
    let applied_outputs = plan.outputs.len();
    state
        .engine
        .apply_outputs(plan.outputs)
        .map_err(engine_error)?;
    Ok(ProfileLoadReport {
        name: profile.name,
        mode: plan.mode,
        applied_outputs,
        missing: plan.missing,
        missing_source: plan.missing_source,
    })
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
        .setup(|app| {
            // Profiles and diagnostics live in per-user app data, never the install dir.
            let data = app.path().app_data_dir()?;
            app.manage(AppState {
                engine: AudioEngine::spawn(),
                profiles: ProfileStore::new(data.join("profiles")),
                diagnostics_dir: data.join("diagnostics"),
            });
            Ok(())
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
            export_diagnostics
        ])
        .build(tauri::generate_context!())
        .expect("error while building SyncSound");
    app.run(|handle, event| {
        if let tauri::RunEvent::Exit = event {
            if let Some(state) = handle.try_state::<AppState>() {
                let _ = state.engine.stop();
            }
        }
    });
}

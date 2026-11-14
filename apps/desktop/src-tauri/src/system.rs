//! App-level behaviour: saved settings, restoring the last session, the tray
//! menu, start-with-Windows, and the VB-CABLE virtual speaker.
use crate::{apply_profile, AppState};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use std::sync::atomic::Ordering;
use std::time::Duration;
use syncsound_engine::capture::{CaptureSource, EndpointState};
use syncsound_engine::commands::CommandError;
use syncsound_engine::platform::windows_default::{
    default_render_id, is_virtual_cable, set_default_render,
};
use syncsound_engine::profile::{Profile, PROFILE_SCHEMA_VERSION};
use tauri::menu::{Menu, MenuItem, PredefinedMenuItem, Submenu};
use tauri::tray::{MouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent};
use tauri::{AppHandle, Manager};

pub const LAST_SESSION_NAME: &str = "Last session";
const TRAY_ID: &str = "syncsound";

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct AppSettings {
    /// Reload the setup that was active when SyncSound last closed.
    pub restore_last_session: bool,
    /// Also press Start routing after restoring.
    pub auto_start_routing: bool,
    /// The window's close button hides SyncSound to the tray instead of quitting.
    pub close_to_tray: bool,
    /// Present while the virtual speaker mode is on.
    pub virtual_speaker: Option<VirtualSpeaker>,
    /// System-wide Ctrl+Alt shortcuts (routing, A/B, listening mode, undo).
    pub hotkeys: bool,
}

impl Default for AppSettings {
    fn default() -> Self {
        Self {
            restore_last_session: true,
            auto_start_routing: false,
            close_to_tray: true,
            virtual_speaker: None,
            hotkeys: true,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct VirtualSpeaker {
    pub cable_id: String,
    /// Windows default before the switch; restored when the mode is turned off
    /// or SyncSound quits.
    pub previous_default: Option<String>,
}

pub fn load_settings(dir: &Path) -> AppSettings {
    std::fs::read_to_string(dir.join("settings.json"))
        .ok()
        .and_then(|t| serde_json::from_str(&t).ok())
        .unwrap_or_default()
}

pub fn save_settings(dir: &Path, settings: &AppSettings) -> Result<(), CommandError> {
    let io = |e: std::io::Error| CommandError::new("settings_io", e.to_string());
    std::fs::create_dir_all(dir).map_err(io)?;
    let tmp = dir.join("settings.json.tmp");
    std::fs::write(&tmp, serde_json::to_string_pretty(settings).unwrap()).map_err(io)?;
    std::fs::rename(&tmp, dir.join("settings.json")).map_err(io)
}

fn last_session_path(dir: &Path) -> PathBuf {
    dir.join("last-session.json")
}

/// Writes the current setup to `last-session.json` (not shown as a profile).
pub fn save_last_session(state: &AppState) {
    let snapshot = state.engine.snapshot();
    if snapshot.outputs.is_empty() {
        return; // nothing configured yet: keep the previous session
    }
    if let Ok(profile) = Profile::from_snapshot(LAST_SESSION_NAME, &snapshot) {
        let path = last_session_path(&state.data_dir);
        let tmp = path.with_extension("json.tmp");
        if std::fs::write(&tmp, profile.to_json()).is_ok() {
            let _ = std::fs::rename(tmp, path);
        }
    }
}

/// Restores the last session (and optionally starts routing) on launch.
pub fn restore_last_session(state: &AppState) {
    let settings = state.settings.lock().unwrap().clone();
    if settings.restore_last_session {
        if let Ok(text) = std::fs::read_to_string(last_session_path(&state.data_dir)) {
            if let Ok(profile) = Profile::from_json(&text) {
                if profile.schema_version == PROFILE_SCHEMA_VERSION {
                    let _ = apply_profile(&state.engine, &profile);
                }
            }
        }
    }
    // Virtual speaker was on when SyncSound last closed: switch back to it.
    // enable_virtual_speaker also starts routing, because with Windows playing
    // into the cable nothing is audible until SyncSound routes it.
    if settings.virtual_speaker.is_some() {
        let _ = enable_virtual_speaker(state);
    } else if settings.auto_start_routing {
        let _ = state.engine.start();
    }
}

/// Saves the session every 30 s so a crash or power cut loses little.
pub fn spawn_session_saver(app: AppHandle) {
    std::thread::Builder::new()
        .name("syncsound-session".into())
        .spawn(move || loop {
            std::thread::sleep(Duration::from_secs(30));
            if let Some(state) = app.try_state::<AppState>() {
                save_last_session(&state);
            }
        })
        .ok();
}

// ---------- Virtual speaker (VB-CABLE) ----------

#[derive(Clone, Debug, Serialize)]
pub struct VirtualSpeakerStatus {
    /// VB-CABLE's "CABLE Input" playback device is present and active.
    pub installed: bool,
    pub cable_name: Option<String>,
    pub active: bool,
    /// The Windows default is currently the cable.
    pub default_is_cable: bool,
    pub previous_default_name: Option<String>,
}

/// Where Windows should go when the virtual speaker is turned off: the saved
/// previous default, or else a real speaker (an enabled wired output first, then
/// any enabled output, then any active device). The VB-CABLE installer makes the
/// cable the default itself, so there often is no "previous" one.
fn restore_target(state: &AppState, saved: &VirtualSpeaker) -> Option<(String, String)> {
    let snapshot = state.engine.snapshot();
    let active = |id: &str| {
        snapshot
            .endpoints
            .iter()
            .find(|e| e.id == id && e.state == EndpointState::Active && !is_virtual_cable(&e.name))
            .map(|e| (e.id.clone(), e.name.clone()))
    };
    if let Some(found) = saved.previous_default.as_deref().and_then(active) {
        return Some(found);
    }
    let enabled: Vec<_> = snapshot
        .outputs
        .iter()
        .filter(|o| o.config.enabled)
        .collect();
    enabled
        .iter()
        .filter(|o| !o.bluetooth)
        .chain(enabled.iter())
        .find_map(|o| active(&o.endpoint_id))
        .or_else(|| {
            snapshot
                .endpoints
                .iter()
                .find(|e| e.state == EndpointState::Active && !is_virtual_cable(&e.name))
                .map(|e| (e.id.clone(), e.name.clone()))
        })
}

fn find_cable(state: &AppState) -> Option<(String, String)> {
    state
        .engine
        .refresh_endpoints()
        .ok()?
        .into_iter()
        .find(|e| e.state == EndpointState::Active && is_virtual_cable(&e.name))
        .map(|e| (e.id, e.name))
}

pub fn virtual_speaker_status(state: &AppState) -> VirtualSpeakerStatus {
    let cable = find_cable(state);
    let settings = state.settings.lock().unwrap().clone();
    let default = default_render_id().ok().flatten();
    VirtualSpeakerStatus {
        installed: cable.is_some(),
        default_is_cable: cable
            .as_ref()
            .is_some_and(|(id, _)| default.as_deref() == Some(id)),
        cable_name: cable.map(|(_, n)| n),
        active: settings.virtual_speaker.is_some(),
        previous_default_name: settings
            .virtual_speaker
            .as_ref()
            .and_then(|v| restore_target(state, v))
            .map(|(_, name)| name),
    }
}

/// Makes VB-CABLE the Windows default and captures it, so every real speaker
/// (including the one that used to be the default) can be delayed and aligned.
pub fn enable_virtual_speaker(state: &AppState) -> Result<(), CommandError> {
    let fail = |m: &str| CommandError::new("virtual_speaker", m);
    let (cable_id, _) = find_cable(state).ok_or_else(|| {
        fail("VB-CABLE is not installed. Install it from vb-audio.com/Cable, then restart SyncSound.")
    })?;
    let current = default_render_id().map_err(|e| fail(&e.to_string()))?;
    let previous = {
        let settings = state.settings.lock().unwrap();
        match &settings.virtual_speaker {
            // Already on (e.g. restored after a restart): keep the original default.
            Some(v) => v.previous_default.clone(),
            None => current.clone().filter(|id| *id != cable_id),
        }
    };
    // Capture the cable before routing to it, so nothing plays twice.
    state
        .engine
        .set_source(CaptureSource::Endpoint(cable_id.clone()))
        .map_err(|e| fail(&e.to_string()))?;
    if current.as_deref() != Some(cable_id.as_str()) {
        set_default_render(&cable_id)
            .map_err(|e| fail(&format!("Could not change the Windows default device: {e}")))?;
    }
    {
        let mut settings = state.settings.lock().unwrap();
        settings.virtual_speaker = Some(VirtualSpeaker {
            cable_id,
            previous_default: previous,
        });
        save_settings(&state.data_dir, &settings)?;
    }
    // Windows now plays into the silent cable: route it, or there is no sound.
    state.engine.start().map_err(|e| {
        fail(&format!(
            "Virtual speaker is on but routing did not start: {e}"
        ))
    })
}

/// Restores the previous Windows default. `forget` = false keeps the mode
/// remembered so it turns back on at the next launch (used when quitting).
pub fn disable_virtual_speaker(state: &AppState, forget: bool) -> Result<(), CommandError> {
    let fail = |m: &str| CommandError::new("virtual_speaker", m);
    let saved = state.settings.lock().unwrap().virtual_speaker.clone();
    let Some(saved) = saved else {
        return Ok(());
    };
    if let Some((target, _)) = restore_target(state, &saved) {
        set_default_render(&target).map_err(|e| {
            fail(&format!(
                "Could not restore the Windows default device: {e}"
            ))
        })?;
    }
    if forget {
        let _ = state.engine.set_source(CaptureSource::DefaultRender);
        let mut settings = state.settings.lock().unwrap();
        settings.virtual_speaker = None;
        save_settings(&state.data_dir, &settings)?;
    }
    Ok(())
}

// ---------- Tray ----------

fn show_main(app: &AppHandle) {
    if let Some(window) = app.get_webview_window("main") {
        let _ = window.unminimize();
        let _ = window.show();
        let _ = window.set_focus();
    }
}

fn build_menu(app: &AppHandle) -> tauri::Result<Menu<tauri::Wry>> {
    let state = app.state::<AppState>();
    let running = state.engine.snapshot().running;
    let show = MenuItem::with_id(app, "show", "Show SyncSound", true, None::<&str>)?;
    let toggle = MenuItem::with_id(
        app,
        "toggle",
        if running {
            "Stop routing"
        } else {
            "Start routing"
        },
        true,
        None::<&str>,
    )?;
    let names = state.profiles.list();
    let profiles = Submenu::with_id(app, "profiles", "Load profile", !names.is_empty())?;
    for name in names {
        profiles.append(&MenuItem::with_id(
            app,
            format!("profile:{name}"),
            &name,
            true,
            None::<&str>,
        )?)?;
    }
    let quit = MenuItem::with_id(app, "quit", "Quit SyncSound", true, None::<&str>)?;
    Menu::with_items(
        app,
        &[
            &show,
            &PredefinedMenuItem::separator(app)?,
            &toggle,
            &profiles,
            &PredefinedMenuItem::separator(app)?,
            &quit,
        ],
    )
}

/// Rebuilds the tray menu (routing state or profile list changed).
pub fn refresh_tray(app: &AppHandle) {
    if let (Some(tray), Ok(menu)) = (app.tray_by_id(TRAY_ID), build_menu(app)) {
        let _ = tray.set_menu(Some(menu));
        let running = app.state::<AppState>().engine.snapshot().running;
        let _ = tray.set_tooltip(Some(if running {
            "SyncSound: routing"
        } else {
            "SyncSound: stopped"
        }));
    }
}

pub fn create_tray(app: &AppHandle) -> tauri::Result<()> {
    let menu = build_menu(app)?;
    TrayIconBuilder::with_id(TRAY_ID)
        .icon(app.default_window_icon().cloned().expect("app icon"))
        .tooltip("SyncSound")
        .menu(&menu)
        .show_menu_on_left_click(false)
        .on_tray_icon_event(|tray, event| {
            if let TrayIconEvent::Click {
                button: MouseButton::Left,
                button_state: MouseButtonState::Up,
                ..
            } = event
            {
                show_main(tray.app_handle());
            }
        })
        .on_menu_event(|app, event| {
            let id = event.id().as_ref().to_string();
            let state = app.state::<AppState>();
            match id.as_str() {
                "show" => show_main(app),
                "toggle" => {
                    let _ = if state.engine.snapshot().running {
                        state.engine.stop()
                    } else {
                        state.engine.start()
                    };
                }
                "quit" => {
                    state.quitting.store(true, Ordering::Release);
                    app.exit(0);
                }
                other => {
                    if let Some(name) = other.strip_prefix("profile:") {
                        if let Ok(profile) = state.profiles.load(name) {
                            let _ = apply_profile(&state.engine, &profile);
                        }
                    }
                }
            }
            refresh_tray(app);
        })
        .build(app)?;
    Ok(())
}

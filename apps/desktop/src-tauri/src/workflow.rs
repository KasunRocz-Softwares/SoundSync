//! Workflow tools: undo / redo, A/B snapshots, settings export / import, and
//! global hotkeys. Every state is a full `Profile` of the engine, so restoring
//! one goes through the same validated path as loading a saved profile.
use crate::{apply_profile, engine_error, AppState, CmdResult, ProfileLoadReport};
use serde::Serialize;
use syncsound_engine::commands::CommandError;
use syncsound_engine::enhance::ListeningMode;
use syncsound_engine::profile::Profile;
use tauri::{AppHandle, Manager, State};
use tauri_plugin_global_shortcut::{
    Code, GlobalShortcutExt, Modifiers, Shortcut, ShortcutState,
};

const HISTORY_LIMIT: usize = 50;
const STATE_NAME: &str = "Undo";

#[derive(Default)]
pub struct Workflow {
    undo: Vec<Profile>,
    redo: Vec<Profile>,
    ab: [Option<Profile>; 2],
    /// The A/B slot last recalled, until something else changes.
    active: Option<usize>,
}

#[derive(Clone, Serialize)]
pub struct WorkflowStatus {
    can_undo: bool,
    can_redo: bool,
    ab_stored: [bool; 2],
    ab_active: Option<usize>,
}

impl Workflow {
    fn status(&self) -> WorkflowStatus {
        WorkflowStatus {
            can_undo: !self.undo.is_empty(),
            can_redo: !self.redo.is_empty(),
            ab_stored: [self.ab[0].is_some(), self.ab[1].is_some()],
            ab_active: self.active,
        }
    }
}

fn current(state: &AppState) -> CmdResult<Profile> {
    Profile::from_snapshot(STATE_NAME, &state.engine.snapshot())
}

/// Remembers the current setup before a change.
pub fn checkpoint_state(state: &AppState) -> CmdResult<WorkflowStatus> {
    let now = current(state)?;
    let mut wf = state.workflow.lock().unwrap();
    if wf.undo.last() != Some(&now) {
        wf.undo.push(now);
        if wf.undo.len() > HISTORY_LIMIT {
            wf.undo.remove(0);
        }
    }
    wf.redo.clear();
    wf.active = None;
    Ok(wf.status())
}

fn step(state: &AppState, back: bool) -> CmdResult<WorkflowStatus> {
    let now = current(state)?;
    let target = {
        let mut wf = state.workflow.lock().unwrap();
        let from = if back { &mut wf.undo } else { &mut wf.redo };
        // Skip entries identical to now (a change that was rejected, say).
        let mut target = None;
        while let Some(p) = from.pop() {
            if p != now {
                target = Some(p);
                break;
            }
        }
        let Some(target) = target else {
            return Ok(wf.status());
        };
        let to = if back { &mut wf.redo } else { &mut wf.undo };
        to.push(now);
        wf.active = None;
        target
    };
    apply_profile(&state.engine, &target)?;
    Ok(state.workflow.lock().unwrap().status())
}

pub fn recall_ab(state: &AppState, slot: usize) -> CmdResult<WorkflowStatus> {
    let profile = state.workflow.lock().unwrap().ab[slot].clone();
    let Some(profile) = profile else {
        return Err(CommandError::new(
            "ab_empty",
            format!("Nothing is stored in {} yet", ["A", "B"][slot]),
        ));
    };
    checkpoint_state(state)?;
    apply_profile(&state.engine, &profile)?;
    let mut wf = state.workflow.lock().unwrap();
    wf.active = Some(slot);
    Ok(wf.status())
}

fn slot_index(slot: u8) -> CmdResult<usize> {
    match slot {
        0 | 1 => Ok(usize::from(slot)),
        _ => Err(CommandError::new("invalid_slot", "Slot must be A or B")),
    }
}

#[tauri::command]
pub async fn workflow_status(state: State<'_, AppState>) -> CmdResult<WorkflowStatus> {
    Ok(state.workflow.lock().unwrap().status())
}

#[tauri::command]
pub async fn checkpoint(state: State<'_, AppState>) -> CmdResult<WorkflowStatus> {
    checkpoint_state(&state)
}

#[tauri::command]
pub async fn undo(state: State<'_, AppState>) -> CmdResult<WorkflowStatus> {
    step(&state, true)
}

#[tauri::command]
pub async fn redo(state: State<'_, AppState>) -> CmdResult<WorkflowStatus> {
    step(&state, false)
}

#[tauri::command]
pub async fn ab_store(slot: u8, state: State<'_, AppState>) -> CmdResult<WorkflowStatus> {
    let slot = slot_index(slot)?;
    let now = current(&state)?;
    let mut wf = state.workflow.lock().unwrap();
    wf.ab[slot] = Some(now);
    wf.active = Some(slot);
    Ok(wf.status())
}

#[tauri::command]
pub async fn ab_recall(slot: u8, state: State<'_, AppState>) -> CmdResult<WorkflowStatus> {
    recall_ab(&state, slot_index(slot)?)
}

/// Writes the current setup to the Downloads folder and returns the file path.
#[tauri::command]
pub async fn export_settings(app: AppHandle, state: State<'_, AppState>) -> CmdResult<String> {
    let io = |e: String| CommandError::new("export_io", e);
    let profile = Profile::from_snapshot("Exported settings", &state.engine.snapshot())?;
    let dir = app
        .path()
        .download_dir()
        .or_else(|_| app.path().document_dir())
        .map_err(|e| io(e.to_string()))?;
    let stamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let path = dir.join(format!("SyncSound-settings-{stamp}.json"));
    std::fs::write(&path, profile.to_json()).map_err(|e| io(e.to_string()))?;
    Ok(path.display().to_string())
}

const MAX_IMPORT_BYTES: usize = 256 * 1024;

/// Applies settings from an exported file's text (read by the UI's file picker).
#[tauri::command]
pub async fn import_settings(
    text: String,
    state: State<'_, AppState>,
) -> CmdResult<ProfileLoadReport> {
    if text.len() > MAX_IMPORT_BYTES {
        return Err(CommandError::new("invalid_profile", "File is too large"));
    }
    let profile = Profile::from_json(&text)?;
    checkpoint_state(&state)?;
    apply_profile(&state.engine, &profile)
}

// --- Global hotkeys -------------------------------------------------------

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Hotkey {
    ToggleRouting,
    RecallA,
    RecallB,
    NextMode,
    Undo,
}

const HOTKEYS: [(Hotkey, Code, &str); 5] = [
    (Hotkey::ToggleRouting, Code::KeyS, "Ctrl+Alt+S: start or stop routing"),
    (Hotkey::RecallA, Code::KeyA, "Ctrl+Alt+A: switch to A"),
    (Hotkey::RecallB, Code::KeyB, "Ctrl+Alt+B: switch to B"),
    (Hotkey::NextMode, Code::KeyL, "Ctrl+Alt+L: next listening mode"),
    (Hotkey::Undo, Code::KeyZ, "Ctrl+Alt+Z: undo"),
];

fn shortcut(code: Code) -> Shortcut {
    Shortcut::new(Some(Modifiers::CONTROL | Modifiers::ALT), code)
}

pub fn hotkey_help() -> Vec<&'static str> {
    HOTKEYS.iter().map(|(_, _, help)| *help).collect()
}

pub fn hotkey_plugin() -> tauri::plugin::TauriPlugin<tauri::Wry> {
    tauri_plugin_global_shortcut::Builder::new()
        .with_handler(|app, pressed, event| {
            if event.state() != ShortcutState::Pressed {
                return;
            }
            let Some((key, _, _)) = HOTKEYS.iter().find(|(_, code, _)| shortcut(*code) == *pressed)
            else {
                return;
            };
            let app = app.clone();
            let key = *key;
            // Engine calls can block briefly: never on the hotkey thread.
            std::thread::spawn(move || {
                if let Some(state) = app.try_state::<AppState>() {
                    let _ = run_hotkey(&state, key);
                }
            });
        })
        .build()
}

fn run_hotkey(state: &AppState, key: Hotkey) -> CmdResult<()> {
    match key {
        Hotkey::ToggleRouting => {
            if state.engine.snapshot().running {
                state.engine.stop().map_err(engine_error)
            } else {
                state.engine.start().map_err(engine_error)
            }
        }
        Hotkey::RecallA => recall_ab(state, 0).map(|_| ()),
        Hotkey::RecallB => recall_ab(state, 1).map(|_| ()),
        Hotkey::NextMode => {
            let now = state.engine.snapshot().listening_mode;
            let modes = ListeningMode::ALL;
            let next = modes
                .iter()
                .position(|m| *m == now)
                .map_or(modes[0], |i| modes[(i + 1) % modes.len()]);
            checkpoint_state(state)?;
            state
                .engine
                .apply_listening_mode(next)
                .map_err(engine_error)
        }
        Hotkey::Undo => step(state, true).map(|_| ()),
    }
}

/// Registers or removes the global hotkeys. Returns the ones Windows refused
/// (already taken by another app).
pub fn set_hotkeys(app: &AppHandle, on: bool) -> Vec<&'static str> {
    let gs = app.global_shortcut();
    let mut refused = Vec::new();
    for (_, code, help) in HOTKEYS {
        let s = shortcut(code);
        if on {
            if !gs.is_registered(s) && gs.register(s).is_err() {
                refused.push(help);
            }
        } else if gs.is_registered(s) {
            let _ = gs.unregister(s);
        }
    }
    refused
}

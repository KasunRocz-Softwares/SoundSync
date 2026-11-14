//! Workflow tools: undo / redo, A/B snapshots, settings export / import, and
//! global hotkeys. Every state is a full `Profile` of the engine, so restoring
//! one goes through the same validated path as loading a saved profile.
use crate::{apply_profile, engine_error, AppState, CmdResult, ProfileLoadReport};
use serde::{Deserialize, Serialize};
use syncsound_engine::commands::CommandError;
use syncsound_engine::enhance::ListeningMode;
use syncsound_engine::profile::Profile;
use tauri::{AppHandle, Manager, State};
use tauri_plugin_global_shortcut::{Code, GlobalShortcutExt, Modifiers, Shortcut, ShortcutState};

const HISTORY_LIMIT: usize = 50;
const STATE_NAME: &str = "Undo";

#[derive(Default)]
pub struct Workflow {
    undo: Vec<Profile>,
    redo: Vec<Profile>,
    ab: [Option<Profile>; 2],
    /// The A/B slot last recalled, until something else changes.
    active: Option<usize>,
    /// The setup as of the last commit. A change is recorded *after* it lands
    /// by pushing this baseline, so the UI never has to wait before changing.
    baseline: Option<Profile>,
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

/// Records a change that just happened: the previous baseline goes on the
/// undo stack and the current setup becomes the new baseline.
pub fn commit_state(state: &AppState) -> CmdResult<WorkflowStatus> {
    let now = current(state)?;
    let mut wf = state.workflow.lock().unwrap();
    match wf.baseline.take() {
        Some(before) if before != now => {
            wf.undo.push(before);
            if wf.undo.len() > HISTORY_LIMIT {
                wf.undo.remove(0);
            }
            wf.redo.clear();
            wf.active = None;
        }
        _ => {}
    }
    wf.baseline = Some(now);
    Ok(wf.status())
}

/// Sets the baseline to the current setup without recording a step.
fn rebase(state: &AppState) -> CmdResult<()> {
    let now = current(state)?;
    state.workflow.lock().unwrap().baseline = Some(now);
    Ok(())
}

fn step(state: &AppState, back: bool) -> CmdResult<WorkflowStatus> {
    // Anything changed outside the UI's normal path counts as one step first.
    commit_state(state)?;
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
    rebase(state)?;
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
    commit_state(state)?;
    apply_profile(&state.engine, &profile)?;
    commit_state(state)?;
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
    // The first poll sets the baseline the first change is measured against.
    if state.workflow.lock().unwrap().baseline.is_none() {
        rebase(&state)?;
    }
    Ok(state.workflow.lock().unwrap().status())
}

/// Called by the UI after each change it makes.
#[tauri::command]
pub async fn checkpoint(state: State<'_, AppState>) -> CmdResult<WorkflowStatus> {
    commit_state(&state)
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
    commit_state(&state)?;
    let report = apply_profile(&state.engine, &profile)?;
    commit_state(&state)?;
    Ok(report)
}

// --- Global hotkeys -------------------------------------------------------

/// One global shortcut: an action id and the keys that run it ("" = off).
/// Actions: `toggle_routing`, `recall_a`, `recall_b`, `next_mode`, `undo`,
/// `redo`, and per output `mute:<id>`, `louder:<id>`, `quieter:<id>`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct HotkeyBinding {
    pub action: String,
    pub keys: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
enum Action {
    ToggleRouting,
    RecallA,
    RecallB,
    NextMode,
    Undo,
    Redo,
    Mute(String),
    Louder(String),
    Quieter(String),
}

impl Action {
    fn parse(action: &str) -> Option<Action> {
        let output = |prefix: &str| {
            action
                .strip_prefix(prefix)
                .filter(|id| !id.is_empty() && id.len() <= 512 && !id.chars().any(char::is_control))
                .map(str::to_string)
        };
        Some(match action {
            "toggle_routing" => Action::ToggleRouting,
            "recall_a" => Action::RecallA,
            "recall_b" => Action::RecallB,
            "next_mode" => Action::NextMode,
            "undo" => Action::Undo,
            "redo" => Action::Redo,
            _ => {
                if let Some(id) = output("mute:") {
                    Action::Mute(id)
                } else if let Some(id) = output("louder:") {
                    Action::Louder(id)
                } else {
                    Action::Quieter(output("quieter:")?)
                }
            }
        })
    }
}

/// App-wide actions, in display order, with their default keys.
pub const GLOBAL_ACTIONS: [(&str, &str); 6] = [
    ("toggle_routing", "Ctrl+Alt+S"),
    ("recall_a", "Ctrl+Alt+A"),
    ("recall_b", "Ctrl+Alt+B"),
    ("next_mode", "Ctrl+Alt+L"),
    ("undo", "Ctrl+Alt+Z"),
    ("redo", ""),
];

pub fn default_bindings() -> Vec<HotkeyBinding> {
    GLOBAL_ACTIONS
        .iter()
        .map(|(action, keys)| HotkeyBinding {
            action: (*action).into(),
            keys: (*keys).into(),
        })
        .collect()
}

/// Per-output bindings are capped so a settings file can't register hundreds.
const MAX_BINDINGS: usize = 64;

/// Parses keys typed or recorded in the UI ("Ctrl+Alt+S", "Ctrl+Shift+Up",
/// "F9"). A bare key is allowed only for F1–F24, so typing never triggers one.
pub fn parse_keys(keys: &str) -> Result<Shortcut, CommandError> {
    let invalid = |why: &str| CommandError::new("invalid_hotkey", why);
    if keys.len() > 64 {
        return Err(invalid("That shortcut is too long"));
    }
    let shortcut: Shortcut = keys.parse().map_err(|_| {
        invalid("SyncSound can't use that key; try a letter, number, arrow or F-key")
    })?;
    let function_key = matches!(
        shortcut.key,
        Code::F1
            | Code::F2
            | Code::F3
            | Code::F4
            | Code::F5
            | Code::F6
            | Code::F7
            | Code::F8
            | Code::F9
            | Code::F10
            | Code::F11
            | Code::F12
            | Code::F13
            | Code::F14
            | Code::F15
            | Code::F16
            | Code::F17
            | Code::F18
            | Code::F19
            | Code::F20
            | Code::F21
            | Code::F22
            | Code::F23
            | Code::F24
    );
    let held = shortcut.mods & (Modifiers::CONTROL | Modifiers::ALT | Modifiers::SUPER);
    if held.is_empty() && !function_key {
        return Err(invalid(
            "Add Ctrl, Alt or Win so the shortcut doesn't fire while you type",
        ));
    }
    Ok(shortcut)
}

/// Checks one change against the full list and returns the new list.
pub fn update_bindings(
    current: &[HotkeyBinding],
    action: &str,
    keys: &str,
) -> Result<Vec<HotkeyBinding>, CommandError> {
    if Action::parse(action).is_none() {
        return Err(CommandError::new(
            "invalid_action",
            "Unknown shortcut action",
        ));
    }
    let keys = keys.trim();
    if !keys.is_empty() {
        let shortcut = parse_keys(keys)?;
        if current.iter().any(|b| {
            b.action != action && !b.keys.is_empty() && parse_keys(&b.keys).ok() == Some(shortcut)
        }) {
            return Err(CommandError::new(
                "hotkey_taken",
                format!("{keys} is already used by another SyncSound shortcut"),
            ));
        }
    }
    let global = GLOBAL_ACTIONS.iter().any(|(a, _)| *a == action);
    let mut next: Vec<HotkeyBinding> = current
        .iter()
        .filter(|b| b.action != action)
        .cloned()
        .collect();
    // App-wide actions stay listed with empty keys; per-output ones disappear.
    if global || !keys.is_empty() {
        next.push(HotkeyBinding {
            action: action.into(),
            keys: keys.into(),
        });
    }
    if next.len() > MAX_BINDINGS {
        return Err(CommandError::new("too_many_hotkeys", "Too many shortcuts"));
    }
    Ok(next)
}

/// What is registered right now, for the handler to look up.
static REGISTERED: std::sync::Mutex<Vec<(Shortcut, Action)>> = std::sync::Mutex::new(Vec::new());

pub fn hotkey_plugin() -> tauri::plugin::TauriPlugin<tauri::Wry> {
    tauri_plugin_global_shortcut::Builder::new()
        .with_handler(|app, pressed, event| {
            if event.state() != ShortcutState::Pressed {
                return;
            }
            let action = REGISTERED
                .lock()
                .unwrap()
                .iter()
                .find(|(s, _)| s == pressed)
                .map(|(_, a)| a.clone());
            let Some(action) = action else { return };
            let app = app.clone();
            // Engine calls can block briefly: never on the hotkey thread.
            std::thread::spawn(move || {
                if let Some(state) = app.try_state::<AppState>() {
                    let _ = run_hotkey(&state, action);
                }
            });
        })
        .build()
}

/// One volume step for the louder / quieter shortcuts.
const STEP: f32 = 1.258_925_4; // 2 dB

fn run_hotkey(state: &AppState, action: Action) -> CmdResult<()> {
    let output = |id: &str| {
        state
            .engine
            .snapshot()
            .outputs
            .into_iter()
            .find(|o| o.endpoint_id == id)
            .ok_or_else(|| CommandError::new("unknown_device", "That speaker isn't connected"))
    };
    match action {
        Action::ToggleRouting => {
            // Start/stop is not a setup change: nothing to record.
            if state.engine.snapshot().running {
                state.engine.stop().map_err(engine_error)
            } else {
                state.engine.start().map_err(engine_error)
            }
        }
        Action::RecallA => recall_ab(state, 0).map(|_| ()),
        Action::RecallB => recall_ab(state, 1).map(|_| ()),
        Action::NextMode => {
            let now = state.engine.snapshot().listening_mode;
            let modes = ListeningMode::ALL;
            let next = modes
                .iter()
                .position(|m| *m == now)
                .map_or(modes[0], |i| modes[(i + 1) % modes.len()]);
            commit_state(state)?;
            state
                .engine
                .apply_listening_mode(next)
                .map_err(engine_error)?;
            commit_state(state).map(|_| ())
        }
        Action::Undo => step(state, true).map(|_| ()),
        Action::Redo => step(state, false).map(|_| ()),
        Action::Mute(id) => {
            let muted = output(&id)?.config.muted;
            commit_state(state)?;
            state.engine.set_mute(id, !muted).map_err(engine_error)?;
            commit_state(state).map(|_| ())
        }
        Action::Louder(id) => change_gain(state, id, true, output),
        Action::Quieter(id) => change_gain(state, id, false, output),
    }
}

fn change_gain(
    state: &AppState,
    id: String,
    louder: bool,
    output: impl Fn(&str) -> Result<syncsound_engine::OutputStatus, CommandError>,
) -> CmdResult<()> {
    let gain = output(&id)?.config.gain;
    let next = if louder {
        if gain < 0.01 {
            0.05
        } else {
            (gain * STEP).min(1.0)
        }
    } else if gain / STEP < 0.01 {
        0.0
    } else {
        gain / STEP
    };
    commit_state(state)?;
    state.engine.set_gain(id, next).map_err(engine_error)?;
    commit_state(state).map(|_| ())
}

/// Registers the bindings (or removes them all when `on` is false). Returns
/// the actions whose keys Windows refused because another app holds them.
pub fn set_hotkeys(app: &AppHandle, on: bool, bindings: &[HotkeyBinding]) -> Vec<String> {
    let gs = app.global_shortcut();
    let mut registered = REGISTERED.lock().unwrap();
    for (shortcut, _) in registered.drain(..) {
        let _ = gs.unregister(shortcut);
    }
    let mut refused = Vec::new();
    if !on {
        return refused;
    }
    for binding in bindings.iter().filter(|b| !b.keys.is_empty()) {
        let (Ok(shortcut), Some(action)) =
            (parse_keys(&binding.keys), Action::parse(&binding.action))
        else {
            continue;
        };
        if registered.iter().any(|(s, _)| *s == shortcut) {
            continue;
        }
        if gs.register(shortcut).is_ok() {
            registered.push((shortcut, action));
        } else {
            refused.push(binding.action.clone());
        }
    }
    refused
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keys_need_a_modifier_unless_they_are_function_keys() {
        assert!(parse_keys("Ctrl+Alt+S").is_ok());
        assert!(parse_keys("Ctrl+Shift+Up").is_ok());
        assert!(parse_keys("F9").is_ok());
        assert_eq!(parse_keys("S").unwrap_err().code, "invalid_hotkey");
        assert_eq!(parse_keys("Shift+S").unwrap_err().code, "invalid_hotkey");
        assert_eq!(
            parse_keys("Ctrl+Alt+Nope").unwrap_err().code,
            "invalid_hotkey"
        );
    }

    #[test]
    fn bindings_reject_duplicates_and_drop_cleared_output_keys() {
        let base = default_bindings();
        assert_eq!(
            update_bindings(&base, "mute:dev-1", "ctrl+alt+s")
                .unwrap_err()
                .code,
            "hotkey_taken"
        );
        let with = update_bindings(&base, "mute:dev-1", "Ctrl+Alt+M").unwrap();
        assert!(with
            .iter()
            .any(|b| b.action == "mute:dev-1" && b.keys == "Ctrl+Alt+M"));
        // Rebinding the same action to its own keys is not a conflict.
        assert!(update_bindings(&with, "mute:dev-1", "Ctrl+Alt+M").is_ok());
        let cleared = update_bindings(&with, "mute:dev-1", "").unwrap();
        assert!(!cleared.iter().any(|b| b.action == "mute:dev-1"));
        let off = update_bindings(&base, "undo", "").unwrap();
        assert!(off.iter().any(|b| b.action == "undo" && b.keys.is_empty()));
        assert_eq!(
            update_bindings(&base, "explode", "F9").unwrap_err().code,
            "invalid_action"
        );
        assert_eq!(
            update_bindings(&base, "mute:", "F9").unwrap_err().code,
            "invalid_action"
        );
    }
}

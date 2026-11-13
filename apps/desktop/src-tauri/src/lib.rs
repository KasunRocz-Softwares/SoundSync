use std::sync::Mutex;
use syncsound_engine::{AudioDevice, Engine};

#[tauri::command]
fn get_audio_devices() -> Result<Vec<AudioDevice>, String> { Engine::devices().map_err(|e| e.to_string()) }

#[tauri::command]
fn start_test_tone(id: String, engine: tauri::State<'_, Mutex<Engine>>) -> Result<(), String> {
    engine.lock().map_err(|e| e.to_string())?.start_test_tone(&id).map_err(|e| e.to_string())
}

#[tauri::command]
fn stop_device(id: String, engine: tauri::State<'_, Mutex<Engine>>) -> Result<(), String> {
    engine.lock().map_err(|e| e.to_string())?.stop(&id); Ok(())
}

#[tauri::command]
fn set_device_volume(id: String, volume: f32, engine: tauri::State<'_, Mutex<Engine>>) -> Result<(), String> {
    engine.lock().map_err(|e| e.to_string())?.set_gain(&id, volume).map_err(|e| e.to_string())
}

#[tauri::command]
fn stop_all(engine: tauri::State<'_, Mutex<Engine>>) -> Result<(), String> {
    engine.lock().map_err(|e| e.to_string())?.stop_all(); Ok(())
}

pub fn run() {
    tauri::Builder::default()
        .manage(Mutex::new(Engine::default()))
        .invoke_handler(tauri::generate_handler![get_audio_devices, start_test_tone, stop_device, set_device_volume, stop_all])
        .run(tauri::generate_context!())
        .expect("error while running SyncSound");
}

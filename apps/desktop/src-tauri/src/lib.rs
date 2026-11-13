use std::sync::mpsc::{self, Sender, SyncSender};
use syncsound_engine::{AudioDevice, Engine};

enum EngineCommand {
    Start(String, SyncSender<Result<(), String>>),
    Stop(String, SyncSender<Result<(), String>>),
    SetGain(String, f32, SyncSender<Result<(), String>>),
    StopAll(SyncSender<Result<(), String>>),
}

struct EngineHandle(Sender<EngineCommand>);

impl EngineHandle {
    fn new() -> Self {
        let (sender, receiver) = mpsc::channel();
        std::thread::spawn(move || {
            let mut engine = Engine::default();
            while let Ok(command) = receiver.recv() {
                match command {
                    EngineCommand::Start(id, reply) => {
                        let _ = reply.send(engine.start_test_tone(&id).map_err(|e| e.to_string()));
                    }
                    EngineCommand::Stop(id, reply) => {
                        engine.stop(&id);
                        let _ = reply.send(Ok(()));
                    }
                    EngineCommand::SetGain(id, gain, reply) => {
                        let _ = reply.send(engine.set_gain(&id, gain).map_err(|e| e.to_string()));
                    }
                    EngineCommand::StopAll(reply) => {
                        engine.stop_all();
                        let _ = reply.send(Ok(()));
                    }
                }
            }
        });
        Self(sender)
    }

    fn call(
        &self,
        make_command: impl FnOnce(SyncSender<Result<(), String>>) -> EngineCommand,
    ) -> Result<(), String> {
        let (reply, result) = mpsc::sync_channel(1);
        self.0
            .send(make_command(reply))
            .map_err(|e| e.to_string())?;
        result.recv().map_err(|e| e.to_string())?
    }
}

#[tauri::command]
fn get_audio_devices() -> Result<Vec<AudioDevice>, String> {
    Engine::devices().map_err(|e| e.to_string())
}

#[tauri::command]
fn start_test_tone(id: String, engine: tauri::State<'_, EngineHandle>) -> Result<(), String> {
    engine.call(|reply| EngineCommand::Start(id, reply))
}

#[tauri::command]
fn stop_device(id: String, engine: tauri::State<'_, EngineHandle>) -> Result<(), String> {
    engine.call(|reply| EngineCommand::Stop(id, reply))
}

#[tauri::command]
fn set_device_volume(
    id: String,
    volume: f32,
    engine: tauri::State<'_, EngineHandle>,
) -> Result<(), String> {
    engine.call(|reply| EngineCommand::SetGain(id, volume, reply))
}

#[tauri::command]
fn stop_all(engine: tauri::State<'_, EngineHandle>) -> Result<(), String> {
    engine.call(EngineCommand::StopAll)
}

pub fn run() {
    tauri::Builder::default()
        .manage(EngineHandle::new())
        .invoke_handler(tauri::generate_handler![
            get_audio_devices,
            start_test_tone,
            stop_device,
            set_device_volume,
            stop_all
        ])
        .run(tauri::generate_context!())
        .expect("error while running SyncSound");
}

//! Lists applications that have audio sessions on any active render endpoint,
//! for per-app routing. Control-thread only (COM + process queries).
use super::windows::{Apartment, CaptureError};
use crate::capture::AudioApp;
use std::collections::BTreeMap;
use windows::core::{Interface, PWSTR};
use windows::Win32::Foundation::CloseHandle;
use windows::Win32::Media::Audio::*;
use windows::Win32::System::Com::{CoCreateInstance, CLSCTX_ALL};
use windows::Win32::System::Threading::{
    OpenProcess, QueryFullProcessImageNameW, PROCESS_NAME_WIN32, PROCESS_QUERY_LIMITED_INFORMATION,
};

pub const SYNCSOUND_EXE: &str = "syncsound-desktop.exe";

/// Executable file name (e.g. "Spotify.exe") for a process id.
pub fn process_exe(pid: u32) -> Option<String> {
    unsafe {
        let handle = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid).ok()?;
        let mut buffer = [0u16; 1024];
        let mut len = buffer.len() as u32;
        let ok = QueryFullProcessImageNameW(
            handle,
            PROCESS_NAME_WIN32,
            PWSTR(buffer.as_mut_ptr()),
            &mut len,
        );
        let _ = CloseHandle(handle);
        ok.ok()?;
        let path = String::from_utf16_lossy(&buffer[..len as usize]);
        path.rsplit(['\\', '/']).next().map(str::to_string)
    }
}

fn friendly_name(exe: &str) -> String {
    exe.strip_suffix(".exe")
        .or_else(|| exe.strip_suffix(".EXE"))
        .unwrap_or(exe)
        .to_string()
}

/// One entry per executable; `active` if any of its sessions is playing now.
pub fn list_audio_apps() -> Result<Vec<AudioApp>, CaptureError> {
    let _apartment = Apartment::new()?;
    let own = std::process::id();
    let mut apps: BTreeMap<String, AudioApp> = BTreeMap::new();
    unsafe {
        let enumerator: IMMDeviceEnumerator =
            CoCreateInstance(&MMDeviceEnumerator, None, CLSCTX_ALL)?;
        let devices = enumerator.EnumAudioEndpoints(eRender, DEVICE_STATE_ACTIVE)?;
        for i in 0..devices.GetCount()? {
            let Ok(device) = devices.Item(i) else {
                continue;
            };
            let Ok(manager) = device.Activate::<IAudioSessionManager2>(CLSCTX_ALL, None) else {
                continue;
            };
            let Ok(sessions) = manager.GetSessionEnumerator() else {
                continue;
            };
            for s in 0..sessions.GetCount().unwrap_or(0) {
                let Ok(control) = sessions.GetSession(s) else {
                    continue;
                };
                let Ok(control2) = control.cast::<IAudioSessionControl2>() else {
                    continue;
                };
                let Ok(pid) = control2.GetProcessId() else {
                    continue;
                };
                if pid == 0 || pid == own {
                    continue;
                }
                let state = control.GetState().unwrap_or(AudioSessionStateExpired);
                if state == AudioSessionStateExpired {
                    continue;
                }
                let Some(exe) = process_exe(pid) else {
                    continue;
                };
                // Another SyncSound instance: capturing it would feed our own output back.
                if exe.eq_ignore_ascii_case(SYNCSOUND_EXE) {
                    continue;
                }
                let active = state == AudioSessionStateActive;
                let key = exe.to_ascii_lowercase();
                let entry = apps.entry(key).or_insert_with(|| AudioApp {
                    name: friendly_name(&exe),
                    exe: exe.clone(),
                    pid,
                    active,
                });
                // Prefer the pid that is actually playing.
                if active && !entry.active {
                    entry.pid = pid;
                    entry.active = true;
                }
            }
        }
    }
    let mut list: Vec<AudioApp> = apps.into_values().collect();
    list.sort_by(|a, b| {
        b.active
            .cmp(&a.active)
            .then(a.name.to_lowercase().cmp(&b.name.to_lowercase()))
    });
    Ok(list)
}

/// Resolves an executable name to a pid that currently has an audio session.
pub fn resolve_app(exe: &str) -> Option<u32> {
    list_audio_apps()
        .ok()?
        .into_iter()
        .find(|a| a.exe.eq_ignore_ascii_case(exe))
        .map(|a| a.pid)
}

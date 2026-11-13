//! Lists audio sessions (process + state) on every active render endpoint.
//!   cargo run -p syncsound-engine --example sessions
#[cfg(windows)]
fn main() {
    use windows::core::Interface;
    use windows::Win32::Devices::FunctionDiscovery::PKEY_Device_FriendlyName;
    use windows::Win32::Media::Audio::*;
    use windows::Win32::System::Com::StructuredStorage::PropVariantToStringAlloc;
    use windows::Win32::System::Com::*;
    unsafe {
        CoInitializeEx(None, COINIT_MULTITHREADED).ok().unwrap();
        let enumerator: IMMDeviceEnumerator =
            CoCreateInstance(&MMDeviceEnumerator, None, CLSCTX_ALL).unwrap();
        let devices = enumerator
            .EnumAudioEndpoints(eRender, DEVICE_STATE_ACTIVE)
            .unwrap();
        for i in 0..devices.GetCount().unwrap() {
            let device = devices.Item(i).unwrap();
            let store = device.OpenPropertyStore(STGM_READ).unwrap();
            let value = store.GetValue(&PKEY_Device_FriendlyName).unwrap();
            let name = PropVariantToStringAlloc(&value)
                .unwrap()
                .to_string()
                .unwrap();
            println!("{name}");
            let Ok(manager) = device.Activate::<IAudioSessionManager2>(CLSCTX_ALL, None) else {
                continue;
            };
            let sessions = manager.GetSessionEnumerator().unwrap();
            for s in 0..sessions.GetCount().unwrap() {
                let control = sessions.GetSession(s).unwrap();
                let state = control.GetState().unwrap();
                let Ok(control2) = control.cast::<IAudioSessionControl2>() else {
                    continue;
                };
                let pid = control2.GetProcessId().unwrap_or(0);
                let label = match state {
                    s if s == AudioSessionStateActive => "ACTIVE",
                    s if s == AudioSessionStateInactive => "inactive",
                    _ => "expired",
                };
                println!("    pid {pid:>6}  {label}");
            }
        }
    }
}

#[cfg(not(windows))]
fn main() {}

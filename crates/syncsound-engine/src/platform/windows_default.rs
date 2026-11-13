//! Read and change the Windows default playback device.
//!
//! Windows has no documented API to *set* the default endpoint. Every audio
//! utility (EarTrumpet, SoundSwitch, nircmd…) uses the undocumented but stable
//! IPolicyConfig COM interface on PolicyConfigClient, as here. Only call this
//! in response to an explicit user action, and always offer to restore the
//! previous default.
#![allow(non_snake_case)] // COM method names mirror the Windows interface.
use super::windows::{device_id, Apartment, CaptureError};
use std::ffi::c_void;
use windows::core::{GUID, HRESULT, PCWSTR};
use windows::Win32::Media::Audio::*;
use windows::Win32::System::Com::{CoCreateInstance, CLSCTX_ALL};
use windows_core::{interface, IUnknown, IUnknown_Vtbl};

const CLSID_POLICY_CONFIG_CLIENT: GUID = GUID::from_u128(0x870af99c_171d_4f9e_af0d_e63df40c2bc9);

/// Vtable order matters; only SetDefaultEndpoint is called.
#[interface("f8679f50-850a-41cf-9c72-430f290290c8")]
unsafe trait IPolicyConfig: IUnknown {
    fn GetMixFormat(&self, id: PCWSTR, format: *mut *mut c_void) -> HRESULT;
    fn GetDeviceFormat(&self, id: PCWSTR, default: i32, format: *mut *mut c_void) -> HRESULT;
    fn ResetDeviceFormat(&self, id: PCWSTR) -> HRESULT;
    fn SetDeviceFormat(&self, id: PCWSTR, endpoint: *mut c_void, mix: *mut c_void) -> HRESULT;
    fn GetProcessingPeriod(&self, id: PCWSTR, default: i32, a: *mut i64, b: *mut i64) -> HRESULT;
    fn SetProcessingPeriod(&self, id: PCWSTR, period: *mut i64) -> HRESULT;
    fn GetShareMode(&self, id: PCWSTR, mode: *mut c_void) -> HRESULT;
    fn SetShareMode(&self, id: PCWSTR, mode: *mut c_void) -> HRESULT;
    fn GetPropertyValue(&self, id: PCWSTR, key: *const c_void, value: *mut c_void) -> HRESULT;
    fn SetPropertyValue(&self, id: PCWSTR, key: *const c_void, value: *const c_void) -> HRESULT;
    fn SetDefaultEndpoint(&self, id: PCWSTR, role: ERole) -> HRESULT;
    fn SetEndpointVisibility(&self, id: PCWSTR, visible: i32) -> HRESULT;
}

/// Endpoint ID of the current default playback device (console role).
pub fn default_render_id() -> Result<Option<String>, CaptureError> {
    let _apartment = Apartment::new()?;
    unsafe {
        let enumerator: IMMDeviceEnumerator =
            CoCreateInstance(&MMDeviceEnumerator, None, CLSCTX_ALL)?;
        Ok(enumerator
            .GetDefaultAudioEndpoint(eRender, eConsole)
            .ok()
            .and_then(|d| device_id(&d).ok()))
    }
}

/// Makes `endpoint_id` the default playback device for every role.
pub fn set_default_render(endpoint_id: &str) -> Result<(), CaptureError> {
    let _apartment = Apartment::new()?;
    let wide: Vec<u16> = endpoint_id.encode_utf16().chain(Some(0)).collect();
    unsafe {
        let policy: IPolicyConfig =
            CoCreateInstance(&CLSID_POLICY_CONFIG_CLIENT, None, CLSCTX_ALL)?;
        for role in [eConsole, eMultimedia, eCommunications] {
            policy
                .SetDefaultEndpoint(PCWSTR(wide.as_ptr()), role)
                .ok()?;
        }
    }
    Ok(())
}

/// VB-Audio's free virtual cable shows up as a render endpoint named
/// "CABLE Input (VB-Audio Virtual Cable)".
pub fn is_virtual_cable(name: &str) -> bool {
    let n = name.to_ascii_lowercase();
    n.contains("cable input") && n.contains("vb-audio")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn recognises_vb_cable_by_name_only() {
        assert!(is_virtual_cable("CABLE Input (VB-Audio Virtual Cable)"));
        assert!(is_virtual_cable(
            "cable input (vb-audio virtual cable 16ch)"
        ));
        assert!(!is_virtual_cable("CABLE Output (VB-Audio Virtual Cable)"));
        assert!(!is_virtual_cable("Speakers (Realtek(R) Audio)"));
    }

    #[test]
    fn reads_the_current_default() {
        // Read-only: never changes the system default in tests.
        let id = default_render_id().expect("query default");
        assert!(id.as_deref().is_none_or(|i| i.starts_with('{')));
    }
}

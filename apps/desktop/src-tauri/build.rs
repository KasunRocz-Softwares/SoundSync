// Every app command must be listed here and granted in capabilities/default.json;
// anything else is denied by the Tauri ACL.
const COMMANDS: &[&str] = &[
    "get_audio_devices",
    "get_engine_status",
    "start_audio_engine",
    "stop_audio_engine",
    "select_capture_source",
    "enable_device",
    "disable_device",
    "set_device_volume",
    "set_device_mute",
    "set_device_delay",
    "set_audio_mode",
    "get_sync_metrics",
    "save_profile",
    "load_profile",
    "list_profiles",
    "rebind_profile_device",
    "export_diagnostics",
    "set_device_placement",
    "set_device_role",
    "set_device_trim",
    "set_distance_compensation",
    "apply_sound_preset",
    "set_device_map_placement",
    "set_device_spread",
    "set_device_subwoofer",
    "set_spatial",
    "set_device_eq",
    "list_audio_apps",
    "select_app_sources",
    "list_microphones",
    "calibrate",
];

fn main() {
    tauri_build::try_build(
        tauri_build::Attributes::new()
            .app_manifest(tauri_build::AppManifest::new().commands(COMMANDS)),
    )
    .expect("failed to run tauri build script");
}

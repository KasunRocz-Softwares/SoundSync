//! OS adapters; portable policy and PCM code do not depend on Windows bindings.
#[cfg(windows)]
pub mod windows;
#[cfg(windows)]
pub mod windows_apps;
#[cfg(windows)]
pub mod windows_default;
#[cfg(windows)]
pub mod windows_engine;
#[cfg(windows)]
pub mod windows_mic;
#[cfg(windows)]
pub mod windows_render;

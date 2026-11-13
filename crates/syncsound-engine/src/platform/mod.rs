//! OS adapters; portable policy and PCM code do not depend on Windows bindings.
#[cfg(windows)]
pub mod windows;

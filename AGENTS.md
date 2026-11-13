# Codex project instructions — SyncSound

You are implementing SyncSound, a Windows 11 multi-device audio synchronization application.

## Current state
- Rust `syncsound-engine` crate uses CPAL for endpoint enumeration and a basic float32 diagnostic tone.
- Tauri 2 desktop application exposes device discovery, start/stop diagnostic tone and per-output gain.
- React + TypeScript UI is a working scaffold.
- **No system audio capture or synchronization has been implemented.** Never describe the current test tone as synchronized system audio.

## Development goals (ordered)
1. Validate that the Windows project builds; fix Windows compilation or Tauri bundling errors first.
2. Implement native WASAPI loopback capture in a dedicated module/crate with reliable device selection and feedback-loop prevention.
3. Build timestamped float32 PCM ring-buffer fanout and per-device WASAPI render clients, including integer sample formats.
4. Implement a monotonic master presentation timeline and per-device buffering.
5. Implement adaptive resampling, drift correction and manual latency offsets.
6. Build measurable telemetry (underruns, buffers, drift, estimated latency) and device hotplug recovery.
7. Add Gaming / Balanced / Synchronized modes and persistent device profiles.
8. Add microphone-assisted acoustic calibration and virtual speaker mapping after stable audio routing.

## Engineering constraints
- No allocation, locks, disk IO or blocking IPC in real-time audio callbacks.
- Keep Windows audio APIs behind clean interfaces; core DSP/timeline logic should be testable on other OSes.
- Keep UI controls truthful; features must not be shown as working before implementation.
- Always run `cargo check`, `npm run build`, and Windows build checks when available.
- Add unit tests with simulated independent clocks and jitter before claiming sync accuracy.
- Do not introduce a virtual audio driver until loopback architecture is validated.
- Do not commit secrets or signing credentials.

## First Codex task
Get the Windows Tauri application compiling and launching; test output enumeration and diagnostic tone on wired/USB/HDMI/Bluetooth endpoints. Then create a focused PR implementing real WASAPI loopback capture and feedback avoidance.

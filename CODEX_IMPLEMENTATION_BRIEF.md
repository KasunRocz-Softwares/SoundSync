# SyncSound — Full Codex Implementation Brief

Version: 1.0 | Target: Windows 11 x64 | Project: `syncsound`

## Mission
Continue the included local SyncSound repository and deliver a genuinely working Windows desktop application that captures PC system audio and routes it to multiple selected Bluetooth, analog, USB, and HDMI/DisplayPort outputs, with low-latency Gaming mode and calibrated Synchronized mode. Do not present the current diagnostic-tone prototype as finished functionality.

## Repository starting point
Read `AGENTS.md`, `README.md`, and all current Rust/Tauri/React code before editing. Existing implementation includes a CPAL output enumeration and diagnostic tone, React UI, Tauri desktop scaffold, PowerShell build script and GitHub Actions workflow. System audio loopback, multi-device sample fanout, clock synchronization, Bluetooth compensation, persistent device IDs, and installer validation are **not yet implemented**. Preserve working scaffolding while replacing prototype functionality with real features.

## Product requirements
- Enumerate Windows output endpoints with persistent device identity, friendly name, connection type where discoverable, state, mix format and supported modes.
- Capture Windows default system audio using WASAPI shared-mode loopback; allow explicit source selection; prevent capture/render feedback and double playback. Explain limits of loopback and exclusive-mode apps.
- Output to two or more selected endpoints concurrently, each with independent gain, mute, channel mapping, delay, format negotiation, buffering, and reconnect behavior.
- Modes: Gaming (minimize end-to-end delay, warn/exclude slow outputs), Balanced (trade-off), Synchronized (align acoustic playback to slowest device, accepts higher overall delay).
- Show real measured metrics: queue occupancy, underruns, clock drift ppm, estimated device latency; distinguish estimated values from microphone-measured acoustic offsets.
- Save named profiles; restore selected devices and parameters safely after restart/hotplug.
- Build and bundle NSIS EXE and MSI on Windows; no signing secrets in repository.
- Future phase: microphone calibration, channel mapping / surround, app-specific routing, signed virtual audio endpoint driver.

## Non-negotiable truth and safety
Zero latency is impossible. Software cannot eliminate Bluetooth codec/transport delay; it can delay faster outputs. Never claim 2ms wired or 10ms Bluetooth skew without external measurement. Never claim a Windows installer has been built or tested unless it actually has. Do not ship always-on test tone. Avoid feedback loops, uncontrolled volume spikes, unbounded buffers, and stale device routing. No hidden microphone/audio recording or audio upload.

## Architecture
Use clean Rust crates/modules for Windows endpoint enumeration and WASAPI capture/render, timestamped PCM transport, master timeline, adaptive resampling, per-device scheduling, DSP, profile storage, telemetry, and Tauri commands. React/TypeScript is only the control UI. Keep DSP and synchronization platform-independent and unit-testable with fake devices.

Suggested processing flow:
Windows audio session -> WASAPI loopback capture -> timestamped float32 PCM frames -> bounded fanout ring buffers -> per-device channel conversion -> drift-aware sample rate conversion -> compensation delay -> WASAPI event-driven render -> output endpoint.

Use a monotonic high-resolution master clock (QPC) mapped to audio frame indices. Each output has an independent clock estimate, buffer fill controller, and correction ratio. Avoid global locks, allocations, logging, I/O, and blocking IPC in audio callbacks. Use MMCSS and event-driven WASAPI where appropriate. The control plane must own device state transitions and recovery.

## Synchronization model
Let source presentation time be T, device acoustic latency estimate be L_i, and software-added delay be D_i. For Sync mode, initial compensation D_i = max_j(L_j) - L_i, plus jitter/safety margin. This is only a starting estimate; Bluetooth and HDMI acoustic latencies may be unobservable from Windows timestamps. Store calibrated acoustic offset separately. For independent clock drift, estimate effective sample-rate error in ppm and adjust per-device resampling ratio gradually with bounded corrections. Use filtered timestamp/buffer error with outlier rejection and explicit reset on discontinuities. Never drop or repeat entire PCM blocks for routine drift correction.

## Implementation sequence and acceptance gates
### Gate 0 — Baseline
- Validate `npm ci`, `npm run build`, `cargo test --workspace`, `cargo check --workspace`, and `npm run desktop:dev` on Windows; fix any compilation errors.
- Verify Tauri capabilities, endpoint enumeration, diagnostic tone stop/start, and cleanup.
- Record baseline limitations and Windows test evidence.

### Gate 1 — Real system audio
- Implement WASAPI shared-mode loopback capture and a source selector.
- Implement bounded timestamped PCM ring buffers, conversion to float32, and source/render feedback prevention.
- Play captured system audio through one selected physical output without echo/doubling. Verify with browser/media playback.

### Gate 2 — Multiple outputs
- Add per-output render worker, format negotiation (float32, int16, int24/int32 where supported), resampling and volume/mute.
- Support analog + USB and analog + HDMI; verify one output can disconnect without killing other streams.
- Add stable endpoint IDs and hotplug handling.

### Gate 3 — Synchronization
- Add shared timeline, manual delay offsets, clock drift estimation, adaptive resampling, and metrics.
- Write deterministic tests for two fake endpoints with ±100 ppm drift, jitter, underruns and discontinuities.
- Verify a 60-minute two-wired-device session with no persistent drift or unintended dropouts; publish measured skew methodology.

### Gate 4 — Bluetooth and modes
- Add Gaming/Balanced/Synchronized policy and Bluetooth device support via Windows-exposed endpoints.
- Report estimated latency and uncertainty; manual calibration; clear warning when Bluetooth cannot meet gaming responsiveness.
- Test disconnect/reconnect and mixed-device groups.

### Gate 5 — Desktop and packaging
- Implement dashboard, device selection, per-output volume/delay, sync health, profile persistence, errors and diagnostics export.
- Build Windows NSIS EXE and MSI with `scripts/build-windows.ps1`; run installation, upgrade and uninstall smoke tests on Windows 11.
- Add CI builds and release artifact retention; document unsigned installer warnings and future signing process.

### Gate 6 — Post-MVP
- Microphone impulse/correlation acoustic calibration; channel mapping, stereo/5.1/7.1 processing, EQ/crossover, application-specific routing and optional signed virtual driver. Keep these disabled or labeled experimental until validated.

## Suggested API/IPC surface
`get_audio_devices`, `get_engine_status`, `start_audio_engine`, `stop_audio_engine`, `select_capture_source`, `enable_device`, `disable_device`, `set_device_volume`, `set_device_mute`, `set_device_delay`, `set_audio_mode`, `get_sync_metrics`, `save_profile`, `load_profile`, `export_diagnostics`.

Commands should validate input and return typed errors. Telemetry events should be throttled and not block real-time audio processing.

## Tests and supported matrix
- Analog + USB; two USB; analog + HDMI; analog + Bluetooth; two Bluetooth where possible.
- 44.1/48/96 kHz conversion; mono/stereo output; sample format negotiation.
- Default endpoint switch; unplug/replug; Bluetooth reconnect; Windows sleep/resume; invalidated WASAPI clients; buffer underruns; CPU pressure.
- Unit tests: ring buffer bounds, clock estimator, resampler ratio clamps, frame scheduling, channel mapping, delay compensation, profile migration.
- Physical tests: 60-minute playback, calibrated microphone or multi-channel recorder for acoustic skew; do not conflate API timing with sound arrival.

## Codex execution instructions
1. Inspect repository and report the exact baseline implementation and build results.
2. Work incrementally; commit logical changes; keep the app runnable after each gate.
3. Prioritize Gate 0, Gate 1 and Gate 2 before any UI polish or surround features.
4. Run available checks after each significant change and fix failures.
5. When hardware or Windows compilation is unavailable, state precisely what is unverified and provide commands to run on the user's Windows 11 machine.
6. Do not install or execute software on the user's PC without access and authorization; produce installer artifacts only after a successful Windows build.
7. Provide final output: implemented feature list, changed files, commands, tests, known limitations, installer artifact paths, and next milestone.

## Windows setup and commands
Install Node.js 22+, Rust stable MSVC target, Visual Studio Build Tools 2022 Desktop development with C++, Windows SDK, and WebView2 Runtime. From repository root:

```powershell
npm ci
cargo test --workspace
cargo check --workspace
npm run build
npm run desktop:dev
powershell -ExecutionPolicy Bypass -File .\scripts\build-windows.ps1
```

Expected installers after successful Windows bundling: `target/release/bundle/nsis/*.exe` and `target/release/bundle/msi/*.msi` (check actual Tauri target directory). Do not assert artifacts exist without checking.

## Definition of done for v0.1
At minimum, Windows system audio is captured and played simultaneously on two independently selected devices, with per-output gain/mute, manual delay, reliable start/stop, no feedback loop, safe disconnection, useful diagnostic metrics, and an installable Windows build validated on Windows. If these criteria are not met, call the result a developer preview and enumerate gaps.

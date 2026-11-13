# SyncSound Windows Audio Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Replace the CPAL diagnostic-tone prototype with a truthful Windows WASAPI loopback router that can play system audio on multiple selected outputs, report bounded metrics, persist profiles, and install as a verified Windows build on `F:\SyncSound`.

**Architecture:** Use Windows `windows` bindings behind a platform module for endpoint enumeration, process-exclusion loopback capture, event-driven shared-mode rendering, and device notifications. Keep bounded PCM transport, timeline, channel conversion, resampling, delay compensation, clock estimation, profiles, and telemetry in Rust modules that are unit-testable without Windows. Tauri owns validated control-plane transitions; React renders only engine-reported capability and health state.

**Tech Stack:** Rust 2021 workspace, `windows` Win32 audio bindings, existing Tauri 2, React 18, TypeScript, Vite, JSON profiles in app data, NSIS/MSI bundling.

**Spec:** `docs/superpowers/specs/2026-11-13-syncsound-windows-audio-design.md`

## Global Constraints

- Target Windows 11 x64 and preserve the existing Tauri 2 + React desktop shell.
- Use WASAPI shared-mode loopback and event-driven render; never claim zero latency or unmeasured acoustic skew.
- Limit v0.1 to eight simultaneously enabled output endpoints.
- Keep real-time callbacks free of allocation, locks, disk I/O, logging, and blocking IPC.
- Use bounded queues and gradual clamped resampling; do not drop/repeat whole PCM blocks for routine drift correction.
- Never silently substitute a missing profile device or create a feedback loop.
- Keep profiles in local app data, never the install directory.
- Build both NSIS and MSI; install and smoke-test the verified NSIS artifact at `F:\SyncSound` only after successful Windows packaging.
- Without physical hardware evidence, label the result developer preview and enumerate unverified tests.

## Review Focus

- A loopback source equal to an enabled output must not capture SyncSound's own render and create runaway feedback; task 3 adds an exclusion/blocked-routing test.
- A queue reaching full or empty must stay bounded and expose an underrun/overrun metric without blocking the audio path; task 2 adds capacity and discontinuity tests.
- A saved profile with missing or renamed endpoints must not silently bind to the wrong device; task 5 adds partial-apply and explicit-rebind tests.
- A device removed during playback must not stop healthy outputs or leak its worker; task 4 adds independent recovery-state tests.
- Untrusted or stale Tauri inputs must be rejected without changing engine state; task 5 adds command validation tests.

### Task 1: Establish the runnable baseline

**Files:**
- Modify: `Cargo.toml`, `crates/syncsound-engine/Cargo.toml`, `apps/desktop/src-tauri/Cargo.toml`
- Modify: `README.md`, `docs/ROADMAP.md`
- Create: `docs/baseline/2026-11-13-windows-baseline.md`
- Test: existing workspace checks and build commands

**Interfaces:**
- Consumes: existing archive source and design spec.
- Produces: a compiling workspace, recorded toolchain results, and a clean baseline for subsequent tasks.

- [ ] **Step 1: Verify environment and source inventory**

Run `node --version`, `npm --version`, `rustc --version`, `cargo --version`, `rustup target list --installed`, and `git status`. Record exact output and whether the MSVC target, Windows SDK, and WebView2 prerequisites are present.

- [ ] **Step 2: Run baseline checks**

Run `npm ci`, `npm run build`, `cargo test --workspace`, and `cargo check --workspace`. Record failures without changing product behavior.

- [ ] **Step 3: Add required Windows audio dependencies and feature flags**

Add only the `windows` crate features required for Core Audio endpoint, client, capture, render, property-store, and device-notification APIs. Keep the platform-independent engine free of Windows-only types.

- [ ] **Step 4: Re-run checks and document limitations**

Run the same commands and write the results and exact unverified items to `docs/baseline/2026-11-13-windows-baseline.md`.

- [ ] **Step 5: Commit**

```powershell
git add Cargo.toml crates apps README.md docs/baseline
git commit -m "chore: establish Windows audio baseline"
```

### Task 2: Build the platform-independent PCM and timing core

**Files:**
- Create: `crates/syncsound-engine/src/audio_format.rs`
- Create: `crates/syncsound-engine/src/ring_buffer.rs`
- Create: `crates/syncsound-engine/src/timeline.rs`
- Create: `crates/syncsound-engine/src/dsp.rs`
- Create: `crates/syncsound-engine/src/metrics.rs`
- Modify: `crates/syncsound-engine/src/lib.rs`
- Test: module unit tests in the new files

**Interfaces:**
- Produces `PcmFormat { sample_rate: u32, channels: u16 }`, `PcmBlock { first_frame: u64, format: PcmFormat, samples: Vec<f32> }`, `BoundedPcmQueue`, `MasterTimeline`, `ClockEstimator`, `resample_interleaved`, `map_channels`, and `DelayLine` for Tasks 3–5.

- [ ] **Step 1: Write failing ring-buffer tests**

Add tests for FIFO order, exact capacity, full-queue rejection, empty-queue underflow, fanout isolation, and discontinuity reset. Assert that no operation grows storage beyond the configured bound.

- [ ] **Step 2: Run the focused tests and observe failure**

Run `cargo test -p syncsound-engine ring_buffer -- --nocapture`; expected result is compilation/test failure because the types do not exist.

- [ ] **Step 3: Implement bounded timestamped PCM transport**

Implement producer/consumer operations with preallocated storage and explicit `QueueStatus`; audio-path methods must not lock, allocate, log, or block.

- [ ] **Step 4: Write failing timing/DSP tests**

Add deterministic tests for QPC/frame mapping, ±100 ppm clock estimates, outlier rejection, clamped correction ratios, mono/stereo conversion, 44.1/48/96 kHz resampling, delay compensation, and reset on discontinuity.

- [ ] **Step 5: Implement the timeline and DSP primitives**

Use integer frame positions for scheduling, bounded floating-point correction ratios, linear/polyphase interpolation appropriate to the current sample-rate range, and explicit channel-layout conversion. Keep the API independent of Windows.

- [ ] **Step 6: Run the focused and workspace tests**

Run `cargo test -p syncsound-engine` and `cargo test --workspace`; expected result is all tests passing with deterministic output.

- [ ] **Step 7: Commit**

```powershell
git add crates/syncsound-engine
git commit -m "feat: add bounded PCM transport and timing core"
```

### Task 3: Implement Windows endpoint identity and loopback capture

**Files:**
- Create: `crates/syncsound-engine/src/platform/mod.rs`
- Create: `crates/syncsound-engine/src/platform/windows.rs`
- Create: `crates/syncsound-engine/src/capture.rs`
- Modify: `crates/syncsound-engine/Cargo.toml`, `crates/syncsound-engine/src/lib.rs`
- Test: Windows integration tests plus platform-independent capture state tests

**Interfaces:**
- Consumes: `PcmFormat`, `PcmBlock`, `BoundedPcmQueue` from Task 2.
- Produces `AudioEndpoint`, `CaptureSource`, `CaptureConfig`, `LoopbackCapture`, and `CaptureState` for Tasks 4–5.

- [ ] **Step 1: Write failing capture-state and feedback tests**

Test default-source follow behavior, explicitly pinned source behavior, endpoint-ID stability across enumeration order, exclusion of the SyncSound process, and rejection of unsafe same-endpoint routing when exclusion is unavailable.

- [ ] **Step 2: Run focused tests and observe failure**

Run `cargo test -p syncsound-engine capture -- --nocapture`; expected result is failure because the capture types and platform adapter are not implemented.

- [ ] **Step 3: Implement endpoint enumeration**

Use `IMMDeviceEnumerator` and property stores to expose stable endpoint IDs, friendly names, default state, active/unavailable state, mix format, channel count, and a best-effort connection-type label. Do not use enumeration indices as IDs.

- [ ] **Step 4: Implement process-exclusion WASAPI loopback**

Create a shared-mode loopback client for the selected render endpoint, configure the process-exclusion activation path for SyncSound's process, convert captured frames to timestamped float32 blocks, and publish only into the bounded source queue. On device invalidation, transition to a recoverable state and release COM/WASAPI resources.

- [ ] **Step 5: Implement source selection transitions**

Default mode follows the Windows default render endpoint; explicit mode pins an endpoint ID. Default-device notifications restart capture safely, while explicit mode reports unavailable without silently switching.

- [ ] **Step 6: Run tests and Windows checks**

Run `cargo test -p syncsound-engine`, `cargo check --workspace`, and a Windows-only endpoint/capture smoke command when the MSVC toolchain is available. Record hardware-dependent results separately.

- [ ] **Step 7: Commit**

```powershell
git add crates/syncsound-engine
git commit -m "feat: add Windows endpoint catalog and loopback capture"
```

### Task 4: Add per-output WASAPI render workers and multi-device routing

**Files:**
- Create: `crates/syncsound-engine/src/render.rs`
- Create: `crates/syncsound-engine/src/output_state.rs`
- Modify: `crates/syncsound-engine/src/platform/windows.rs`, `crates/syncsound-engine/src/lib.rs`
- Test: render scheduling, format conversion, disconnect/reconnect, and two-output fake-device tests

**Interfaces:**
- Consumes: `AudioEndpoint`, `PcmBlock`, `BoundedPcmQueue`, `MasterTimeline`, and DSP functions from Tasks 2–3.
- Produces `OutputConfig`, `OutputStatus`, `RenderWorker`, `OutputGroup`, and `EngineSnapshot` for Task 5.

- [ ] **Step 1: Write failing worker tests**

Test independent queues for two outputs, per-output gain/mute/delay, stereo/mono mapping, float32 and integer sample conversion, bounded queue refill, healthy-output continuation after one disconnect, and retry backoff capped at 30 seconds.

- [ ] **Step 2: Run focused tests and observe failure**

Run `cargo test -p syncsound-engine render output_state -- --nocapture`; expected result is failure because worker/state types do not exist.

- [ ] **Step 3: Implement per-output scheduling and conversion**

For each enabled endpoint, read from its own bounded queue, apply mode delay and gain/mute, update the per-device clock estimator, resample gradually, convert to the negotiated WASAPI sample format, and render on an event-driven client. Do not share mutable audio callback state across endpoints.

- [ ] **Step 4: Implement output lifecycle and recovery**

Use an explicit state machine for Disabled, Starting, Running, Disconnected, Retrying, and Failed. On removal, keep other outputs running; release the failed client and retry with bounded exponential backoff before requiring explicit user action.

- [ ] **Step 5: Run focused, workspace, and two-output checks**

Run `cargo test -p syncsound-engine`, `cargo test --workspace`, and Windows smoke tests against two available endpoints. Expected result: one source can feed two independently configured outputs without diagnostic-tone code remaining in the active path.

- [ ] **Step 6: Commit**

```powershell
git add crates/syncsound-engine
git commit -m "feat: route loopback audio to independent outputs"
```

### Task 5: Integrate the engine with typed Tauri commands, telemetry, and profiles

**Files:**
- Create: `crates/syncsound-engine/src/profile.rs`
- Create: `crates/syncsound-engine/src/commands.rs`
- Modify: `crates/syncsound-engine/src/lib.rs`
- Modify: `apps/desktop/src-tauri/src/lib.rs`, `apps/desktop/src-tauri/Cargo.toml`
- Modify: `apps/desktop/src-tauri/capabilities/default.json`
- Test: command validation, profile migration, partial apply, and telemetry throttling

**Interfaces:**
- Consumes: `EngineSnapshot`, `OutputConfig`, source/output lifecycle APIs from Tasks 3–4.
- Produces Tauri commands: `get_audio_devices`, `get_engine_status`, `start_audio_engine`, `stop_audio_engine`, `select_capture_source`, `enable_device`, `disable_device`, `set_device_volume`, `set_device_mute`, `set_device_delay`, `set_audio_mode`, `get_sync_metrics`, `save_profile`, `load_profile`, and `export_diagnostics`.

- [ ] **Step 1: Write failing command/profile tests**

Test invalid IDs, volume outside `[0,1]`, delay outside the configured bound, more than eight outputs, invalid mode/source transitions, profile version migration, missing-device partial apply, and throttled telemetry snapshots.

- [ ] **Step 2: Run focused tests and observe failure**

Run `cargo test -p syncsound-engine commands profile -- --nocapture`; expected result is failure because the typed command/profile layer is not implemented.

- [ ] **Step 3: Implement versioned profile persistence**

Store validated profiles in the per-user app-data directory with an explicit schema version. Load unavailable devices as missing entries and require an explicit rebind; never match by friendly name without user confirmation.

- [ ] **Step 4: Implement Tauri state and command validation**

Move engine ownership behind a control-plane state manager, validate all values before mutating engine state, return typed serializable errors, and expose snapshots/metrics at a throttled cadence.

- [ ] **Step 5: Add diagnostics export and capabilities**

Export a redacted JSON diagnostic bundle containing versions, endpoint metadata, engine states, queue metrics, and error history; update Tauri permissions only for the commands actually used.

- [ ] **Step 6: Run Rust and Tauri checks**

Run `cargo test --workspace`, `cargo check --workspace`, and `npm run build`; expected result is green typecheck, Rust tests, and command generation.

- [ ] **Step 7: Commit**

```powershell
git add crates apps/desktop/src-tauri
git commit -m "feat: expose validated engine control and profiles"
```

### Task 6: Replace the prototype UI with the real control dashboard

**Files:**
- Modify: `apps/desktop/src/main.tsx`
- Modify: `apps/desktop/src/style.css`
- Create: `apps/desktop/src/types.ts`, `apps/desktop/src/api.ts`
- Test: `npm run build` and manual Tauri UI smoke test

**Interfaces:**
- Consumes: typed Tauri commands and snapshots from Task 5.
- Produces: source selector, mode selector, device cards with enable/gain/mute/delay, health metrics, profile controls, diagnostics export, errors, and truthful capability/uncertainty labels.

- [ ] **Step 1: Define TypeScript models and API wrappers**

Mirror the serializable Rust types and centralize `invoke` calls with typed results and user-safe error formatting.

- [ ] **Step 2: Implement source/mode/device controls**

Show default-follow versus pinned source, active/missing endpoint status, per-output controls, 8-output limit, and start/stop state. Disable controls that the engine reports unavailable.

- [ ] **Step 3: Implement telemetry and profile views**

Render queue occupancy, underruns, drift ppm, estimated latency, and uncertainty; label estimated values separately from acoustic calibration, which is not in v0.1. Add save/load/rebind profile flows and diagnostics export.

- [ ] **Step 4: Remove prototype-only test-tone UI**

Remove the 440 Hz start/stop controls and warning copy that suggests the tone is the product. Keep a clearly labeled developer-preview warning until release gates pass.

- [ ] **Step 5: Build and smoke-test**

Run `npm run build` and `npm run desktop:dev` on Windows. Verify refresh, source selection, start/stop, device removal display, profile error states, and no stale prototype calls.

- [ ] **Step 6: Commit**

```powershell
git add apps/desktop/src
git commit -m "feat: add SyncSound routing dashboard"
```

### Task 7: Package, install, and verify the Windows application

**Files:**
- Modify: `apps/desktop/src-tauri/tauri.conf.json`
- Modify: `scripts/build-windows.ps1`, `.github/workflows/windows.yml`, `README.md`
- Create: `docs/verification/2026-11-13-windows-release.md`
- Test: Windows bundle, installation, upgrade, uninstall, and artifact checks

**Interfaces:**
- Consumes: completed engine, Tauri commands, UI, and profiles from Tasks 1–6.
- Produces: verified NSIS/MSI artifacts, a tested install at `F:\SyncSound`, and an evidence report distinguishing verified and unavailable hardware checks.

- [ ] **Step 1: Write packaging smoke checks**

Add script checks that fail when `target/release/bundle/nsis/*.exe` or `target/release/bundle/msi/*.msi` is absent, when the application fails to launch after installation, or when profile data is not preserved across an upgrade.

- [ ] **Step 2: Configure truthful product metadata**

Set the final short description, installer metadata, bundle targets, and Windows-safe paths without signing secrets. Keep install data and profiles separate.

- [ ] **Step 3: Build both bundles**

Run `npm ci`, `npm run build`, `cargo test --workspace`, `cargo check --workspace`, and `npm run desktop:build`. Confirm both artifacts exist at the actual Tauri output paths.

- [ ] **Step 4: Install NSIS to `F:\SyncSound`**

Run the verified installer with an explicit target directory, launch the installed executable, enumerate endpoints, and confirm start/stop leaves no active clients. Do not overwrite unrelated files on `F:`.

- [ ] **Step 5: Run upgrade/uninstall smoke checks**

Install the same or newer build over the existing installation, confirm profiles remain in app data, then uninstall and verify only the installed application is removed. Record exact commands and exit codes.

- [ ] **Step 6: Run available physical checks**

Test the available analog/USB/HDMI/Bluetooth combinations, default-device switching, unplug/replug, sleep/resume, and a wired session where hardware permits. Mark every unavailable test as unverified.

- [ ] **Step 7: Commit release evidence**

```powershell
git add apps/desktop/src-tauri scripts .github README.md docs/verification
git commit -m "build: package and verify Windows release"
```

## Execution order

Run Tasks 1–7 in order. Each task must finish its focused RED→GREEN test cycle and workspace checks before the next task starts. A failure in a Windows-only step is recorded with the exact missing prerequisite; it is not converted into a success claim.

## Final review checklist

- `cargo test --workspace` and `cargo check --workspace` pass on Windows.
- `npm run build` passes.
- The installed app captures system audio through loopback and routes it to two selected outputs without feedback.
- Gain, mute, delay, start/stop, disconnect recovery, metrics, profiles, and diagnostics are exercised.
- NSIS and MSI artifacts exist and are inspected; the verified NSIS build is installed at `F:\SyncSound`.
- Final report explicitly separates implemented features, verified hardware behavior, and developer-preview gaps.

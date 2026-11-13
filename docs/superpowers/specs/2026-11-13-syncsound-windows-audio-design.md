# SyncSound Windows Audio Design

Date: 2026-11-13  
Status: Draft for approval  
Target: Windows 11 x64, installed to `F:\SyncSound`

## Intent

Turn the current Tauri/React diagnostic-tone prototype into a truthful Windows desktop application that captures Windows system audio through WASAPI shared-mode loopback and renders it to multiple independently selected physical output endpoints. The first usable milestone is two-output playback with safe start/stop, per-output gain/mute/delay, bounded buffering, disconnect recovery, and diagnostics. Synchronization is measured and reported with uncertainty; the application will not promise zero latency or unmeasured acoustic skew.

## Current baseline

- Tauri 2 desktop shell with React/TypeScript UI.
- Rust `syncsound-engine` crate using CPAL for output enumeration.
- Session-local numeric device IDs and default format metadata.
- Independent float32 440 Hz test tone with a mutex-backed gain control.
- No loopback capture, timestamped transport, multi-device timeline, persistent IDs, hotplug recovery, telemetry, or validated installer.

## Goals

1. Enumerate Windows render endpoints with stable endpoint identity, friendly name, default state, connection type when discoverable, state, mix format, and supported modes.
2. Capture the selected Windows render mix using WASAPI shared-mode loopback without routing SyncSound's own render streams back into the source.
3. Fan out timestamped float32 PCM through bounded per-output queues.
4. Render to two or more endpoints concurrently with format negotiation, channel conversion, gain/mute, delay, reconnect, and bounded buffering.
5. Provide Gaming, Balanced, and Synchronized modes with explicit latency/uncertainty messaging.
6. Report queue occupancy, underruns, clock drift estimate, and estimated device latency separately from microphone-measured acoustic offset.
7. Persist named profiles safely across restart and hotplug.
8. Produce and validate Windows NSIS and MSI bundles before installation to `F:\SyncSound`.

## Non-goals for v0.1

- A signed virtual audio driver.
- Application-specific routing, EQ, surround mapping, or microphone calibration.
- Claiming acoustic synchronization based only on Windows stream timestamps.
- Shipping an always-on diagnostic tone or any hidden microphone/audio upload.

## Anti-goals and safety rules

- Never use unbounded queues, uncontrolled volume changes, feedback loops, or routine whole-block drop/repeat for drift correction.
- Never describe an estimate as a physical measurement.
- Never claim an installer or hardware test was completed without checking the artifact or hardware result.
- Stop and release all render/capture clients on failure, device removal, or application shutdown.

## Approaches considered

### A. Native Windows WASAPI with platform-independent Rust core — selected

Use `windows` bindings for endpoint enumeration, loopback capture, event-driven shared-mode render, endpoint properties, and device notifications. Keep PCM transport, timeline, clock estimator, resampler, delay compensation, profiles, and telemetry in testable Rust modules that do not depend on Windows. This is the selected approach because the product requires endpoint identity, loopback-specific behavior, format negotiation, and per-device timing that CPAL does not expose consistently.

### B. CPAL-first with a WASAPI loopback adapter

Retain CPAL for capture and render, adding Windows-specific code only for loopback. This minimizes initial code but leaves endpoint identity, notification, buffer timing, and format control constrained by the abstraction. It is likely to create a second implementation when synchronization and recovery are added, so it is not selected.

### C. External helper or virtual-driver pipeline

Delegate capture/routing to another process or a virtual endpoint. This could reduce Rust audio code but adds installation and lifecycle failure modes, does not remove independent hardware clock drift, and violates the brief's sequencing constraint that a virtual driver comes only after loopback validation. It is not selected.

## Architecture

The audio path will be:

```text
Windows default/selected render mix
        -> WASAPI shared-mode loopback capture
        -> timestamped float32 PCM blocks
        -> bounded fanout queues
        -> per-device channel conversion and format conversion
        -> gradual drift-aware resampling
        -> manual/mode compensation delay
        -> event-driven WASAPI shared-mode render clients
```

The engine will be split into these modules:

- `platform/windows`: endpoint identity/properties, loopback capture, render client, device notifications.
- `transport`: bounded timestamped PCM blocks and fanout ownership rules.
- `timeline`: monotonic QPC-to-frame mapping, scheduling, delay compensation, discontinuity reset.
- `dsp`: channel conversion, sample-format conversion, and bounded adaptive resampling.
- `devices`: output state machine, reconnect, metrics, and safe shutdown.
- `profiles`: versioned JSON profile storage with validation and migration.
- `telemetry`: throttled snapshots/events for Tauri without touching real-time callbacks.

The Tauri command layer owns control-plane state transitions and validates all user input. React displays only capabilities returned by the engine and labels unsupported or estimated values.

## Synchronization model

For output `i`, use source presentation time `T`, estimated device latency `L_i`, and software delay `D_i`. In Synchronized mode, start with `D_i = max(L_j) - L_i + safety_margin`; in Gaming mode, reject or warn about outputs whose estimated delay exceeds the selected budget; Balanced uses bounded compromise. Each output maintains a filtered buffer/timestamp error and an independent correction ratio. Resampler corrections are clamped and applied gradually. Discontinuities reset the estimator and refill the affected queue rather than dropping or duplicating routine PCM blocks.

## Delivery gates

1. **Baseline:** extract and index the repository, run JavaScript/Rust checks, fix compilation/configuration issues, and document environment limitations.
2. **Loopback:** implement selected-source WASAPI loopback, timestamped bounded transport, feedback prevention, and one-output playback.
3. **Multiple outputs:** implement per-output workers, format/channel conversion, stable IDs, hotplug/reconnect, gain/mute/delay, and two-output routing.
4. **Synchronization:** add master timeline, drift estimation, adaptive resampling, mode policy, metrics, and deterministic fake-device tests.
5. **Profiles/UI:** add profiles, device status, health metrics, errors, and diagnostics export.
6. **Packaging:** build NSIS/MSI, run install/upgrade/uninstall smoke checks on Windows, and install only the verified artifact to `F:\SyncSound`.

Each gate must leave the application runnable. The UI will retain a developer-preview label until the v0.1 acceptance criteria are actually verified.

## Verification strategy

- Rust unit tests for ring-buffer bounds, fanout isolation, channel mapping, delay scheduling, resampler clamps, clock estimation, discontinuities, and profile migration.
- Fake endpoints with ±100 ppm drift, jitter, underruns, and disconnect/reconnect sequences.
- JavaScript typecheck and production build.
- Windows `cargo check --workspace`, `cargo test --workspace`, Tauri development launch, bundle build, and artifact existence checks.
- Physical validation where hardware is available: analog + USB, analog + HDMI, and Bluetooth combinations; default-device switching; unplug/replug; sleep/resume; and a 60-minute wired session. API timing will not be reported as acoustic measurement.

## Install and operational boundary

The application will be installed under `F:\SyncSound` only after a successful Windows build and artifact verification. If the environment cannot run the Windows toolchain or hardware checks, the result will be reported as a developer preview with exact commands and unverified items rather than presented as production-ready.

## Resolved implementation calls

- Use the existing Tauri 2 and React stack; do not replace the desktop shell.
- Use Rust `windows` bindings for Windows-only audio functionality and keep the engine API typed/serializable.
- Use versioned JSON for profiles in the user's local app-data directory; never store profiles in the install directory.
- Treat endpoint IDs as Windows device instance IDs; retain a display-name fallback only for user review, never silent reassignment.
- Keep all real-time callbacks free of allocation, locks, disk I/O, logging, and blocking IPC.
- The default capture source follows the current Windows default render endpoint. An explicitly selected source remains pinned until the user changes it.
- Use Windows process-exclusion loopback to prevent SyncSound's own render streams from feeding back into capture. If the capability is unavailable, block unsafe same-endpoint routing and show the reason.
- Limit v0.1 to eight simultaneously enabled output endpoints. The engine remains structured so a later limit change is configuration, not a redesign.
- Preserve stereo when the endpoint supports it; downmix unsupported multichannel source audio to stereo in v0.1. User-defined surround/channel mapping is post-MVP.
- When an output disappears, keep remaining outputs running. Retry the missing output with bounded exponential backoff for up to 30 seconds, then mark it unavailable and require explicit retry/re-enable.
- Applying a profile is partial and explicit: unavailable devices appear as missing, no device is silently substituted, and the user may manually rebind a missing entry.
- Build both NSIS and MSI. Validate both artifacts, then install and smoke-test the NSIS package at `F:\SyncSound`; upgrades must preserve the app-data profile directory.
- Mode thresholds are scheduling budgets, not acoustic promises: Gaming targets the lowest safe buffer and warns/excludes outputs with excessive or uncertain estimated latency; Balanced uses a conservative shared buffer; Synchronized aligns to the slowest estimated output and reports uncertainty.
- Physical hardware combinations are release-blocking when available. Without the relevant hardware, report the build as a developer preview and list the unverified matrix explicitly.

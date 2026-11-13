# SyncSound v0.1 — Windows multi-output audio router

**Status: Developer preview.** SyncSound captures Windows audio with WASAPI shared-mode loopback and plays it on up to eight output devices at the same time, each with its own gain, mute, delay, buffer, and drift correction. Latency and alignment figures are **estimates** from Windows-reported buffer sizes. They are not acoustic measurements and exclude Bluetooth/HDMI transport delay and speaker distance. Microphone calibration is not part of v0.1.

## What works

- Render endpoint catalog with stable Windows endpoint IDs, default state, format, and best-effort connection type.
- Capture sources:
  - **Windows default output.** This source follows changes to the default device.
  - **Pinned output.** The source stays on one device. SyncSound never switches it to another device automatically.
  - **All apps except SyncSound.** This uses system-wide process-exclusion loopback.
- Feedback guard: an endpoint that is the capture source cannot also be an enabled output. Capture stops with a "Blocked" state instead of looping.
- Per-output event-driven WASAPI render worker. Each worker has its own bounded queue and fill-level drift correction (clamped, gradual resampling), manual delay (0–1000 ms), gain, mute, and float/integer sample formats.
- Gaming / Balanced / Synchronized modes. Synchronized mode delays faster outputs to the slowest Windows-reported device latency.
- Per-output recovery: a removed device retries with backoff (1 s up to 30 s, 8 attempts) without stopping the other outputs.
- Telemetry: buffered ms, estimated latency, drift correction ppm, underruns, and overruns.
- Versioned profiles stored in `%APPDATA%\cloud.syncsound.desktop\profiles`. Missing devices are listed and never matched by name. You rebind them explicitly.
- Diagnostics export (JSON, device IDs anonymised) to `%APPDATA%\cloud.syncsound.desktop\diagnostics`.

## Using it

1. Pick a **Source**. To hear audio on your normal speakers *and* other devices, choose **All apps except SyncSound**. Your default speakers already play the original audio directly. To keep everything aligned, set the Windows default output to a device you are not listening to, or leave it out of the enabled outputs.
2. Turn on the outputs you want, then select **Start routing**.
3. Use **Delay** to fine-tune by ear. Bluetooth devices usually need extra delay on the *other* outputs.

## Speaker setup

Each enabled output has four placement settings:

- **Position:** front, side or rear, left or right.
- **Distance:** how far it is from your seat, 0.2–20 m, optional.
- **Plays:** stereo, stereo with left and right swapped, left only, right only, or a mono mix.
- **Level:** a trim from 0 to −24 dB.

Presets set what each output plays and its level, and keep your gain, delay and positions:

| Preset | Effect |
|---|---|
| Stereo | Every output plays full stereo |
| Front + Rear | Outputs placed at the rear play 3 dB quieter; at least one must be set to Rear |
| Left / Right pair | Exactly two outputs form one stereo pair, assigned by their left/right positions |
| Party mono | Every output plays the same mono mix |

With **Compensate for distance** on, nearer speakers are delayed by 2.9 ms per metre and turned down by inverse distance (never below 25%). That way sound from every speaker reaches your seat together and evenly. If you tuned delays with a microphone at your seat, turn it off, because the measurement already includes distance. All of these settings are saved in profiles.

## Room map, 7.1 and 8D

The **Speaker setup** panel includes a top-down room map. You sit in the centre, the screen is at the top, and the standard 7.1 positions (C, FL, FR, SL, SR, BL, BR) are marked. Drag each enabled output to where it really is. It snaps to the 7.1 positions when it's within 10°. Distance rings run from 1 to 5 m.

Turn on **Spatial sound from this map** to pan the audio to your speakers by their position on the map (pairwise constant-power panning):

- **Stereo:** a virtual left and right channel at ±30°.
- **7.1 surround:** center, front, side and back channels built from the stereo capture. This is a matrix upmix; Windows loopback provides stereo, not discrete 7.1. Outputs marked **Subwoofer** get low-passed bass (about 120 Hz).
- **8D audio:** the whole virtual field rotates around you, one turn every 2–60 s, clockwise or counter-clockwise. The rotation follows the audio timeline, so every output turns in step.
- **Width:** single box, small stereo (±15°), wide pair (±45°) or headphones (±90°). Use Headphones on a single pair of headphones for the classic 8D effect.

Spatial mode changes only what each speaker plays. Delay, gain, level and alignment still apply. All settings are saved in profiles.

## Requirements (Windows 11 x64)

- Node.js 22 or later and npm
- Rust stable (`x86_64-pc-windows-msvc`)
- Visual Studio 2022 Build Tools (Desktop development with C++, Windows SDK)
- Microsoft Edge WebView2 Runtime

## Develop

```powershell
npm ci
npm run desktop:dev
cargo test --workspace
# Native hardware smoke tests (outputs are muted):
cargo test -p syncsound-engine --test windows_capture --test windows_routing -- --ignored --nocapture --test-threads=1
```

## Build and install

```powershell
powershell -ExecutionPolicy Bypass -File .\scripts\build-windows.ps1
powershell -ExecutionPolicy Bypass -File .\scripts\install-windows.ps1 -InstallDir F:\SyncSound -Upgrade
```

The build script fails if either `target/release/bundle/nsis/*.exe` or `target/release/bundle/msi/*.msi` is missing. The install script installs the NSIS build silently into the given directory, launches it, re-installs it when `-Upgrade` is set, and checks that profile data in app data survives. Installers are unsigned.

## Known limitations

- Windows process-exclusion loopback is system-wide. Excluding SyncSound while capturing one specific endpoint is not supported by the Windows API.
- Synchronization is estimated, not measured acoustically. Bluetooth codec/transport latency is not reported by Windows and must be compensated manually.
- Changing delay or mode causes a short audible jump on the affected outputs.
- The capture format is fixed at 48 kHz stereo float. Multichannel content is downmixed by Windows.
- Hardware checks for hotplug, sleep/resume, Bluetooth profile switches, and long sessions are listed as unverified in `docs/verification/2026-11-13-windows-release.md`.

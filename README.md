# SyncSound v0.1 — Windows audio prototype

**Status: Developer preview, not production ready.** This version discovers Windows audio output devices and plays a diagnostic 440 Hz tone independently on selected endpoints. It does **not yet** capture Windows system audio, synchronize device clocks, apply delays, or provide Bluetooth calibration. The UI explicitly communicates these limitations.

## Requirements (Windows 11 x64)

- Node.js 22 or later and npm
- Rust stable (`rustup` with `x86_64-pc-windows-msvc`)
- Visual Studio 2022 Build Tools with **Desktop development with C++** and Windows SDK
- Microsoft Edge WebView2 Runtime (usually included in Windows 11)
- WiX Toolset may be installed by Tauri bundling as required

## Run locally

```powershell
cd syncsound
npm ci
npm run desktop:dev
```

The checkout includes npm and Cargo lockfiles for repeatable builds. The Windows toolchain and check results for this baseline are recorded in [the Windows baseline](docs/baseline/2026-11-13-windows-baseline.md). The `windows` Core Audio bindings are present for upcoming platform work; this task does not add capture or synchronized playback.

**Safety:** Reduce speaker volume before testing; multiple devices will play a continuous 440 Hz tone. Test tone volume defaults to 12% and can be stopped individually or globally.

## Build Windows installers

```powershell
powershell -ExecutionPolicy Bypass -File .\scripts\build-windows.ps1
```

Expected artifacts after a successful Windows build:

- `target/release/bundle/nsis/*.exe`
- `target/release/bundle/msi/*.msi`

No prebuilt installer is included. Builds must be performed on Windows (or through GitHub Actions). Installer binaries are unsigned in this prototype.

## Codex workflow

Open the `syncsound` folder in Codex and use `AGENTS.md` as the project implementation instructions. Start with the WASAPI loopback capture and timestamped shared PCM ring buffer. Do not claim synchronized playback until measured on physical hardware.

## Known limitations

- Uses `cpal` to enumerate devices; v0.1 IDs are session-local enumeration indices and may change on hotplug.
- The diagnostic tone currently supports only float32 output formats; some endpoints will report an unsupported-format error.
- Test tones start independently and are not clock-aligned.
- Windows-only installer build is unverified in this environment.
- No persistent profiles, WASAPI loopback capture, driver, microphone calibration, or virtual surround yet.

## Next milestones

1. Implement WASAPI shared-mode loopback capture with capture endpoint selection and feedback avoidance.
2. Implement timestamped ring-buffer fanout, output format negotiation, and sample-rate conversion.
3. Implement master timeline, device clock estimation, drift compensation, manual delay control.
4. Implement Bluetooth/HDMI calibration and diagnostics.
5. Validate on real hardware; add persistent endpoint IDs, profiles and production installer signing.

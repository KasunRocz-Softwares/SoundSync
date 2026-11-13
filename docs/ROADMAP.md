# SyncSound development roadmap

The current v0.1 developer preview combines the 0.2–0.4 and 0.6 engine work below. Acceptance items that need physical measurement stay open until they are measured. See [the release verification report](verification/2026-11-13-windows-release.md).

| Phase | Deliverable | Status |
|---|---|---|
| 0.1 | Windows Tauri UI and device discovery | Done (WASAPI endpoint catalog with stable IDs) |
| 0.2 | WASAPI loopback and PCM fanout | Implemented; feedback guard verified natively. Acoustic exclusion not measured |
| 0.3 | Timeline and delay offsets | Implemented. Alignment is estimated from Windows-reported latency; not measured acoustically on two wired devices |
| 0.4 | Adaptive resampling and drift control | Implemented and simulated (±300 ppm with jitter). 60-minute hardware run still open |
| 0.5 | Bluetooth and HDMI acoustic calibration | Not started; needs a microphone |
| 0.6 | Profiles and channel mapping | Profiles done (versioned, explicit rebind). Mixer and channel map not started |
| 1.0 | Validated production build and signed installer | Unsigned NSIS/MSI only |

## Key risks
Windows API timestamps often do not show Bluetooth latency, so acoustic calibration is needed. Low latency and close cross-device sync may be mutually exclusive. Exclusive-mode audio bypasses loopback. Process exclusion is system-wide, so selected-endpoint capture uses the same-endpoint block instead.

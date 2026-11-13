# SyncSound development roadmap

| Phase | Deliverable | Acceptance |
|---|---|---|
| 0.1 | Windows Tauri UI, device discovery, diagnostic output | App launches; outputs enumerated; tone plays independently |
| 0.2 | WASAPI loopback and PCM fanout | Windows system audio duplicated without feedback |
| 0.3 | Clock/timestamp pipeline and delay offsets | Measured alignment on two wired devices |
| 0.4 | Adaptive resampling and drift control | 60-minute no-persistent-drift test |
| 0.5 | Bluetooth and HDMI calibration | Acoustic offset measured; degraded behavior reported |
| 0.6 | Profiles, mixer, channel mapping | Settings persist and recover |
| 1.0 | Validated production build and signed installer | Device matrix, tests, signed artifacts |

## Key risks
Bluetooth latency is often unobservable from Windows API timestamps; acoustic calibration is required. Low latency and perfect cross-device sync may be mutually exclusive. Exclusive-mode audio may bypass loopback. Use dedicated capture routing to avoid recapturing outputs.

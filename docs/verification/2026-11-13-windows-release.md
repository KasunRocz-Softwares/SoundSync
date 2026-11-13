# Windows release verification — 2026-11-13

SyncSound v0.1.0, **developer preview**. Host: Windows 11 Pro build 26300, Rust 1.98.1 MSVC, Node 24.20.0, branch `codex/syncsound-windows-audio`.

## Automated checks

| Command | Result |
|---|---|
| `npm ci`, `npm run build` | Passed |
| `cargo test --workspace` | 63 passed (54 engine unit + 9 portable capture), 4 hardware tests ignored by default |
| `cargo check --workspace` | Passed |
| `npm run desktop:build` | Passed; produced both bundles |

Artifacts (unsigned):

| File | Size | SHA256 |
|---|---|---|
| `target/release/bundle/nsis/SyncSound_0.1.0_x64-setup.exe` | 2,121,532 B | `C9E011E886BFA4415C537A52735E3EFB40F874629DBD48208207F4E976B2EA69` |
| `target/release/bundle/msi/SyncSound_0.1.0_x64_en-US.msi` | 3,203,072 B | `C0E9C3166A6F5A3722C41E174B11E33BECACD7C6FE3A5E912C8E1607EA20AAC8` |

These hashes are for the rebuild after the Bluetooth fixes below. The rebuild passed `install-windows.ps1 -Upgrade` again.

## Native hardware smoke tests (outputs muted)

`cargo test -p syncsound-engine --test windows_routing -- --ignored --nocapture --test-threads=1`: 2 passed.

- **Two-output routing.** The source was *All apps except SyncSound*, in Synchronized mode. It played on "AI Noise-Canceling Speaker (ASUS Utility)" and "MSI MP223 (NVIDIA HDMI)". Both outputs reached `Running` and rendered more than 57k frames each. Each held 50 ms in its buffer, with a Windows-reported device latency of 22 ms. Drift correction read −90 ppm and −42 ppm. Each output had 1 underrun during startup. After stop, both outputs returned to `Disabled`.
- **Feedback guard.** The source was the Windows default output, and the default endpoint was enabled as an output. Capture went to `Blocked` instead of creating a loop.
- The Task 3 capture smoke tests (`windows_capture`) passed earlier on this host.

## Bluetooth test: iOX 2 headphones + MSI MP223 HDMI

Bluetooth A2DP endpoint: PnP enumerator `BTHENUM`, mix format 44.1 kHz stereo (Windows resamples from the 48 kHz capture). The test was `cargo run -p syncsound-engine --example probe -- route 90 0.25 synchronized "iOX 2" "MSI MP223"`, with "All apps except SyncSound" as the source and outputs at 25% gain (audible).

| Run | Result |
|---|---|
| 1 (original pipeline) | Both outputs ran for 90 s without failure. At the same moments on both outputs (38 s, 45 s), capture discontinuities cut 10–25 ms from the buffer. The ±200 ppm drift correction then took more than a minute to refill, so estimated latency wandered between 36 and 70 ms. |
| 2 (silence-fill of timestamp gaps) | Regression. The source pauses when nothing plays, the renderer had already played silence for the underrun, and the fill counted that gap twice. The buffer stuck near 185 ms on both outputs. This approach was reverted. |
| 3 (one-shot resync after a discontinuity) | Stable for 90 s. Estimated latency was 63.4 ms on the headphones and 62.0 ms on HDMI for the whole run, with 1 startup underrun each and 0 overruns. Only one discontinuity happened this run, so the resync path ran once on hardware; unit tests cover the rest. |

Windows reports 23.4 ms device latency for the Bluetooth endpoint, about the same as HDMI (22.0 ms). Real A2DP codec and radio delay is not included, so Synchronized mode added no alignment. The UI now tags Bluetooth outputs and tells the user to add delay to the other outputs. Acoustic offset was **not measured**.

## Install at F:\SyncSound

`scripts/install-windows.ps1 -InstallDir F:\SyncSound -Upgrade`, exit 0:

- `F:\SyncSound` did not exist before install, so no unrelated files were overwritten.
- The NSIS silent install with `/D=F:\SyncSound` returned exit 0. Installed files: `syncsound-desktop.exe`, `uninstall.exe`.
- The installed app launched and stayed running for 6 s, then closed.
- The upgrade (reinstall over the existing install) returned exit 0, and the app launched again.
- The sentinel profile in `%APPDATA%\cloud.syncsound.desktop\profiles` was preserved.
- The installed app rendered the routing dashboard and listed 7 active endpoints (checked by screenshot).
- Uninstall was **not** run, so the install stays in place. `install-windows.ps1 -Uninstall` performs that check.

## Unverified (needs physical testing)

- Acoustic alignment between outputs, and whether process exclusion actually keeps SyncSound out of the captured audio.
- Actual Bluetooth acoustic offset (needs a microphone or listening test), A2DP/HFP profile switches, and HDMI transport latency.
- Unplug/replug during playback (the policy is unit-tested, but no physical hotplug was done), sleep/resume, and changing the default device mid-session.
- Integer-format fallback on a device that refuses float32 auto-convert.
- 60-minute drift run on real hardware (drift handling has only been simulated at ±300 ppm with jitter).
- MSI install path. Only the NSIS installer was installed.

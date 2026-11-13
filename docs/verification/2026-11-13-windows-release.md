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
| `target/release/bundle/nsis/SyncSound_0.1.0_x64-setup.exe` | 2,118,115 B | `3F2BDE6EFEEE41DA450F29714A8AE55E2E83089E178A011EC27ACE366448657A` |
| `target/release/bundle/msi/SyncSound_0.1.0_x64_en-US.msi` | 3,198,976 B | `0F2773D1BA00748E29ABF43820F69979CD679280C9C88D9CF6B8348DC1FAF82C` |

## Native hardware smoke tests (outputs muted)

`cargo test -p syncsound-engine --test windows_routing -- --ignored --nocapture --test-threads=1`: 2 passed.

- **Two-output routing.** The source was *All apps except SyncSound*, in Synchronized mode. It played on "AI Noise-Canceling Speaker (ASUS Utility)" and "MSI MP223 (NVIDIA HDMI)". Both outputs reached `Running` and rendered more than 57k frames each. Each held 50 ms in its buffer, with a Windows-reported device latency of 22 ms. Drift correction read −90 ppm and −42 ppm. Each output had 1 underrun during startup. After stop, both outputs returned to `Disabled`.
- **Feedback guard.** The source was the Windows default output, and the default endpoint was enabled as an output. Capture went to `Blocked` instead of creating a loop.
- The Task 3 capture smoke tests (`windows_capture`) passed earlier on this host.

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
- Bluetooth endpoints and A2DP/HFP profile switches. HDMI transport latency.
- Unplug/replug during playback (the policy is unit-tested, but no physical hotplug was done), sleep/resume, and changing the default device mid-session.
- Integer-format fallback on a device that refuses float32 auto-convert.
- 60-minute drift run on real hardware (drift handling has only been simulated at ±300 ppm with jitter).
- MSI install path. Only the NSIS installer was installed.

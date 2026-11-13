# Windows runnable baseline — 2026-11-13

This baseline covers the existing Tauri 2 + React diagnostic-tone shell on Windows 11 x64. It does not implement WASAPI loopback capture or synchronized rendering.

## Environment and source inventory

Run from `E:\KR Softwares\SyncSound\syncsound` on branch `codex/syncsound-windows-audio`:

```text
node --version                  v24.20.0
npm --version                   11.19.0
rustc --version                 rustc 1.98.1 (48a229cea 2026-09-01)
cargo --version                 cargo 1.98.1 (797e8a9bc 2026-08-05)
rustup target list --installed  x86_64-pc-windows-msvc
git status --short --branch     ## codex/syncsound-windows-audio
```

Prerequisite inventory:

- MSVC target: installed (`x86_64-pc-windows-msvc`). Visual Studio Community 2022 version `17.14.37628.2` is installed with `Microsoft.VisualStudio.Component.VC.Tools.x86.x64`; `link.exe` exists at `C:\Program Files\Microsoft Visual Studio\2022\Community\VC\Tools\MSVC\14.44.35207\bin\Hostx64\x64\link.exe`.
- Windows SDK: installed. Include and Lib directories for `10.0.22621.0` and `10.0.26100.0` exist; `10.0.26100.0\um\x64\windowsapp.lib` exists.
- WebView2: `C:\Program Files (x86)\Microsoft\EdgeWebView\Application\157.0.4341.46\msedgewebview2.exe` exists. The queried EdgeUpdate Client keys under HKLM, HKLM WOW6432Node, and HKCU for `{F1E7C1D5-0195-4B7C-A95E-2CE3D3D51B6B}` were absent, so runtime registration and launch are unverified.
- Source inventory: Rust workspace has `syncsound-engine` and `syncsound-desktop`; frontend is React 18, TypeScript, Vite 6, and Tauri 2. The fresh checkout lacked `package-lock.json`, `Cargo.lock`, and the Tauri Windows icon.

## Initial checks

| Command | Result | Relevant output |
|---|---|---|
| `npm ci` | Failed, exit 1 | `npm error code EUSAGE`; `The npm ci command can only install with an existing package-lock.json or npm-shrinkwrap.json` |
| `npm run build` | Failed, exit 1 | `'tsc' is not recognized as an internal or external command` |
| `cargo test --workspace` | Failed, exit 1 | Tauri build script: `icons/icon.ico` not found; required for a Windows Resource file |
| `cargo check --workspace` | Failed, exit 1 | Same missing icon error |

After creating the lockfile and icon, `cargo test --workspace` exposed a further existing source error: `E0277`, because `cpal::Stream` is not `Send` and `tauri::Builder::manage` requires shared state to be `Send + Sync`. The related Tauri command state arguments failed for the same reason. The desktop command adapter now owns `Engine` on one thread and sends its existing start, stop, gain, and stop-all requests through a channel; this preserves the command interface and diagnostic-tone behavior.

## Baseline changes

- Added npm and Cargo lockfiles so `npm ci` and Rust builds resolve consistently.
- Added the Tauri-required Windows icon and ignored generated Tauri schema files.
- Added `windows` 0.62 as a Windows-only engine dependency with `Win32_Foundation`, `Win32_Media_Audio`, `Win32_Media_Audio_Endpoints`, `Win32_System_Com`, `Win32_System_Com_StructuredStorage`, and `Win32_UI_Shell_PropertiesSystem`. These cover Core Audio endpoint enumeration, clients, capture/render interfaces, property stores, and device notifications. No Windows types enter the platform-independent engine API.
- Moved the existing desktop command adapter to a single engine-owning thread to satisfy Tauri's state bounds.

## Final checks

| Command | Result | Relevant output |
|---|---|---|
| `npm ci` | Passed, exit 0 | `added 28 packages, and audited 29 packages in 6s`; `found 0 vulnerabilities` |
| `npm run build` | Passed, exit 0 | `tsc && vite build`; `1578 modules transformed`; `built in 1.72s` |
| `cargo test --workspace` | Passed, exit 0 | Desktop lib, desktop main, engine, and doc-test targets all passed; each reported `running 0 tests` (final rerun: 11.00s) |
| `cargo check --workspace` | Passed, exit 0 | Final rerun: `Finished dev profile [unoptimized + debuginfo] target(s) in 1.54s` |

`npm ci` also warned that `esbuild@0.25.12` has a postinstall script not covered by npm `allowScripts`. The build completed despite this warning.

## Not verified

- Tauri desktop launch, WebView2 runtime initialization, and a Windows installer build were not run.
- Audio endpoint enumeration, test-tone playback, sound level, hotplug behavior, and physical hardware were not tested.
- WASAPI loopback capture, multi-output rendering, clock alignment, and installer signing are not part of this baseline.

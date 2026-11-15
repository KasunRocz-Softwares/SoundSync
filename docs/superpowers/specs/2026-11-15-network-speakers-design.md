# Network speakers: design

Date: 2026-11-15 · Target release: 0.8.0 · Status: draft for approval

## Intent

Let SyncSound play the PC's audio on **phones, other PCs and smart TVs on the same Wi-Fi**, in sync with the PC's own speakers. The device opens a web page (scan a QR code, or type an address); nothing is installed on it.

**Why:** more downloads. This is the headline feature for 0.8.0, the one to show in TikTok/Facebook videos and on the website ("turn your phone into a speaker for your PC, in sync"). It fits the slogan "every speaker you own".

**Success:** someone with only a PC and a phone can, in under a minute, scan a QR code and hear their PC's audio on the phone, lined up with the PC speakers. It has to look and feel good enough to film.

## Goals

- A network device becomes an **output** like any other on the Routing tab: volume, mute, delay, room-map position, EQ, Sound settings, mic calibration, profiles, hotkeys, the 16-output limit.
- Joining works from the browsers people already have: iPhone Safari, Android Chrome, desktop Chrome/Edge/Firefox, and the Samsung (Tizen), LG (webOS) and Android TV browsers.
- Latency of roughly 150–300 ms on normal Wi-Fi, which the existing delay and calibration tools absorb (max delay stays 1,000 ms).
- It keeps playing for hours without slowly drifting, using the same drift-correction idea as local outputs.
- Private by default: the feature is off until turned on. **Scanning the QR code shown on the PC connects a phone straight away**; a device that types the address must be allowed on the PC.

## Non-goals (0.8.0)

- A SyncSound receiver app for other PCs (approach B). It may come later.
- DLNA, Chromecast, AirPlay or Sonos (approach C). Their 1–3 s buffers fight synchronisation.
- Internet or remote listening outside the local network.
- Sending audio *from* phones to the PC.
- Compressed audio (Opus or AAC). The stream is 16-bit PCM; see Constraints.

## Anti-goals (failing these is a failure even if it "works")

- A feature that only works on the developer's own phone. The browser matrix above is the bar.
- Anyone on the Wi-Fi being able to listen without the PC owner noticing.
- Local outputs getting worse (glitches or extra latency) because a network device joined or left.
- A demo that needs a manual or a firewall tweak most people won't do.

## Constraints found in the project and the platform

- **Plain HTTP on the LAN is not a "secure context".** On `http://192.168.x.x` browsers disable AudioWorklet, WebCodecs, Wake Lock and `getUserMedia`. HTTPS would need a certificate the phone trusts, which isn't possible for a LAN IP without warnings. So the page must play audio with features available over HTTP: an `AudioContext` with **scheduled `AudioBufferSourceNode` chunks**. This works on iOS, Android and TV browsers.
- **iOS specifics:**
  - Web Audio starts only after a tap.
  - The ring/silent switch mutes Web Audio unless an `<audio>` element is playing (a silent looping `<audio>` element switches the session to "playback").
  - Locking the screen pauses web audio. The page must say "keep this page open, screen on". It uses a no-sleep fallback (a muted looping video) because the Wake Lock API needs HTTPS.
- **Windows Firewall** shows its "allow access" prompt the first time SyncSound listens on the network. The installer is per-user (no admin), so it can't pre-add a rule; the app explains the prompt before it appears ("Choose Allow on Private networks").
- **Bandwidth:** 48 kHz stereo 16-bit PCM is about 1.5 Mbit/s per device. That's fine on home Wi-Fi for several devices. When the engine runs at 96 kHz, network outputs get a 48 kHz stream (resampled), so they stay at 1.5 Mbit/s.
- **Engine rules (AGENTS.md):** no allocation, locks, I/O or blocking in real-time paths. Windows-specific code stays behind interfaces.
- **How outputs work today** (`platform/windows_engine.rs`):
  - Every output is an `OutputSlot` keyed by a Windows endpoint ID, fed by a bounded PCM queue.
  - It is rendered by a `RenderPipeline` (buffer, delay, drift correction, spatial, EQ, enhance, FIR, limiter, meters) inside a WASAPI `RenderWorker` that the device clock drives.
  - Validation (`commands::validate_endpoint_id`) and profiles look outputs up in the endpoint catalog.
  - Synchronized mode aligns outputs to the slowest *reported* latency (`OutputMetrics::set_device_timing` / `set_device_padding`).

## Approaches considered

| | Approach | Trade-off | Decision |
|---|---|---|---|
| **A** | **Web page player.** SyncSound serves a page; devices open it; audio streams over WebSocket. | One feature covers phones, PCs and TVs, nothing to install. Limits: HTTP-only browser features, iPhone screen must stay on, TV browsers vary. | **Chosen** |
| B | SyncSound "receive" mode on other PCs | Best quality and latency for PCs, but no phones or TVs. | Later, maybe |
| C | DLNA / Chromecast push | Reaches TVs with no browser, but 1–3 s buffers that can't be drift-corrected. | Rejected: works against sync |

## Design

### What the user sees

A new card on the Routing tab, under Outputs:

```
┌ Network speakers ─────────────────────────────── [ off ● on ] ┐
│ Play on phones, TVs and other PCs on your Wi-Fi.              │
│  ▣▣▣  Scan with your phone, or open                           │
│  ▣ ▣  http://192.168.1.20:4877                                │
│  ▣▣▣  on a TV or PC on the same Wi-Fi.                        │
│                                                               │
│  Waiting to join:  Pixel 8 (Chrome)       [Allow] [Deny]      │
│  Joined:           Living room TV · 182 ms · Playing          │
└───────────────────────────────────────────────────────────────┘
```

- Turning it on starts the server. The first time, a note explains the Windows Firewall prompt ("choose Allow"), and says to set the Wi-Fi to Private and allow SyncSound in any antivirus firewall if phones can't connect (new Windows 11 networks default to Public).
- A phone that **scans the QR code connects immediately** and appears as a new output (the code carries a one-time token that expires after 10 minutes and is replaced after each use). A device that types the address shows up under **Waiting to join** with a name guessed from its browser ("iPhone (Safari)"); the user can rename it later. **Allow** turns it into an output; **Deny** closes it.
- Allowed devices appear in the Outputs list as normal outputs, tagged **Network**, with their reported latency. They're remembered: when the same device opens the page again, it joins without asking.
- Turning the feature off disconnects everyone and stops the server.

On the device (phone, TV, PC browser):

```
        SyncSound
   ┌──────────────────┐
   │   ▶  Tap to play  │      ← required tap (iOS/Android audio unlock)
   └──────────────────┘
   Waiting for the PC to allow this device…
   → Playing · in sync with "KASUN-PC" · 182 ms
   Keep this page open with the screen on.
   [ Stop ]
```

The page is large, dark, on-brand and readable from a sofa (TV), with a big status line. It is good enough to film.

### Architecture

```
capture ─▶ fan-out queues ─▶ RenderPipeline (per output)
                                  │
              ┌───────────────────┴───────────────────┐
     WASAPI RenderWorker                    NetworkRenderWorker (new)
     (device clock)                         (paced by QPC at the stream rate)
                                                     │ i16 frames, lock-free ring
                                              network thread (new)
                                       HTTP page · WebSocket per device
                                                     │
                                         browser: AudioContext scheduler
```

1. **Network outputs are outputs.**
   - Each allowed device gets a stable ID `net:<device-key>`. The key is random, made by the page and kept in that browser's `localStorage`.
   - The engine merges a list of network endpoints into its endpoint list (`AudioEndpoint` with `connection_type: "Network"` and `bus: "NETWORK"`, `state: Active` while connected), so `endpoint_active`, `validate_endpoint_id`, profiles and the UI work unchanged. `start_output` picks `NetworkRenderWorker` for `net:` IDs.
   - Validation, profiles, calibration and the UI keep working on IDs and need only small changes.
   - A network output is "available" while its page is connected.
2. **`NetworkRenderWorker`** replaces `RenderWorker` for `net:` outputs. It's a thread that wakes every 10 ms and asks the same `RenderPipeline` for the frames due by the QPC clock. It converts them to 16-bit (and resamples to 48 kHz if the engine runs at 96 or 44.1 kHz), then writes them into a preallocated lock-free byte ring. There is no allocation or locking per callback, so all existing processing applies unchanged.
3. **Network thread** (Rust, in the engine crate, behind a small `NetworkServer` interface):
   - An HTTP server on one port (default **4877**; the next free port if taken) serves the page from files embedded in the binary, plus a WebSocket endpoint.
   - It reads each output's ring and sends 20 ms binary frames. Each frame carries a sequence number and the stream position.
   - Control messages are JSON: hello, allow, clock ping, stats.
   - A slow device whose ring fills up drops its own oldest audio and gets resynced, so it never blocks the engine or other outputs.
   - Library choice is left to the builder; something small and synchronous over `std::net` is preferred, so the engine doesn't pull in a big async runtime.
4. **Timing and sync on the page:**
   - The page plays chunks on its `AudioContext` clock with a target buffer of about 150 ms (adjustable).
   - It reports `buffered_ms + outputLatency` every second. The engine feeds that to `OutputMetrics::set_device_timing` as the device's latency. Synchronized mode already aligns every output to the largest `device_latency_frames`, so **it delays the faster local speakers automatically**. The 500 ms `MAX_ALIGN_MS` cap is enough.
   - Mic calibration works unchanged (clicks go through the network output), and it's the precise way to align.
   - **Drift:** the phone's audio clock differs slightly from the PC's. The page keeps its buffer at target by nudging each chunk's `playbackRate` by up to ±0.1% (inaudible), the same idea as the engine's drift correction.
   - After a stall or big jump the page resyncs: it drops to target and reports a discontinuity.
5. **Security:**
   - Off by default.
   - Only private and link-local addresses are answered; public-IP clients are refused.
   - A device that types the address must be allowed on the PC.
   - The QR code carries a one-time join token: scanning it allows that device at once (seeing the PC screen is the permission). The token expires after 10 minutes and is replaced after each use, so a photo of the code can't be reused.
   - Allowed device keys are stored in app settings and can be removed.
   - Nothing is reachable from outside the LAN.
6. **Settings and profiles:**
   - The on/off state and allowed devices live in `AppSettings`.
   - Network outputs are saved in profiles like any output, by `net:` ID. "Missing" works as it does for unplugged devices.

### Website and launch (same release)

- New website section, "Your phone is a speaker now", with a QR-flow screenshot and a short demo.
- A new 20–25 s TikTok/Facebook video built around it, with sound.
- README section and an FAQ: "Why does my iPhone need the screen on?" and "Why does Windows ask about the firewall?".

## Decided (by the user)

- Devices: phones, other PCs and TVs on the same Wi-Fi.
- Approach A: a web page player, nothing to install on the device.
- Purpose: headline feature to drive downloads; it must demo well.

## My calls (please check)

1. Off by default. Scanning the QR code connects at once (one-time token, 10-minute expiry); typing the address needs Allow on the PC. Allowed devices are remembered. (Changed at the user's request: "allow phone device to scan a qr and connect".)
2. 16-bit PCM at 48 kHz, no compression in 0.8.0.
3. Port 4877, falling back to the next free port. The address and QR code are shown in the app.
4. Page target buffer about 150 ms. Synchronized mode uses each device's reported latency; calibration remains the precise tool.
5. Network devices count toward the 16-output limit.
6. Device names guessed from the browser, editable in the app.
7. Network server code stays in the engine crate behind an interface, with no async runtime.

## Left to the builder

- WebSocket/HTTP library, frame header layout, and how the page JS is bundled (static files embedded with `include_bytes!`).
- Exact resampler for the 44.1/96 → 48 kHz network stream (the existing `dsp::resample_interleaved` may do).
- Reconnect backoff on the page, the iOS silent-audio unlock details, the no-sleep fallback.
- Tests:
  - the worker pacing and ring with simulated clocks
  - the page's scheduler logic as a pure JS function under a small test
  - a loopback integration test: engine → WebSocket client in Rust → check sample continuity and that the reported latency reaches `OutputMetrics`.
- Verification on real devices: an iPhone, an Android phone, a second PC browser, and one smart-TV browser.

## Risks

- **TV browsers:** some may lack Web Audio or throttle it. Mitigation: the device matrix in verification; the page shows "This browser can't play SyncSound audio" instead of failing silently.
- **iPhone screen lock:** the page must stay open. It's stated clearly on the page and in the FAQ.
- **Wi-Fi hiccups:** dropouts on weak Wi-Fi. A larger buffer option (300 ms) on the page's settings mitigates this.

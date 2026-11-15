# Network Speakers Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Phones, other PCs and smart TVs on the same Wi-Fi become SyncSound outputs by opening a web page (scan a QR code or type an address), playing the PC's audio in sync with the local speakers.

**Architecture:** A network device is a normal output (`net:<key>`) fed by the existing `RenderPipeline`. A `NetworkWorker` thread paces it by the PC clock instead of a WASAPI device. The 16-bit 48 kHz chunks go through a lock-free queue to a small `std::net` HTTP + WebSocket server in the engine crate. The page (embedded in the binary) schedules `AudioBufferSourceNode`s on its `AudioContext` clock, corrects drift with `playbackRate`, and reports its latency. That latency feeds `OutputMetrics`, so Synchronized mode and calibration align it like any other output.

**Tech Stack:** Rust (engine crate, `tungstenite` sync WebSocket, `qrcode` for SVG), Tauri 2 commands, React/TypeScript UI, plain JS page (no build step), `node --test` for the page scheduler.

**Spec:** `docs/superpowers/specs/2026-11-15-network-speakers-design.md`

## Global Constraints

- Stream format to devices: 48 000 Hz, stereo, 16-bit little-endian PCM. A chunk is 20 ms = 960 frames = 1 920 samples. The engine converts from 44.1/96 kHz when needed.
- HTTP + WebSocket on port 4877; if that port is taken, try 4878–4887 and use the first free one.
- Page playback: target buffer 150 ms (a "weak Wi-Fi" option raises it to 300 ms). Drift correction is `playbackRate` within ±0.001. Latency is reported every 1 s.
- Off by default. Answer only private, loopback and link-local client addresses (10/8, 172.16/12, 192.168/16, 169.254/16, 127/8, fc00::/7, fe80::/10). Refuse all others.
- Joining by QR code uses a one-time token (16 random bytes, hex). It expires 10 minutes after it is shown, is replaced after each use, and allows the device immediately. Joining by typed address needs Allow on the PC.
- Network outputs count toward `MAX_ENABLED_OUTPUTS` (16). Synchronized-mode alignment stays capped by `MAX_ALIGN_MS` (500).
- No allocation, locks, I/O or blocking in `RenderPipeline::render` or the `NetworkWorker` render loop (AGENTS.md). No async runtime in the engine.
- Pages work over plain HTTP, so no AudioWorklet, WebCodecs, Wake Lock or other secure-context-only APIs.
- Site and app copy: no em-dashes (memory rule for site copy), plain English. Website updates ship in the same push as the feature.

## Review Focus

1. **A device disconnects mid-play** (phone locks, Wi-Fi drops, tab closes). Its output turns "unavailable" within 3 s, the other outputs never glitch, and reopening the page rejoins the same output with its settings. Test: Task 6 `disconnect_marks_unavailable_and_rejoin_reuses_output`.
2. **A device stalls, then floods** (backgrounded tab resumes). It must resync to the target buffer, not play seconds late. Tests: Task 7 `scheduler resyncs after a stall`; Task 4 `full_queue_drops_oldest_and_flags_resync`.
3. **The engine runs at 44.1 or 96 kHz.** The device still gets correct-pitch 48 kHz audio. Test: Task 2 `converts_96k_to_48k_at_correct_pitch`.
4. **The same device opens two tabs, or the same key reconnects while the old socket is still open.** The newest connection replaces the old one and there is no duplicate output. Test: Task 5 `same_key_replaces_previous_connection`.
5. **Port 4877 is in use, or a public-IP client connects.** The next port is used and the UI shows the real address; the public client is refused. Tests: Task 5 `falls_back_to_next_free_port`, Task 3 `rejects_public_addresses`.

---

## File Structure

| File | Responsibility |
|---|---|
| `crates/syncsound-engine/src/net/mod.rs` | Module root; public types `NetworkDevice`, `NetworkStatus`, `PendingDevice`, re-exports |
| `crates/syncsound-engine/src/net/convert.rs` | `StreamConverter`: f32 stereo at engine rate → i16 48 kHz chunks, stateful, allocation-free |
| `crates/syncsound-engine/src/net/access.rs` | Address checks, join tokens, device-name guessing from the User-Agent |
| `crates/syncsound-engine/src/net/link.rs` | `DeviceLink`: chunk queue + latency/connection state shared by worker and server |
| `crates/syncsound-engine/src/net/worker.rs` | `NetworkWorker`: paces a `RenderPipeline` by the PC clock into a `DeviceLink` |
| `crates/syncsound-engine/src/net/server.rs` | `NetworkServer`: HTTP page, WebSocket per device, join flow, control messages |
| `crates/syncsound-engine/src/net/page/index.html`, `player.js`, `scheduler.js`, `style.css` | The page, embedded with `include_str!` |
| `crates/syncsound-engine/src/net/page/scheduler.test.mjs` | `node --test` tests for the scheduler |
| `crates/syncsound-engine/tests/network_loopback.rs` | Server + worker + Rust WebSocket client end to end |
| `crates/syncsound-engine/src/platform/windows_engine.rs` | Merge network endpoints, pick the worker type, commands, snapshot |
| `crates/syncsound-engine/src/routing.rs` | `EngineSnapshot.network` |
| `apps/desktop/src-tauri/src/lib.rs`, `system.rs` | Tauri commands, persistence of the on/off state and allowed devices |
| `apps/desktop/src/NetworkCard.tsx`, `types.ts`, `api.ts`, `main.tsx`, `style.css` | Routing-tab card with the QR code, address, waiting list and joined list |
| `README.md`, `site/index.html`, `site/assets/*` | Docs, website section, screenshots |

---

### Task 1: Network module skeleton and shared types

**Files:**
- Create: `crates/syncsound-engine/src/net/mod.rs`
- Modify: `crates/syncsound-engine/src/lib.rs` (add `pub mod net;`), `crates/syncsound-engine/Cargo.toml` (add `tungstenite = "0.24"`, `qrcode = { version = "0.14", default-features = false, features = ["svg"] }`, `rand = "0.8"`)

**Interfaces:**
- Produces:
  - `pub const NET_PREFIX: &str = "net:";`
  - `pub const STREAM_RATE: u32 = 48_000;`
  - `pub const CHUNK_FRAMES: usize = 960;`
  - `pub const DEFAULT_PORT: u16 = 4877;`
  - `pub struct NetworkDevice { pub key: String, pub name: String }` (Serialize/Deserialize, Clone, PartialEq)
  - `pub struct PendingDevice { pub key: String, pub name: String, pub address: String }`
  - `pub struct NetworkStatus { pub enabled: bool, pub url: Option<String>, pub qr_svg: Option<String>, pub pending: Vec<PendingDevice>, pub error: Option<String> }` (Serialize, Default)
  - `pub fn endpoint_id(key: &str) -> String` returns `"net:" + key`
  - `pub fn key_of(id: &str) -> Option<&str>` returns the part after `net:`, if any

- [ ] **Step 1: Write the failing test** in `net/mod.rs`:
  ```rust
  #[test] fn ids_round_trip() {
      assert_eq!(endpoint_id("ab12"), "net:ab12");
      assert_eq!(key_of("net:ab12"), Some("ab12"));
      assert_eq!(key_of("{0.0.0.00000000}.{guid}"), None);
  }
  ```
- [ ] **Step 2:** `cargo test -p syncsound-engine net::` → FAIL (module missing)
- [ ] **Step 3:** Implement the types and functions above; declare the submodules from later tasks as they land.
- [ ] **Step 4:** `cargo test -p syncsound-engine net::` → PASS
- [ ] **Step 5:** Commit `feat(net): network module skeleton`

### Task 2: StreamConverter (engine rate → 48 kHz i16 chunks)

**Files:**
- Create: `crates/syncsound-engine/src/net/convert.rs`

**Interfaces:**
- Produces:
  - `pub struct StreamConverter`
  - `impl StreamConverter { pub fn new(source_rate: u32) -> Self; pub fn push(&mut self, stereo: &[f32], out: &mut impl FnMut(&[i16; CHUNK_FRAMES * 2])); pub fn reset(&mut self) }`
  - `push` takes interleaved stereo at `source_rate`, resamples linearly to 48 kHz (a pass-through when the rates match), clamps to i16, and calls `out` once per completed 960-frame chunk. It uses only fixed internal buffers.

- [ ] **Step 1: Write the failing tests:**
  - `passes_48k_through_as_full_scale_i16`: push 960 frames of `[0.5, -0.5]` at 48 kHz → exactly one chunk, every pair `== [16384, -16384]` (±1).
  - `converts_96k_to_48k_at_correct_pitch`: push 1 s of a 1 kHz sine at 96 kHz → 50 chunks (±1). Counting zero crossings over the output gives 1 000 Hz ±5 Hz.
  - `converts_44k1_to_48k`: push 44 100 frames → 50 chunks (±1).
  - `clamps_overrange`: input 2.0 → 32767, −2.0 → −32768.
  - `reset_drops_partial_chunk`: push 500 frames, reset, push 960 frames → one chunk, with its first sample from the second push.
- [ ] **Step 2:** `cargo test -p syncsound-engine net::convert` → FAIL
- [ ] **Step 3:** Implement. Keep a fractional read phase and the previous frame across calls; no `Vec` growth after `new`.
- [ ] **Step 4:** `cargo test -p syncsound-engine net::convert` → PASS
- [ ] **Step 5:** Commit `feat(net): 48 kHz i16 stream converter`

### Task 3: Access rules (addresses, QR tokens, device names)

**Files:**
- Create: `crates/syncsound-engine/src/net/access.rs`

**Interfaces:**
- Produces:
  - `pub fn is_local(addr: std::net::IpAddr) -> bool`
  - `pub struct JoinTokens` with `new()`, `current(now: Instant) -> String` (makes a new token if there is none or it is older than 10 min), `redeem(token: &str, now: Instant) -> bool` (true once; replaces the token).
  - `pub fn guess_name(user_agent: &str) -> String` returns names like "iPhone (Safari)", "Android phone (Chrome)", "Samsung TV", "LG TV", "Android TV", "Windows PC (Edge)", "Mac (Safari)", falling back to "Browser".
  - `pub fn valid_key(key: &str) -> bool`: 16–64 chars of `[0-9a-f-]`.

- [ ] **Step 1: Write the failing tests:**
  - `rejects_public_addresses`: `is_local` is true for 192.168.1.5, 10.0.0.2, 172.20.1.1, 169.254.3.4, 127.0.0.1, `fe80::1`, `fd00::1`; false for 8.8.8.8, 172.32.0.1, `2001:db8::1`.
  - `qr_token_is_single_use_and_expires`: `redeem(current)` is true, then false for the same token. A token older than 600 s returns false. After a redeem, `current` returns a different token.
  - `guesses_common_device_names`: real UA strings for iPhone Safari, Pixel Chrome, Samsung Tizen TV, LG webOS, Android TV and Windows Edge map to the names above.
  - `validates_keys`: 32-char hex is accepted; `""`, `"../x"` and 100 chars are rejected.
- [ ] **Step 2:** `cargo test -p syncsound-engine net::access` → FAIL
- [ ] **Step 3:** Implement. Tokens come from `rand::rngs::OsRng`, 16 bytes, hex.
- [ ] **Step 4:** `cargo test -p syncsound-engine net::access` → PASS
- [ ] **Step 5:** Commit `feat(net): address checks, QR tokens, device names`

### Task 4: DeviceLink and NetworkWorker

**Files:**
- Create: `crates/syncsound-engine/src/net/link.rs`, `crates/syncsound-engine/src/net/worker.rs`

**Interfaces:**
- Consumes: `StreamConverter` (Task 2); `RenderPipeline::render(&mut self, &mut [f32])`, `OutputMetrics::set_device_timing(u64, u64)` and `record_underrun()` (existing).
- Produces:
  - `pub struct Chunk { pub seq: u64, pub samples: [i16; CHUNK_FRAMES * 2] }`
  - `pub struct DeviceLink` (in `Arc`) with:
    - `new(capacity_chunks: usize)` (capacity 50 = 1 s)
    - `push(&self, chunk: Chunk)`: when full, drops the oldest chunk and sets the resync flag
    - `pop(&self) -> Option<Chunk>`
    - `take_resync(&self) -> bool`
    - `set_connected(&self, bool)`, `connected(&self) -> bool`
    - `report_latency_ms(&self, f32)`, `latency_ms(&self) -> f32`
  - Uses `crossbeam_queue::ArrayQueue<Chunk>` and atomics.
  - `pub struct NetworkWorker` with `spawn(pipeline: RenderPipeline, engine_rate: u32, link: Arc<DeviceLink>, metrics: Arc<OutputMetrics>) -> Self` and `poll(&self) -> crate::render::WorkerPoll` (same states as `RenderWorker`). Drop stops and joins.
  - The worker loop wakes every 10 ms and renders the frames due since start, from `Instant` elapsed × `engine_rate`, rendering at most 100 ms per wake. It feeds `StreamConverter`, pushes chunks while `link.connected()`, and calls `metrics.set_device_timing(0, latency_frames)` from `link.latency_ms()` each wake, with buffer frames = 1 so `device_latency_frames` counts. When not connected it still renders, so the pipeline keeps draining, but discards the output.
  - Move `WorkerPoll` from `platform/windows_render.rs` into `render.rs` (portable, `pub`) and use it from both workers, so `net/` compiles on any OS.

- [ ] **Step 1: Write the failing tests:**
  - `full_queue_drops_oldest_and_flags_resync` (link): capacity 3, push 4 → first pop has `seq == 1`, and `take_resync()` returns true once.
  - `worker_paces_at_real_time` (worker): pipeline fed with a constant signal from a test `BoundedPcmQueue`, link connected. After 500 ms, about 25 chunks (±4) are queued and `seq` is contiguous.
  - `worker_reports_device_latency`: `link.report_latency_ms(180.0)`, wait 50 ms → `metrics.snapshot().device_latency_frames()` is within 1 ms of 180 ms at the engine rate.
  - `disconnected_worker_discards`: link not connected, 300 ms → `pop()` is `None`.
- [ ] **Step 2:** `cargo test -p syncsound-engine net::` → FAIL
- [ ] **Step 3:** Implement `link.rs` and `worker.rs`. Preallocate the render buffer (100 ms of stereo at `engine_rate`) in `spawn`.
- [ ] **Step 4:** `cargo test -p syncsound-engine net::` → PASS
- [ ] **Step 5:** Commit `feat(net): device link and clock-paced network worker`

### Task 5: NetworkServer (HTTP page, WebSocket, join flow)

**Files:**
- Create: `crates/syncsound-engine/src/net/server.rs`; page placeholders `net/page/index.html`, `player.js`, `scheduler.js`, `style.css` (real content comes in Task 7)

**Interfaces:**
- Consumes: `access` (Task 3), `DeviceLink`/`Chunk` (Task 4).
- Produces:
  - `pub enum ServerEvent { Joined { key: String, name: String }, Left { key: String }, Pending(PendingDevice) }`
  - `pub struct NetworkServer` with:
    - `start(allowed: Vec<NetworkDevice>, port: u16) -> std::io::Result<Self>`: binds `0.0.0.0:port`, falling back through port+10.
    - `port() -> u16`
    - `url() -> String`: `http://<first private IPv4 of this PC>:<port>`
    - `qr_url() -> String`: `url()/?join=<token>`
    - `link(key) -> Option<Arc<DeviceLink>>`
    - `allow(key)`, `deny(key)`
    - `events() -> Vec<ServerEvent>` (drains)
    - `pending() -> Vec<PendingDevice>`
    - `stop()`
  - The accept thread spawns one thread per connection. `GET /`, `/player.js`, `/scheduler.js` and `/style.css` return the embedded files with correct content types and `Cache-Control: no-store`. `GET /ws` upgrades to WebSocket.
  - Protocol (JSON text frames, client→server):
    - `{"type":"hello","key","join"?:string}`. The server replies `{"type":"welcome","name","state":"playing"|"waiting"}`, or closes with `{"type":"denied"}`.
    - `{"type":"stats","latency_ms":f32}` once a second.
    - `{"type":"ping","t":f64}` → `{"type":"pong","t"}`.
  - Server→client binary frames are `[u8 kind=1][u64 seq LE][u8 flags (bit0 = resync)][i16 LE × 1920]`.
  - A hello with an allowed key, or a valid `join` token, is allowed at once: a `Joined` event, a new `DeviceLink`, and the device is added to the allowed set. Any other valid key → `Pending` and waits; `allow(key)` then sends welcome. Non-local client addresses are closed before the HTTP parse.
  - The page path is not a file system path; no other routes.

- [ ] **Step 1: Write the failing tests** (in `server.rs`, using `tungstenite::connect` against `127.0.0.1`):
  - `serves_the_page`: GET `/` → 200, `text/html`, body contains `SyncSound`.
  - `qr_token_joins_immediately`: hello with `join = qr token` → welcome `state:"playing"` and a `Joined` event.
  - `typed_address_waits_for_allow`: hello without a token → `state:"waiting"` and a `Pending` event. After `allow(key)` → `{"type":"welcome","state":"playing"}`.
  - `deny_closes_the_socket`.
  - `same_key_replaces_previous_connection`: two sockets with the same key → the first is closed, one link exists, and only one `Joined` event (no duplicate output).
  - `falls_back_to_next_free_port`: occupy 4877 with a `TcpListener`, then `start(.., 4877)` → `port()` is 4878.
  - `streams_chunks_with_header`: allowed client, `link.push(Chunk{seq:7,..})` → client receives a binary frame of 3 850 bytes with `kind == 1` and `seq == 7`.
  - `stats_update_latency`: send stats with 200 → `link.latency_ms() == 200`.
- [ ] **Step 2:** `cargo test -p syncsound-engine net::server` → FAIL
- [ ] **Step 3:** Implement. On socket close, set `link.set_connected(false)` and emit `Left`.
- [ ] **Step 4:** `cargo test -p syncsound-engine net::server` → PASS
- [ ] **Step 5:** Commit `feat(net): LAN HTTP and WebSocket server with QR and approval join`

### Task 6: Engine integration (outputs, commands, snapshot)

**Files:**
- Modify: `crates/syncsound-engine/src/platform/windows_engine.rs`, `crates/syncsound-engine/src/routing.rs`
- Test: `crates/syncsound-engine/tests/network_loopback.rs`

**Interfaces:**
- Consumes: Tasks 1–5.
- Produces:
  - `AudioEngine::set_network(&self, enabled: bool, allowed: Vec<NetworkDevice>) -> Result<NetworkStatus, EngineError>`
  - `AudioEngine::allow_network_device(&self, key: String) -> Result<(), EngineError>`
  - `AudioEngine::deny_network_device(&self, key: String) -> Result<(), EngineError>`
  - `AudioEngine::rename_network_device(&self, key: String, name: String) -> Result<(), EngineError>` (1–40 chars)
  - `AudioEngine::take_network_joins(&self) -> Vec<NetworkDevice>`: devices newly allowed by QR or Allow, for the desktop app to persist
  - `EngineSnapshot.network: NetworkStatus` (`#[serde(default)]`)
  - An `enum OutputWorker { Wasapi(RenderWorker), Network(NetworkWorker) }` with `poll()` replaces `Option<RenderWorker>` in `OutputSlot`.
- Behaviour:
  - Allowed network devices are merged into `self.endpoints` as `AudioEndpoint { id: "net:<key>", name, connection_type: Some("Network"), bus: Some("NETWORK"), state: Active if connected else NotPresent, .. }` after every `refresh_endpoints` and on server events in `tick`.
  - A device that joins for the first time gets an `OutputSlot` and is **enabled automatically** if fewer than 16 outputs are enabled; otherwise it is listed but off.
  - `start_output` uses `NetworkWorker` for `net:` IDs, with the server's `link(key)`.
  - A `Left` event marks the output's lifecycle `failed("Disconnected")`. Retry happens through the existing backoff once the device rejoins (the `Joined` event triggers `start_output`).
  - The QR SVG comes from `qrcode` for `qr_url()` and is refreshed whenever the token changes.
  - The feedback guard ignores `net:` IDs.

- [ ] **Step 1: Write the failing integration test** `tests/network_loopback.rs` (`#[cfg(windows)]`, `#[ignore]` like the other hardware tests, because it needs the real engine): `disconnect_marks_unavailable_and_rejoin_reuses_output`.
  1. Start the engine with network on and a fake key allowed.
  2. Connect a tungstenite client with that key, then start routing.
  3. Receive at least 20 contiguous chunks.
  4. Drop the socket → within 3 s the snapshot output for `net:<key>` has `endpoint_available == false`, and other outputs' underrun counts don't change.
  5. Reconnect → chunks flow again under the same output ID, with its gain unchanged.
- [ ] **Step 2:** Also add a portable unit test `merges_network_endpoints` in `windows_engine.rs` tests (or a helper fn `merge_network_endpoints(&mut Vec<AudioEndpoint>, &[NetworkDevice], connected: &dyn Fn(&str)->bool)` in `net/mod.rs` tested there): an allowed device appears once, with `connection_type == "Network"`, Active only while connected.
- [ ] **Step 3:** `cargo test -p syncsound-engine merges_network_endpoints` → FAIL; implement; → PASS.
- [ ] **Step 4:** `cargo test -p syncsound-engine --test network_loopback -- --ignored --nocapture --test-threads=1` → PASS on the dev PC.
- [ ] **Step 5:** `cargo test --workspace` → all pass. Commit `feat(net): network outputs in the engine`.

### Task 7: The device page

**Files:**
- Create: `crates/syncsound-engine/src/net/page/index.html`, `player.js`, `scheduler.js`, `style.css`, `scheduler.test.mjs`
- Modify: `package.json` (script `"test:page": "node --test crates/syncsound-engine/src/net/page/"`)

**Interfaces:**
- Consumes: the protocol from Task 5.
- Produces:
  - `scheduler.js`, an ES module with no DOM:
    - `export function createScheduler({ targetMs = 150, rate = 48000 })`
    - It returns `{ enqueue(seq, flags, nowCtxTime) -> { startAt, playbackRate } | null, bufferedMs(nowCtxTime), latencyMs(nowCtxTime, outputLatencySec) }`.
    - The first chunk (or one flagged resync, or after a gap of more than 250 ms in context time) starts at `now + targetMs`.
    - Later chunks start at the previous end; `playbackRate = 1 + clamp((buffered − target) / target × 0.002, ±0.001)`.
    - A chunk that would start in the past returns `null` (dropped) and the next one resyncs.
  - `player.js`:
    1. Makes the device key (`crypto.getRandomValues`, 16 bytes, hex) and keeps it in `localStorage`.
    2. Reads `?join=`; shows a big "Tap to play" button.
    3. On tap: creates the `AudioContext` and plays a silent looping `<audio>` (iOS silent switch). It also starts the no-sleep fallback (muted looping inline video made from a tiny embedded data URI).
    4. Opens `ws://<host>/ws` and sends hello.
    5. Turns each binary frame into an `AudioBuffer` (i16 → f32) and schedules it with `createScheduler`.
    6. Sends stats every 1 s. Reconnects with backoff 1, 2, 4, 8 s (max 8).
    7. Status line: "Waiting for the PC to allow this device…", "Playing · in sync with <PC name> · 182 ms", "Disconnected, reconnecting…", "This browser can't play SyncSound audio" (no `AudioContext`).
    8. A "Weak Wi-Fi" toggle sets the target to 300 ms. Includes a Stop button.
  - Copy fixed by the spec: "Tap to play", "Keep this page open with the screen on."
  - Style: dark, on-brand teal `#2fc4ab` on `#0b0f16`, large type readable from a sofa, works from 320 px wide up to a 1920 px TV.

- [ ] **Step 1: Write the failing tests** `scheduler.test.mjs`:
  - `first chunk starts at target`: enqueue at t=10 → `startAt ≈ 10.15`.
  - `chunks play back to back`: the second enqueue's `startAt` equals the first `startAt + 0.02`.
  - `rate speeds up when buffered over target`: buffered 300 ms → `playbackRate` > 1 and ≤ 1.001.
  - `rate slows when under target` → ≥ 0.999 and < 1.
  - `scheduler resyncs after a stall`: enqueue at t=10, then the next at t=12 → `startAt ≈ 12.15`, not 10.17.
  - `resync flag restarts at target`.
  - `late chunk is dropped`: returns `null`.
- [ ] **Step 2:** `npm run test:page` → FAIL
- [ ] **Step 3:** Implement `scheduler.js`, then `player.js`, `index.html`, `style.css`.
- [ ] **Step 4:** `npm run test:page` → PASS. Open `http://127.0.0.1:4877` in a desktop browser with a dev build running: it joins as "Windows PC (Edge)", plays and shows a latency.
- [ ] **Step 5:** Commit `feat(net): device web page player`

### Task 8: Desktop app (commands, persistence, Routing-tab card)

**Files:**
- Modify: `apps/desktop/src-tauri/src/lib.rs`, `system.rs`, `build.rs`, `capabilities/default.json`; `apps/desktop/src/types.ts`, `api.ts`, `main.tsx`, `style.css`
- Create: `apps/desktop/src/NetworkCard.tsx`

**Interfaces:**
- Consumes: Task 6 engine API.
- Produces:
  - `AppSettings.network_enabled: bool` (default false) and `AppSettings.network_devices: Vec<NetworkDevice>`.
  - Tauri commands: `set_network(enabled: bool) -> NetworkStatus`, `allow_network_device(key)`, `deny_network_device(key)`, `rename_network_device(key, name)`, `forget_network_device(key)` (removes it from settings and the engine).
  - At startup the saved state is applied. Every poll (`get_snapshot`) also calls `take_network_joins()` and persists new devices, so QR joins are remembered.
  - TS: `type NetworkStatus = { enabled: boolean; url: string | null; qr_svg: string | null; pending: { key: string; name: string; address: string }[]; error: string | null }` on `EngineSnapshot.network`.
  - `NetworkCard` sits on the Routing tab under Outputs, as in the spec sketch:
    - An on/off switch. The first time it's turned on, a note: "Windows may ask about the firewall. Choose Allow on Private networks."
    - The QR SVG at 168 px with the address in large mono text and a Copy button.
    - The waiting list with Allow/Deny, and the joined devices with name, latency, state, Rename and Forget.
  - In the Outputs list, `net:` outputs show a **Network** tag (same style as the existing Bluetooth tag).
- [ ] **Step 1:** Rust unit test in `lib.rs` or `system.rs`: `network_settings_default_off_and_round_trip`. Default `network_enabled == false`; a serialize → deserialize round trip keeps the devices; an old settings JSON without the fields loads with defaults.
- [ ] **Step 2:** `cargo test -p syncsound-desktop` → FAIL; implement the Rust side; → PASS.
- [ ] **Step 3:** Implement the UI; `npm run build` → no type errors.
- [ ] **Step 4:** Install a release build to `F:\SyncSound` (`scripts/build-windows.ps1`, then the silent installer). Turn on Network speakers, scan the QR code with a phone, and check:
  - it plays within a few seconds
  - it shows up as an output
  - Calibrate lines it up
  - locking the phone marks it unavailable, and unlocking rejoins it.
- [ ] **Step 5:** Commit `feat(net): Network speakers card, persistence and commands`

### Task 9: Device matrix, docs, website

**Files:**
- Modify: `README.md`, `site/index.html`, `site/style.css` (if needed), `site/assets/` (new screenshots), `docs/verification/2026-11-15-network-speakers.md` (create)

- [ ] **Step 1:** Run the matrix and record the results in `docs/verification/2026-11-15-network-speakers.md`, with latency after calibration and any problems:
  - iPhone Safari
  - Android Chrome
  - a second PC (Chrome or Edge)
  - one smart-TV browser (whatever is available)
  - a 30-minute run on one phone, checking drift and dropouts.
- [ ] **Step 2:** README: a "Network speakers" section (how to join, QR vs typed address, latency, iPhone screen-on note, firewall prompt, privacy: off by default, LAN only).
- [ ] **Step 3:** Website, in the same commit/push as the feature:
  - a new feature cell "Your phone is a speaker now" with a QR-flow screenshot
  - FAQ entries "Why does my iPhone need the screen on?" and "Why does Windows ask about the firewall?"
  - hero copy mentioning phones and TVs
  - no em-dashes.
- [ ] **Step 4:** `npm run build`, `cargo test --workspace`, `npm run test:page` → all pass.
- [ ] **Step 5:** Commit and push to main: `docs+site: network speakers`. The 0.8.0 release and the promo video are a separate step, done after this plan with the user's go-ahead.

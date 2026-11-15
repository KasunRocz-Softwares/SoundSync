//! LAN server for network speakers: serves the player page, accepts one
//! WebSocket per device, runs the join flow and streams audio chunks.
//!
//! Threads: one accept thread plus one thread per connection. Every piece of
//! shared state sits behind a single mutex that is never held during I/O.
//! Each device key owns one `DeviceLink` that survives reconnects, so the
//! engine's worker for that output keeps feeding the same queue.

use std::collections::{HashMap, VecDeque};
use std::io::{self, Write};
use std::net::{IpAddr, Ipv4Addr, Shutdown, SocketAddr, TcpListener, TcpStream, UdpSocket};
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicU8, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use serde_json::{json, Value};
use tungstenite::protocol::Role;
use tungstenite::{Message, WebSocket};

use super::access::{guess_name, is_local, valid_key, JoinTokens};
use super::http;
use super::link::{Chunk, DeviceLink};
use super::{NetworkDevice, PendingDevice, CHUNK_FRAMES};

const INDEX_HTML: &str = include_str!("page/index.html");
const PLAYER_JS: &str = include_str!("page/player.js");
const SCHEDULER_JS: &str = include_str!("page/scheduler.js");
const STYLE_CSS: &str = include_str!("page/style.css");

/// Chunks each device link can queue (50 x 20 ms = 1 s).
const LINK_CAPACITY: usize = 50;
/// How many ports after the requested one are tried when it is taken.
const PORT_FALLBACKS: u16 = 10;
/// Read timeout while streaming, so control messages never block the writer.
const POLL: Duration = Duration::from_millis(5);
/// Time allowed for the HTTP request head and for the WebSocket hello.
const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(10);
/// A device that stops reading for this long is dropped.
const WRITE_TIMEOUT: Duration = Duration::from_secs(2);
/// Failed QR tokens allowed per address inside `FAIL_WINDOW` before tokens
/// from that address are ignored for `FAIL_WINDOW`.
const MAX_TOKEN_FAILURES: usize = 5;
const FAIL_WINDOW: Duration = Duration::from_secs(60);

/// Binary frame: kind, sequence number, flags, then the samples.
const FRAME_KIND_AUDIO: u8 = 1;
const FLAG_RESYNC: u8 = 1;
const FRAME_LEN: usize = 1 + 8 + 1 + CHUNK_FRAMES * 2 * 2;

/// Per-connection control word, written by the server API and read by the
/// connection thread.
const CTL_WAITING: u8 = 0;
const CTL_PLAY: u8 = 1;
const CTL_DENIED: u8 = 2;
const CTL_CLOSE: u8 = 3;

#[derive(Clone, Debug, PartialEq)]
pub enum ServerEvent {
    Joined { key: String, name: String },
    Left { key: String },
    Pending(PendingDevice),
}

/// A connection registered under a device key.
struct Conn {
    id: u64,
    ctl: Arc<AtomicU8>,
}

#[derive(Default)]
struct TokenFailures {
    times: VecDeque<Instant>,
    blocked_until: Option<Instant>,
}

#[derive(Default)]
struct State {
    /// Allowed device keys and their display names.
    allowed: HashMap<String, String>,
    links: HashMap<String, Arc<DeviceLink>>,
    tokens: JoinTokens,
    events: Vec<ServerEvent>,
    pending: Vec<PendingDevice>,
    /// Connections waiting for Allow, by key.
    waiting: HashMap<String, Conn>,
    /// The live streaming connection for each key.
    current: HashMap<String, Conn>,
    failures: HashMap<IpAddr, TokenFailures>,
    /// Clones of every open socket, so `stop` can shut them down.
    sockets: HashMap<u64, TcpStream>,
    threads: Vec<JoinHandle<()>>,
}

struct Shared {
    state: Mutex<State>,
    stopping: AtomicBool,
    next_id: AtomicU64,
}

impl Shared {
    fn lock(&self) -> MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(|e| e.into_inner())
    }

    fn stopping(&self) -> bool {
        self.stopping.load(Ordering::Acquire)
    }
}

enum Admit {
    Play,
    Wait,
}

pub struct NetworkServer {
    shared: Arc<Shared>,
    port: u16,
    accept: Mutex<Option<JoinHandle<()>>>,
}

impl NetworkServer {
    /// Binds `0.0.0.0:port`, or the first free port up to `port + 10`
    /// (port 0 picks any free port), and starts accepting devices.
    pub fn start(allowed: Vec<NetworkDevice>, port: u16) -> io::Result<Self> {
        let listener = bind(port)?;
        let port = listener.local_addr()?.port();
        let mut state = State::default();
        for d in allowed {
            state.links.entry(d.key.clone()).or_insert_with(|| Arc::new(DeviceLink::new(LINK_CAPACITY)));
            state.allowed.insert(d.key, d.name);
        }
        let shared = Arc::new(Shared {
            state: Mutex::new(state),
            stopping: AtomicBool::new(false),
            next_id: AtomicU64::new(1),
        });
        let accept_shared = shared.clone();
        let accept = std::thread::Builder::new()
            .name("syncsound-net-accept".into())
            .spawn(move || accept_loop(accept_shared, listener))?;
        Ok(Self { shared, port, accept: Mutex::new(Some(accept)) })
    }

    pub fn port(&self) -> u16 {
        self.port
    }

    /// `http://<this PC's private IPv4>:<port>`.
    pub fn url(&self) -> String {
        format!("http://{}:{}", local_ipv4(), self.port)
    }

    /// Page address with the live one-time join token, for the QR code.
    pub fn qr_url(&self) -> String {
        let token = self.shared.lock().tokens.current(Instant::now());
        format!("{}/?join={token}", self.url())
    }

    pub fn link(&self, key: &str) -> Option<Arc<DeviceLink>> {
        self.shared.lock().links.get(key).cloned()
    }

    /// Allows a pending device; its waiting page starts playing.
    pub fn allow(&self, key: &str) {
        let mut st = self.shared.lock();
        if let Some(pos) = st.pending.iter().position(|p| p.key == key) {
            let p = st.pending.remove(pos);
            st.allowed.insert(p.key, p.name);
        }
        if let Some(conn) = st.waiting.remove(key) {
            conn.ctl.store(CTL_PLAY, Ordering::Release);
        }
    }

    /// Refuses a pending device; its socket is told and closed.
    pub fn deny(&self, key: &str) {
        let mut st = self.shared.lock();
        st.pending.retain(|p| p.key != key);
        if let Some(conn) = st.waiting.remove(key) {
            conn.ctl.store(CTL_DENIED, Ordering::Release);
        }
    }

    /// Returns and clears the events since the last call.
    pub fn events(&self) -> Vec<ServerEvent> {
        std::mem::take(&mut self.shared.lock().events)
    }

    pub fn pending(&self) -> Vec<PendingDevice> {
        self.shared.lock().pending.clone()
    }

    /// Closes the listener and every socket. Safe to call more than once.
    pub fn stop(&self) {
        if self.shared.stopping.swap(true, Ordering::AcqRel) {
            return;
        }
        // Wake the blocking accept() so the thread sees `stopping`.
        let wake = SocketAddr::from((Ipv4Addr::LOCALHOST, self.port));
        let _ = TcpStream::connect_timeout(&wake, Duration::from_millis(500));
        if let Some(handle) = self.accept.lock().unwrap_or_else(|e| e.into_inner()).take() {
            let _ = handle.join();
        }
        let threads = {
            let mut st = self.shared.lock();
            for s in st.sockets.values() {
                let _ = s.shutdown(Shutdown::Both);
            }
            std::mem::take(&mut st.threads)
        };
        for t in threads {
            let _ = t.join();
        }
        let mut st = self.shared.lock();
        for link in st.links.values() {
            link.set_connected(false);
        }
        st.current.clear();
        st.waiting.clear();
        st.pending.clear();
    }
}

impl Drop for NetworkServer {
    fn drop(&mut self) {
        self.stop();
    }
}

fn bind(port: u16) -> io::Result<TcpListener> {
    if port == 0 {
        return TcpListener::bind((Ipv4Addr::UNSPECIFIED, 0));
    }
    let mut last = None;
    for p in port..=port.saturating_add(PORT_FALLBACKS) {
        match TcpListener::bind((Ipv4Addr::UNSPECIFIED, p)) {
            Ok(l) => return Ok(l),
            Err(e) => last = Some(e),
        }
    }
    Err(last.unwrap_or_else(|| io::Error::new(io::ErrorKind::AddrInUse, "no free port")))
}

/// The PC's address on the home network: the source address the OS would use
/// to reach the internet (no packet is sent), if it is private.
fn local_ipv4() -> Ipv4Addr {
    let probe = || -> io::Result<IpAddr> {
        let s = UdpSocket::bind((Ipv4Addr::UNSPECIFIED, 0))?;
        s.connect(("8.8.8.8", 80))?;
        Ok(s.local_addr()?.ip())
    };
    match probe() {
        Ok(IpAddr::V4(v4)) if v4.is_private() || v4.is_link_local() => v4,
        _ => Ipv4Addr::LOCALHOST,
    }
}

fn accept_loop(shared: Arc<Shared>, listener: TcpListener) {
    for stream in listener.incoming() {
        if shared.stopping() {
            break;
        }
        let Ok(stream) = stream else { continue };
        let Ok(peer) = stream.peer_addr() else { continue };
        if !is_local(peer.ip()) {
            drop(stream);
            continue;
        }
        let conn_shared = shared.clone();
        let spawned = std::thread::Builder::new()
            .name("syncsound-net-conn".into())
            .spawn(move || serve(conn_shared, stream, peer));
        if let Ok(handle) = spawned {
            let mut st = shared.lock();
            st.threads.retain(|t| !t.is_finished());
            st.threads.push(handle);
        }
    }
}

fn serve(shared: Arc<Shared>, stream: TcpStream, peer: SocketAddr) {
    let id = shared.next_id.fetch_add(1, Ordering::Relaxed);
    if let Ok(clone) = stream.try_clone() {
        shared.lock().sockets.insert(id, clone);
    }
    if !shared.stopping() {
        let _ = serve_http(&shared, stream, peer, id);
    }
    shared.lock().sockets.remove(&id);
}

fn serve_http(shared: &Arc<Shared>, mut stream: TcpStream, peer: SocketAddr, id: u64) -> io::Result<()> {
    stream.set_read_timeout(Some(HANDSHAKE_TIMEOUT))?;
    stream.set_write_timeout(Some(WRITE_TIMEOUT))?;
    let _ = stream.set_nodelay(true);
    let head = http::read_head(&mut stream)?;
    let Some(req) = http::parse(&head) else {
        return stream.write_all(&http::response("400 Bad Request", "text/plain", b"Bad request"));
    };
    let file = match req.path.as_str() {
        "/" => Some(("text/html; charset=utf-8", INDEX_HTML)),
        "/player.js" => Some(("text/javascript; charset=utf-8", PLAYER_JS)),
        "/scheduler.js" => Some(("text/javascript; charset=utf-8", SCHEDULER_JS)),
        "/style.css" => Some(("text/css; charset=utf-8", STYLE_CSS)),
        _ => None,
    };
    if req.method != "GET" {
        return stream.write_all(&http::response("404 Not Found", "text/plain", b"Not found"));
    }
    if let Some((content_type, body)) = file {
        return stream.write_all(&http::response("200 OK", content_type, body.as_bytes()));
    }
    if req.path != "/ws" {
        return stream.write_all(&http::response("404 Not Found", "text/plain", b"Not found"));
    }
    let Some(ws_key) = req.header("sec-websocket-key").filter(|_| req.is_websocket_upgrade()) else {
        return stream.write_all(&http::response("400 Bad Request", "text/plain", b"Expected WebSocket"));
    };
    let accept = tungstenite::handshake::derive_accept_key(ws_key.as_bytes());
    let reply = format!(
        "HTTP/1.1 101 Switching Protocols\r\nUpgrade: websocket\r\nConnection: Upgrade\r\n\
         Sec-WebSocket-Accept: {accept}\r\n\r\n"
    );
    stream.write_all(reply.as_bytes())?;
    let user_agent = req.header("user-agent").unwrap_or("").to_string();
    stream.set_read_timeout(Some(POLL))?;
    let ws = WebSocket::from_raw_socket(stream, Role::Server, None);
    run_device(shared, ws, peer, id, &user_agent);
    Ok(())
}

/// What one poll of the socket produced.
enum Polled {
    Msg(Value),
    Nothing,
    Closed,
}

fn is_timeout(e: &tungstenite::Error) -> bool {
    matches!(e, tungstenite::Error::Io(io) if matches!(io.kind(), io::ErrorKind::WouldBlock | io::ErrorKind::TimedOut))
}

/// Reads at most one message, waiting up to `POLL`. Pings are answered here.
fn poll(ws: &mut WebSocket<TcpStream>) -> Polled {
    match ws.read() {
        Ok(Message::Text(t)) => match serde_json::from_str::<Value>(&t) {
            Ok(v) if v["type"] == "ping" => {
                let _ = ws.send(Message::Text(json!({ "type": "pong", "t": v["t"] }).to_string()));
                Polled::Nothing
            }
            Ok(v) => Polled::Msg(v),
            Err(_) => Polled::Nothing,
        },
        Ok(_) => Polled::Nothing,
        Err(e) if is_timeout(&e) => Polled::Nothing,
        Err(_) => Polled::Closed,
    }
}

fn send_json(ws: &mut WebSocket<TcpStream>, v: Value) -> bool {
    ws.send(Message::Text(v.to_string())).is_ok()
}

fn close(mut ws: WebSocket<TcpStream>) {
    let _ = ws.close(None);
    let _ = ws.flush();
    let _ = ws.get_ref().shutdown(Shutdown::Both);
}

fn run_device(shared: &Arc<Shared>, mut ws: WebSocket<TcpStream>, peer: SocketAddr, id: u64, user_agent: &str) {
    // Wait for the hello.
    let deadline = Instant::now() + HANDSHAKE_TIMEOUT;
    let hello = loop {
        if shared.stopping() || Instant::now() > deadline {
            return close(ws);
        }
        match poll(&mut ws) {
            Polled::Msg(v) if v["type"] == "hello" => break v,
            Polled::Msg(_) | Polled::Nothing => {}
            Polled::Closed => return,
        }
    };
    let key = hello["key"].as_str().unwrap_or("").to_string();
    if !valid_key(&key) {
        send_json(&mut ws, json!({ "type": "denied" }));
        return close(ws);
    }
    let join = hello["join"].as_str().filter(|t| !t.is_empty());
    let guessed = guess_name(user_agent);
    let ctl = Arc::new(AtomicU8::new(CTL_WAITING));

    if let Admit::Wait = admit(shared, &key, join, peer.ip(), &guessed, id, &ctl) {
        if !send_json(&mut ws, json!({ "type": "welcome", "name": guessed, "state": "waiting" })) {
            return forget_waiting(shared, &key, id);
        }
        loop {
            if shared.stopping() {
                return close(ws);
            }
            match ctl.load(Ordering::Acquire) {
                CTL_PLAY => break,
                CTL_DENIED => {
                    send_json(&mut ws, json!({ "type": "denied" }));
                    return close(ws);
                }
                CTL_CLOSE => return close(ws),
                _ => {}
            }
            if let Polled::Closed = poll(&mut ws) {
                return forget_waiting(shared, &key, id);
            }
        }
    }

    let (link, name) = join_device(shared, &key, &guessed, id, &ctl);
    let welcome = send_json(&mut ws, json!({ "type": "welcome", "name": name, "state": "playing" }));
    if welcome {
        stream_audio(shared, &mut ws, &link, &ctl);
    }
    leave(shared, &key, id);
    close(ws);
}

/// Decides whether a hello may play now. Unknown keys become pending.
fn admit(
    shared: &Shared,
    key: &str,
    join: Option<&str>,
    ip: IpAddr,
    guessed: &str,
    id: u64,
    ctl: &Arc<AtomicU8>,
) -> Admit {
    let mut st = shared.lock();
    if st.allowed.contains_key(key) {
        return Admit::Play;
    }
    if let Some(token) = join {
        let now = Instant::now();
        let blocked = st
            .failures
            .get(&ip)
            .and_then(|f| f.blocked_until)
            .is_some_and(|until| now < until);
        if !blocked {
            if st.tokens.redeem(token, now) {
                st.allowed.insert(key.to_string(), guessed.to_string());
                return Admit::Play;
            }
            let f = st.failures.entry(ip).or_default();
            while f.times.front().is_some_and(|t| now.duration_since(*t) >= FAIL_WINDOW) {
                f.times.pop_front();
            }
            f.times.push_back(now);
            if f.times.len() >= MAX_TOKEN_FAILURES {
                f.times.clear();
                f.blocked_until = Some(now + FAIL_WINDOW);
            }
        }
    }
    // Typed address: wait for Allow. A newer socket for the same key replaces
    // an older waiting one.
    if let Some(old) = st.waiting.insert(key.to_string(), Conn { id, ctl: ctl.clone() }) {
        old.ctl.store(CTL_CLOSE, Ordering::Release);
    }
    st.pending.retain(|p| p.key != key);
    let device = PendingDevice { key: key.to_string(), name: guessed.to_string(), address: ip.to_string() };
    st.pending.push(device.clone());
    st.events.push(ServerEvent::Pending(device));
    Admit::Wait
}

/// A waiting device went away before it was allowed.
fn forget_waiting(shared: &Shared, key: &str, id: u64) {
    let mut st = shared.lock();
    if st.waiting.get(key).is_some_and(|c| c.id == id) {
        st.waiting.remove(key);
        st.pending.retain(|p| p.key != key);
    }
}

/// Makes this connection the live one for `key`. A previous live connection
/// is told to close, and no second `Joined` is emitted for it.
fn join_device(shared: &Shared, key: &str, guessed: &str, id: u64, ctl: &Arc<AtomicU8>) -> (Arc<DeviceLink>, String) {
    let mut st = shared.lock();
    let name = st.allowed.entry(key.to_string()).or_insert_with(|| guessed.to_string()).clone();
    let link = st
        .links
        .entry(key.to_string())
        .or_insert_with(|| Arc::new(DeviceLink::new(LINK_CAPACITY)))
        .clone();
    if let Some(old) = st.waiting.remove(key) {
        if old.id != id {
            old.ctl.store(CTL_CLOSE, Ordering::Release);
        }
    }
    st.pending.retain(|p| p.key != key);
    ctl.store(CTL_PLAY, Ordering::Release);
    match st.current.insert(key.to_string(), Conn { id, ctl: ctl.clone() }) {
        Some(old) => old.ctl.store(CTL_CLOSE, Ordering::Release),
        None => st.events.push(ServerEvent::Joined { key: key.to_string(), name: name.clone() }),
    }
    // Stale audio from before this (re)connect is dropped; the first frame
    // sent asks the page to resync.
    while link.pop().is_some() {}
    link.take_resync();
    link.set_connected(true);
    (link, name)
}

/// The connection for `key` closed. Only the live one emits `Left`.
fn leave(shared: &Shared, key: &str, id: u64) {
    let mut st = shared.lock();
    if st.current.get(key).is_some_and(|c| c.id == id) {
        st.current.remove(key);
        if let Some(link) = st.links.get(key) {
            link.set_connected(false);
        }
        st.events.push(ServerEvent::Left { key: key.to_string() });
    }
}

fn stream_audio(shared: &Shared, ws: &mut WebSocket<TcpStream>, link: &DeviceLink, ctl: &AtomicU8) {
    let mut resync = true;
    loop {
        if shared.stopping() || ctl.load(Ordering::Acquire) != CTL_PLAY {
            return;
        }
        // Waits up to POLL, which also paces the loop when there is no audio.
        match poll(ws) {
            Polled::Msg(v) if v["type"] == "stats" => {
                if let Some(ms) = v["latency_ms"].as_f64() {
                    link.report_latency_ms(ms as f32);
                }
            }
            Polled::Msg(_) | Polled::Nothing => {}
            Polled::Closed => return,
        }
        let mut wrote = false;
        while let Some(chunk) = link.pop() {
            let flags = if link.take_resync() || resync { FLAG_RESYNC } else { 0 };
            resync = false;
            if ws.write(Message::Binary(encode(&chunk, flags))).is_err() {
                return;
            }
            wrote = true;
        }
        if wrote && ws.flush().is_err() {
            return;
        }
    }
}

/// `[u8 kind=1][u64 seq LE][u8 flags][i16 LE x 1920]`.
fn encode(chunk: &Chunk, flags: u8) -> Vec<u8> {
    let mut out = Vec::with_capacity(FRAME_LEN);
    out.push(FRAME_KIND_AUDIO);
    out.extend_from_slice(&chunk.seq.to_le_bytes());
    out.push(flags);
    for s in chunk.samples.iter() {
        out.extend_from_slice(&s.to_le_bytes());
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::net::link::Chunk;
    use crate::net::CHUNK_FRAMES;
    use std::io::{Read, Write};
    use std::net::{TcpListener, TcpStream};
    use std::time::{Duration, Instant};
    use tungstenite::stream::MaybeTlsStream;
    use tungstenite::{Message, WebSocket};

    type Client = WebSocket<MaybeTlsStream<TcpStream>>;

    const KEY_A: &str = "0123456789abcdef0123456789abcdef";
    const KEY_B: &str = "fedcba9876543210fedcba9876543210";

    fn device(key: &str) -> NetworkDevice {
        NetworkDevice { key: key.into(), name: "Kitchen phone".into() }
    }

    fn connect(server: &NetworkServer) -> Client {
        let (ws, _) = tungstenite::connect(format!("ws://127.0.0.1:{}/ws", server.port())).unwrap();
        if let MaybeTlsStream::Plain(s) = ws.get_ref() {
            s.set_read_timeout(Some(Duration::from_secs(3))).unwrap();
        }
        ws
    }

    fn send_json(ws: &mut Client, v: serde_json::Value) {
        ws.send(Message::Text(v.to_string())).unwrap();
    }

    fn read_json(ws: &mut Client) -> serde_json::Value {
        loop {
            match ws.read().expect("expected a text frame") {
                Message::Text(t) => return serde_json::from_str(&t).unwrap(),
                Message::Binary(_) | Message::Ping(_) | Message::Pong(_) => continue,
                other => panic!("unexpected frame {other:?}"),
            }
        }
    }

    fn hello(ws: &mut Client, key: &str, join: Option<&str>) -> serde_json::Value {
        let mut v = serde_json::json!({ "type": "hello", "key": key });
        if let Some(j) = join {
            v["join"] = j.into();
        }
        send_json(ws, v);
        read_json(ws)
    }

    fn wait_until(mut f: impl FnMut() -> bool) -> bool {
        let end = Instant::now() + Duration::from_secs(3);
        while Instant::now() < end {
            if f() {
                return true;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        false
    }

    /// Collects events until `f` is satisfied (or a timeout), returning all.
    fn events_until(server: &NetworkServer, f: impl Fn(&[ServerEvent]) -> bool) -> Vec<ServerEvent> {
        let mut all = Vec::new();
        wait_until(|| {
            all.extend(server.events());
            f(&all)
        });
        all
    }

    fn joined(ev: &[ServerEvent]) -> usize {
        ev.iter().filter(|e| matches!(e, ServerEvent::Joined { .. })).count()
    }

    fn token_of(qr_url: &str) -> String {
        qr_url.split("?join=").nth(1).unwrap().to_string()
    }

    fn closed(ws: &mut Client) -> bool {
        loop {
            match ws.read() {
                Ok(Message::Close(_)) | Err(_) => return true,
                Ok(_) => continue,
            }
        }
    }

    #[test]
    fn serves_the_page() {
        let server = NetworkServer::start(vec![], 0).unwrap();
        let mut s = TcpStream::connect(("127.0.0.1", server.port())).unwrap();
        s.write_all(b"GET / HTTP/1.1\r\nHost: x\r\nConnection: close\r\n\r\n").unwrap();
        let mut body = String::new();
        s.read_to_string(&mut body).unwrap();
        assert!(body.starts_with("HTTP/1.1 200"), "{body}");
        assert!(body.contains("Content-Type: text/html"));
        assert!(body.contains("Cache-Control: no-store"));
        assert!(body.contains("SyncSound"));

        let mut s = TcpStream::connect(("127.0.0.1", server.port())).unwrap();
        s.write_all(b"GET /../Cargo.toml HTTP/1.1\r\nHost: x\r\n\r\n").unwrap();
        let mut body = String::new();
        s.read_to_string(&mut body).unwrap();
        assert!(body.starts_with("HTTP/1.1 404"), "{body}");
    }

    #[test]
    fn qr_token_joins_immediately() {
        let server = NetworkServer::start(vec![], 0).unwrap();
        let qr = server.qr_url();
        assert!(qr.starts_with(&server.url()));
        let mut ws = connect(&server);
        let reply = hello(&mut ws, KEY_A, Some(&token_of(&qr)));
        assert_eq!(reply["type"], "welcome");
        assert_eq!(reply["state"], "playing");
        let ev = events_until(&server, |e| joined(e) == 1);
        assert_eq!(joined(&ev), 1);
        assert!(server.link(KEY_A).is_some());
        assert_ne!(token_of(&server.qr_url()), token_of(&qr), "token is replaced after use");
    }

    #[test]
    fn typed_address_waits_for_allow() {
        let server = NetworkServer::start(vec![], 0).unwrap();
        let mut ws = connect(&server);
        let reply = hello(&mut ws, KEY_A, None);
        assert_eq!(reply["type"], "welcome");
        assert_eq!(reply["state"], "waiting");
        let ev = events_until(&server, |e| e.iter().any(|e| matches!(e, ServerEvent::Pending(_))));
        assert!(ev.iter().any(|e| matches!(e, ServerEvent::Pending(p) if p.key == KEY_A)));
        assert_eq!(server.pending().len(), 1);
        assert_eq!(server.pending()[0].address, "127.0.0.1");
        server.allow(KEY_A);
        let reply = read_json(&mut ws);
        assert_eq!(reply["type"], "welcome");
        assert_eq!(reply["state"], "playing");
        assert!(server.pending().is_empty());
        let ev = events_until(&server, |e| joined(e) == 1);
        assert_eq!(joined(&ev), 1);
    }

    #[test]
    fn deny_closes_the_socket() {
        let server = NetworkServer::start(vec![], 0).unwrap();
        let mut ws = connect(&server);
        assert_eq!(hello(&mut ws, KEY_A, None)["state"], "waiting");
        assert!(wait_until(|| !server.pending().is_empty()));
        server.deny(KEY_A);
        assert_eq!(read_json(&mut ws)["type"], "denied");
        assert!(closed(&mut ws));
        assert!(server.pending().is_empty());
        assert!(server.link(KEY_A).is_none());
    }

    #[test]
    fn same_key_replaces_previous_connection() {
        let server = NetworkServer::start(vec![device(KEY_A)], 0).unwrap();
        let mut first = connect(&server);
        assert_eq!(hello(&mut first, KEY_A, None)["state"], "playing");
        let link = server.link(KEY_A).unwrap();
        let mut second = connect(&server);
        assert_eq!(hello(&mut second, KEY_A, None)["state"], "playing");
        assert!(closed(&mut first), "the older socket is closed");
        assert!(Arc::ptr_eq(&link, &server.link(KEY_A).unwrap()), "one link per key");
        std::thread::sleep(Duration::from_millis(100));
        let ev = server.events();
        assert_eq!(joined(&ev), 1, "{ev:?}");
        assert!(!ev.iter().any(|e| matches!(e, ServerEvent::Left { .. })), "{ev:?}");
        assert!(link.connected());
        drop(second);
        let ev = events_until(&server, |e| e.iter().any(|e| matches!(e, ServerEvent::Left { .. })));
        assert!(ev.iter().any(|e| matches!(e, ServerEvent::Left { key } if key == KEY_A)));
        assert!(!link.connected());
    }

    #[test]
    fn falls_back_to_next_free_port() {
        let _busy = TcpListener::bind(("0.0.0.0", 4877)).unwrap();
        let server = NetworkServer::start(vec![], 4877).unwrap();
        assert_eq!(server.port(), 4878);
        assert!(server.url().ends_with(":4878"));
    }

    #[test]
    fn streams_chunks_with_header() {
        let server = NetworkServer::start(vec![device(KEY_A)], 0).unwrap();
        let mut ws = connect(&server);
        assert_eq!(hello(&mut ws, KEY_A, None)["state"], "playing");
        let link = server.link(KEY_A).unwrap();
        assert!(link.connected());
        let mut samples = [0i16; CHUNK_FRAMES * 2];
        samples[0] = -2;
        link.push(Chunk { seq: 7, samples });
        let frame = loop {
            match ws.read().unwrap() {
                Message::Binary(b) => break b,
                _ => continue,
            }
        };
        assert_eq!(frame.len(), 3850);
        assert_eq!(frame[0], 1);
        assert_eq!(u64::from_le_bytes(frame[1..9].try_into().unwrap()), 7);
        assert_eq!(frame[9] & 1, 1, "first frame after connect asks for resync");
        assert_eq!(i16::from_le_bytes([frame[10], frame[11]]), -2);
    }

    #[test]
    fn stats_update_latency() {
        let server = NetworkServer::start(vec![device(KEY_A)], 0).unwrap();
        let mut ws = connect(&server);
        assert_eq!(hello(&mut ws, KEY_A, None)["state"], "playing");
        send_json(&mut ws, serde_json::json!({ "type": "stats", "latency_ms": 200.0 }));
        send_json(&mut ws, serde_json::json!({ "type": "ping", "t": 12.5 }));
        let pong = read_json(&mut ws);
        assert_eq!(pong["type"], "pong");
        assert_eq!(pong["t"], 12.5);
        let link = server.link(KEY_A).unwrap();
        assert!(wait_until(|| link.latency_ms() == 200.0));
    }

    #[test]
    fn bad_tokens_are_rate_limited() {
        let server = NetworkServer::start(vec![], 0).unwrap();
        for _ in 0..5 {
            let mut ws = connect(&server);
            assert_eq!(hello(&mut ws, KEY_B, Some("bad"))["state"], "waiting");
        }
        let mut ws = connect(&server);
        let good = token_of(&server.qr_url());
        assert_eq!(hello(&mut ws, KEY_A, Some(&good))["state"], "waiting", "ignored while blocked");
    }

    #[test]
    fn stop_disconnects_devices() {
        let server = NetworkServer::start(vec![device(KEY_A)], 0).unwrap();
        let mut ws = connect(&server);
        assert_eq!(hello(&mut ws, KEY_A, None)["state"], "playing");
        let link = server.link(KEY_A).unwrap();
        server.stop();
        assert!(closed(&mut ws));
        assert!(!link.connected());
        let addr = std::net::SocketAddr::from(([127, 0, 0, 1], server.port()));
        assert!(TcpStream::connect_timeout(&addr, Duration::from_millis(300)).is_err(), "listener is closed");
    }
}

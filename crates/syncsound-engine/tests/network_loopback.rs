//! Network speaker loopback test: runs the real engine with the network
//! server on and plays to a WebSocket client on this PC. Any other active
//! render endpoint is added MUTED so we can check it is not disturbed.
//! Run with:
//! cargo test -p syncsound-engine --test network_loopback -- --ignored --nocapture --test-threads=1
#![cfg(windows)]
use std::net::TcpStream;
use std::time::{Duration, Instant};
use syncsound_engine::capture::EndpointState;
use syncsound_engine::net::NetworkDevice;
use syncsound_engine::{AudioEngine, CaptureSource, EngineSnapshot, OutputConfig, OutputStatus};
use tungstenite::stream::MaybeTlsStream;
use tungstenite::{Message, WebSocket};

type Client = WebSocket<MaybeTlsStream<TcpStream>>;

const KEY: &str = "0123456789abcdef0123456789abcdef";
const ID: &str = "net:0123456789abcdef0123456789abcdef";
/// Kind + sequence + flags + 960 stereo i16 frames.
const FRAME_LEN: usize = 1 + 8 + 1 + 960 * 2 * 2;

fn connect(port: u16) -> Client {
    let (mut ws, _) = tungstenite::connect(format!("ws://127.0.0.1:{port}/ws")).expect("connect");
    if let MaybeTlsStream::Plain(s) = ws.get_ref() {
        s.set_read_timeout(Some(Duration::from_secs(3))).unwrap();
    }
    let hello = serde_json::json!({ "type": "hello", "key": KEY });
    ws.send(Message::Text(hello.to_string())).unwrap();
    loop {
        match ws.read().expect("welcome") {
            Message::Text(t) => {
                let v: serde_json::Value = serde_json::from_str(&t).unwrap();
                assert_eq!(v["type"], "welcome", "{v}");
                assert_eq!(v["state"], "playing", "{v}");
                return ws;
            }
            _ => continue,
        }
    }
}

/// Reads audio frames until `count` arrive with consecutive sequence numbers.
fn contiguous_chunks(ws: &mut Client, count: usize) -> u64 {
    let deadline = Instant::now() + Duration::from_secs(5);
    let mut run = 0;
    let mut last: Option<u64> = None;
    while run < count {
        assert!(Instant::now() < deadline, "only {run} contiguous chunks in 5 s");
        match ws.read().expect("audio frame") {
            Message::Binary(b) => {
                assert_eq!(b.len(), FRAME_LEN);
                assert_eq!(b[0], 1);
                let seq = u64::from_le_bytes(b[1..9].try_into().unwrap());
                run = if last.is_some_and(|l| seq == l + 1) { run + 1 } else { 1 };
                last = Some(seq);
            }
            _ => continue,
        }
    }
    last.unwrap()
}

fn wait_for(engine: &AudioEngine, within: Duration, f: impl Fn(&EngineSnapshot) -> bool) -> EngineSnapshot {
    let deadline = Instant::now() + within;
    loop {
        let snapshot = engine.snapshot();
        if f(&snapshot) || Instant::now() > deadline {
            return snapshot;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
}

fn net_output(snapshot: &EngineSnapshot) -> Option<&OutputStatus> {
    snapshot.outputs.iter().find(|o| o.endpoint_id == ID)
}

fn print(snapshot: &EngineSnapshot) {
    for o in &snapshot.outputs {
        println!(
            "{} [{}]: {:?} available={} gain={} underruns={} err={:?}",
            o.name, o.endpoint_id, o.state, o.endpoint_available, o.config.gain, o.metrics.underruns, o.last_error
        );
    }
    for e in &snapshot.recent_errors {
        println!("error [{}] {}", e.scope, e.message);
    }
}

#[test]
#[ignore = "runs the real Windows engine and network server"]
fn disconnect_marks_unavailable_and_rejoin_reuses_output() {
    let engine = AudioEngine::spawn();
    let status = engine
        .set_network(true, vec![NetworkDevice { key: KEY.into(), name: "Test phone".into() }])
        .expect("set_network");
    assert!(status.enabled, "server failed: {:?}", status.error);
    assert!(status.qr_svg.as_deref().is_some_and(|s| s.contains("<svg")));
    let url = status.url.expect("url");
    let port: u16 = url.rsplit(':').next().unwrap().parse().unwrap();
    println!("server: {url}");

    // Another output, muted, to check a disconnect leaves it alone.
    let others: Vec<String> = engine
        .refresh_endpoints()
        .unwrap()
        .iter()
        .filter(|e| e.state == EndpointState::Active && !e.id.starts_with("net:"))
        .take(1)
        .map(|e| e.id.clone())
        .collect();
    if others.is_empty() {
        println!("SKIPPED: needs one active Windows render endpoint besides the network speaker");
        return;
    }
    let mut ws = connect(port);
    let snapshot = wait_for(&engine, Duration::from_secs(2), |s| {
        net_output(s).is_some_and(|o| o.config.enabled && o.endpoint_available)
    });
    assert!(
        net_output(&snapshot).is_some_and(|o| o.config.enabled && o.endpoint_available),
        "joined device did not become an enabled output"
    );
    for id in &others {
        engine
            .apply_outputs(
                snapshot
                    .outputs
                    .iter()
                    .map(|o| o.config.clone())
                    .chain(std::iter::once(OutputConfig {
                        endpoint_id: id.clone(),
                        enabled: true,
                        gain: 0.0,
                        muted: true,
                        ..OutputConfig::new("")
                    }))
                    .collect(),
            )
            .unwrap();
    }
    engine.set_gain(ID.into(), 0.4).unwrap();
    engine.set_source(CaptureSource::SystemExcludingSelf).unwrap();
    engine.start().unwrap();

    let last = contiguous_chunks(&mut ws, 20);
    println!("first connection: 20 contiguous chunks, last seq {last}");
    // Settle so start-up underruns are not counted, keep reading meanwhile.
    let settle = Instant::now() + Duration::from_secs(1);
    while Instant::now() < settle {
        contiguous_chunks(&mut ws, 1);
    }
    let before = engine.snapshot();
    print(&before);
    let underruns = |s: &EngineSnapshot, id: &str| {
        s.outputs.iter().find(|o| o.endpoint_id == id).unwrap().metrics.underruns
    };

    // Drop the socket: the output becomes unavailable within 3 s.
    drop(ws);
    let dropped_at = Instant::now();
    let after = wait_for(&engine, Duration::from_secs(3), |s| {
        net_output(s).is_some_and(|o| !o.endpoint_available)
    });
    print(&after);
    let out = net_output(&after).expect("net output");
    assert!(!out.endpoint_available, "still available after 3 s");
    println!("unavailable after {} ms", dropped_at.elapsed().as_millis());
    assert_eq!(out.last_error.as_deref(), Some("Disconnected"));
    for id in &others {
        assert_eq!(underruns(&before, id), underruns(&after, id), "other output underran on disconnect");
    }

    // Reconnect: audio flows again on the same output, same gain.
    let mut ws = connect(port);
    let last = contiguous_chunks(&mut ws, 20);
    println!("second connection: 20 contiguous chunks, last seq {last}");
    let settle = Instant::now() + Duration::from_secs(1);
    while Instant::now() < settle {
        contiguous_chunks(&mut ws, 1);
    }
    let rejoined = engine.snapshot();
    print(&rejoined);
    for id in &others {
        assert_eq!(
            underruns(&before, id),
            underruns(&rejoined, id),
            "other output underran across disconnect and rejoin"
        );
    }
    let out = net_output(&rejoined).expect("net output");
    assert!(out.endpoint_available);
    assert_eq!(out.config.gain, 0.4);
    assert_eq!(rejoined.outputs.iter().filter(|o| o.endpoint_id == ID).count(), 1);
    // Allowed by set_network, so not a new join to persist.
    assert!(engine.take_network_joins().is_empty());

    engine.stop().unwrap();
    let off = engine.set_network(false, Vec::new()).unwrap();
    assert!(!off.enabled);
}

//! Diagnostic: switch the engine's capture format and report what happens.
//! `cargo run -p syncsound-engine --example format_probe -- 96000 2`
fn main() {
    let args: Vec<String> = std::env::args().collect();
    let rate: u32 = args.get(1).and_then(|s| s.parse().ok()).unwrap_or(96_000);
    let channels: u16 = args.get(2).and_then(|s| s.parse().ok()).unwrap_or(2);
    let engine = syncsound_engine::AudioEngine::spawn();
    std::thread::sleep(std::time::Duration::from_millis(300));
    // Like the app: one muted output on and routing running before the switch.
    if let Some(out) = engine
        .refresh_endpoints()
        .unwrap_or_default()
        .into_iter()
        .find(|e| !e.is_default && e.state == syncsound_engine::capture::EndpointState::Active)
    {
        println!("output: {}", out.name);
        let _ = engine.enable(out.id.clone());
        let _ = engine.set_mute(out.id, true);
    }
    println!("start first: {:?}", engine.start());
    std::thread::sleep(std::time::Duration::from_millis(800));
    println!("before: {:?}", engine.snapshot().capture_format);
    let result = engine.set_format(syncsound_engine::PcmFormat {
        sample_rate: rate,
        channels,
    });
    println!("set_format: {result:?}");
    std::thread::sleep(std::time::Duration::from_millis(300));
    println!("after: {:?}", engine.snapshot().capture_format);
    println!("start: {:?}", engine.start());
    std::thread::sleep(std::time::Duration::from_millis(800));
    let s = engine.snapshot();
    println!("capture: {:?}", s.capture_state);
    for e in s.recent_errors {
        println!("error: {} {}", e.scope, e.message);
    }
    let _ = engine.stop();
}

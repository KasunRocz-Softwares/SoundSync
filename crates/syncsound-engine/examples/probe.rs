//! Hardware probe for manual testing.
//!   cargo run -p syncsound-engine --example probe -- list
//!   cargo run -p syncsound-engine --example probe -- route <seconds> <gain 0..1> <mode> <name-substring>...
//! Routes "All apps except SyncSound" to every active endpoint whose name contains
//! one of the substrings, printing metrics once per second.
#[cfg(windows)]
fn main() {
    use std::time::Duration;
    use syncsound_engine::capture::EndpointState;
    use syncsound_engine::commands::parse_mode;
    use syncsound_engine::{AudioEngine, CaptureSource, OutputConfig};

    let args: Vec<String> = std::env::args().skip(1).collect();
    let engine = AudioEngine::spawn();
    let endpoints = engine.refresh_endpoints().expect("enumerate endpoints");
    if args.first().map(String::as_str) == Some("apps") {
        for app in engine.list_apps().expect("list apps") {
            println!(
                "{:<28} {:<32} pid {:>6} {}",
                app.name,
                app.exe,
                app.pid,
                if app.active { "PLAYING" } else { "idle" }
            );
        }
        return;
    }
    if args.first().map(String::as_str) != Some("route") {
        for e in endpoints
            .iter()
            .filter(|e| e.state == EndpointState::Active)
        {
            println!(
                "{}{} | {:?} | {} | bus={:?} | {:?} | {:?}-bit",
                if e.is_default { "* " } else { "  " },
                e.name,
                e.state,
                e.connection_type.as_deref().unwrap_or("?"),
                e.bus,
                e.mix_format,
                e.bits_per_sample
            );
        }
        return;
    }
    let seconds: u64 = args[1].parse().unwrap();
    let gain: f32 = args[2].parse().unwrap();
    let mode = parse_mode(&args[3]).unwrap();
    let filters = &args[4..];
    let outputs: Vec<OutputConfig> = endpoints
        .iter()
        .filter(|e| e.state == EndpointState::Active)
        .filter(|e| {
            filters
                .iter()
                .any(|f| e.name.to_lowercase().contains(&f.to_lowercase()))
        })
        .map(|e| OutputConfig {
            endpoint_id: e.id.clone(),
            enabled: true,
            gain,
            muted: false,
            delay_ms: 0,
            ..OutputConfig::new("")
        })
        .collect();
    assert!(
        !outputs.is_empty(),
        "no active endpoint matched {filters:?}"
    );
    match std::env::var("SYNCSOUND_APP") {
        Ok(app) => engine.set_source(CaptureSource::Apps(vec![app])).unwrap(),
        Err(_) => engine
            .set_source(CaptureSource::SystemExcludingSelf)
            .unwrap(),
    }
    engine.set_mode(mode).unwrap();
    engine.apply_outputs(outputs).unwrap();
    engine.start().unwrap();
    for second in 1..=seconds {
        std::thread::sleep(Duration::from_secs(1));
        let s = engine.snapshot();
        for o in &s.outputs {
            println!(
                "t={second:>3}s {:<45} {:?} buf={:>5.1}ms dev={:>5.1}ms est={:>5.1}ms align={:>5.1}ms ppm={:>7.1} under={} over={} disc={} err={:?}",
                o.name, o.state, o.buffered_ms, o.device_latency_ms, o.estimated_latency_ms, o.align_ms,
                o.metrics.correction_ppm, o.metrics.underruns, o.metrics.overruns, o.metrics.discontinuities,
                o.last_error
            );
        }
    }
    let s = engine.snapshot();
    println!("capture: {:?}", s.capture_state);
    for e in &s.recent_errors {
        println!("error [{}] {}", e.scope, e.message);
    }
    engine.stop().unwrap();
}

#[cfg(not(windows))]
fn main() {}

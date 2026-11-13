//! Native routing smoke test. Requires at least two active render endpoints.
//! Outputs are MUTED: this verifies streams open, render, and recover state, not
//! acoustic results. Run with:
//! cargo test -p syncsound-engine --test windows_routing -- --ignored --nocapture
#![cfg(windows)]
use std::time::{Duration, Instant};
use syncsound_engine::capture::EndpointState;
use syncsound_engine::{AudioEngine, AudioMode, CaptureSource, OutputConfig, OutputLifecycleState};

#[test]
#[ignore = "requires two active Windows render endpoints"]
fn routes_loopback_to_two_muted_outputs() {
    let engine = AudioEngine::spawn();
    let endpoints = engine.refresh_endpoints().expect("enumerate endpoints");
    let active: Vec<_> = endpoints
        .iter()
        .filter(|e| e.state == EndpointState::Active)
        .take(2)
        .collect();
    assert!(
        active.len() == 2,
        "need two active render endpoints, found {}",
        active.len()
    );
    for endpoint in &active {
        println!("output: {} ({:?})", endpoint.name, endpoint.mix_format);
    }
    let outputs: Vec<OutputConfig> = active
        .iter()
        .map(|e| OutputConfig {
            endpoint_id: e.id.clone(),
            enabled: true,
            gain: 0.0,
            muted: true,
            delay_ms: 0,
        })
        .collect();
    engine
        .set_source(CaptureSource::SystemExcludingSelf)
        .unwrap();
    engine.set_mode(AudioMode::Synchronized).unwrap();
    engine.apply_outputs(outputs).unwrap();
    engine.start().unwrap();

    let deadline = Instant::now() + Duration::from_secs(5);
    let snapshot = loop {
        std::thread::sleep(Duration::from_millis(250));
        let snapshot = engine.snapshot();
        let rendering = snapshot.outputs.iter().all(|o| {
            o.state == OutputLifecycleState::Running && o.metrics.rendered_frames > 48_000
        });
        if rendering || Instant::now() > deadline {
            break snapshot;
        }
    };
    println!("capture: {:?}", snapshot.capture_state);
    for output in &snapshot.outputs {
        println!(
            "{}: {:?} rendered={} buffered={:.1}ms device={:.1}ms align={:.1}ms ppm={:.1} underruns={} err={:?}",
            output.name,
            output.state,
            output.metrics.rendered_frames,
            output.buffered_ms,
            output.device_latency_ms,
            output.align_ms,
            output.metrics.correction_ppm,
            output.metrics.underruns,
            output.last_error
        );
    }
    for error in &snapshot.recent_errors {
        println!("error [{}] {}", error.scope, error.message);
    }
    assert!(snapshot.running);
    for output in &snapshot.outputs {
        assert_eq!(
            output.state,
            OutputLifecycleState::Running,
            "{}",
            output.name
        );
        assert!(output.metrics.rendered_frames > 0);
    }

    engine.stop().unwrap();
    std::thread::sleep(Duration::from_millis(150));
    let stopped = engine.snapshot();
    assert!(!stopped.running);
    assert!(stopped
        .outputs
        .iter()
        .all(|o| o.state == OutputLifecycleState::Disabled));
}

#[test]
#[ignore = "requires an active Windows render endpoint"]
fn default_source_blocks_routing_back_to_default_endpoint() {
    let engine = AudioEngine::spawn();
    let endpoints = engine.refresh_endpoints().unwrap();
    let default = endpoints
        .iter()
        .find(|e| e.is_default && e.state == EndpointState::Active)
        .expect("active default endpoint");
    engine.set_source(CaptureSource::DefaultRender).unwrap();
    engine
        .apply_outputs(vec![OutputConfig {
            endpoint_id: default.id.clone(),
            enabled: true,
            gain: 0.0,
            muted: true,
            delay_ms: 0,
        }])
        .unwrap();
    engine.start().unwrap();
    std::thread::sleep(Duration::from_millis(500));
    let snapshot = engine.snapshot();
    println!("capture: {:?}", snapshot.capture_state);
    assert!(matches!(
        snapshot.capture_state,
        syncsound_engine::CaptureState::Blocked { .. }
    ));
    engine.stop().unwrap();
}

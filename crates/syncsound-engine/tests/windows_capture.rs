#![cfg(windows)]
use syncsound_engine::{
    platform::windows::EndpointCatalog, CaptureConfig, CaptureSource, CaptureState, LoopbackCapture,
};

#[test]
#[ignore = "Windows audio service and render endpoint required; run explicitly for hardware smoke"]
fn windows_capture_endpoint_smoke() {
    let catalog = EndpointCatalog::new().expect("Windows COM/audio service unavailable");
    let first = catalog
        .enumerate()
        .expect("Windows endpoint enumeration unavailable");
    assert!(
        !first.is_empty(),
        "No Windows render endpoints; hardware smoke unavailable"
    );
    for endpoint in &first {
        println!("{endpoint:?}");
        assert!(!endpoint.id.is_empty());
    }
    let second = catalog.enumerate().unwrap();
    for endpoint in &first {
        assert!(
            second.iter().any(|other| other.id == endpoint.id),
            "Endpoint disappeared during smoke"
        );
    }
    let (mut capture, mut consumer) = LoopbackCapture::new(CaptureConfig::default()).unwrap();
    capture
        .start()
        .expect("Default render endpoint/loopback unavailable");
    assert!(
        matches!(capture.state(), CaptureState::Capturing(_)),
        "{:?}",
        capture.state()
    );
    let mut samples = [0.0; 960];
    let mut blocks = 0;
    for _ in 0..100 {
        capture.poll().unwrap();
        while consumer.pop_into(&mut samples).is_ok() {
            blocks += 1;
            assert!(samples.iter().all(|s| s.is_finite()));
        }
        std::thread::sleep(std::time::Duration::from_millis(5));
    }
    println!("Endpoint capture started/stopped; packets observed: {blocks}. Zero packets means PCM delivery unverified (source may be idle).");
    capture.stop();
    assert_eq!(capture.state(), &CaptureState::Stopped);
    assert!(consumer.pop_into(&mut samples).is_err());

    // Exercise the real adapter's safety boundary without creating any render stream.
    let id = first
        .iter()
        .find(|endpoint| endpoint.is_default)
        .unwrap()
        .id
        .clone();
    let (mut pinned, mut pending) = LoopbackCapture::new(CaptureConfig {
        source: CaptureSource::Endpoint(id.clone()),
        ..CaptureConfig::default()
    })
    .unwrap();
    pinned.start().unwrap();
    assert!(pinned.set_output_endpoints(vec![id]).is_err());
    assert!(matches!(pinned.state(), CaptureState::Blocked { .. }));
    assert!(
        pending.pop_into(&mut samples).is_err(),
        "Blocked capture retained queued audio"
    );
    pinned.poll().unwrap();
    assert!(matches!(pinned.state(), CaptureState::Blocked { .. }));
    pinned.set_output_endpoints(Vec::new()).unwrap();
    assert!(matches!(pinned.state(), CaptureState::Capturing(_)));
    pinned.stop();
}

#[test]
#[ignore = "Windows process-loopback activation required; run explicitly for runtime smoke"]
fn windows_capture_process_exclusion_smoke() {
    let (mut capture, mut consumer) = LoopbackCapture::new(CaptureConfig {
        source: CaptureSource::SystemExcludingSelf,
        ..CaptureConfig::default()
    })
    .unwrap();
    capture
        .start()
        .expect("Windows process exclusion unavailable; no endpoint fallback permitted");
    assert!(matches!(capture.state(), CaptureState::Capturing(_)));
    let mut samples = [0.0; 960];
    let mut blocks = 0;
    for _ in 0..100 {
        capture.poll().unwrap();
        while consumer.pop_into(&mut samples).is_ok() {
            blocks += 1;
        }
        std::thread::sleep(std::time::Duration::from_millis(5));
    }
    println!("System-wide self-excluding capture packets: {blocks}; acoustic exclusion is not measured by this smoke.");
    capture.stop();
}

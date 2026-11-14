//! Diagnostic: how many frames per second does loopback capture really deliver
//! at a requested rate? `cargo run --example capture_rate -- 96000`
#[cfg(windows)]
fn main() {
    use syncsound_engine::capture::CaptureConfig;
    use syncsound_engine::platform::windows::LoopbackCapture;
    use syncsound_engine::PcmFormat;
    let rate: u32 = std::env::args()
        .nth(1)
        .and_then(|s| s.parse().ok())
        .unwrap_or(96_000);
    let config = CaptureConfig {
        format: PcmFormat {
            sample_rate: rate,
            channels: 2,
        },
        ..CaptureConfig::default()
    };
    let max = config.max_block_frames;
    // Loopback only delivers while something plays: keep a silent stream open
    // on the default output (a render pipeline that is never fed plays zeros).
    let _silence = {
        use syncsound_engine::platform::windows::EndpointCatalog;
        use syncsound_engine::platform::windows_render::RenderWorker;
        use syncsound_engine::render::{AudioMode, OutputControls, OutputMetrics, RenderPipeline};
        use std::sync::Arc;
        let catalog = EndpointCatalog::new().expect("catalog");
        let default = catalog
            .enumerate()
            .expect("endpoints")
            .into_iter()
            .find(|e| e.is_default)
            .expect("default output");
        let fmt = PcmFormat { sample_rate: 48_000, channels: 2 };
        let (_producer, consumer) = syncsound_engine::BoundedPcmQueue::new(fmt, 8, 480)
            .unwrap()
            .into_spsc();
        let controls = Arc::new(OutputControls::new(0.0, true, 0, AudioMode::Balanced.policy(), 48_000));
        let metrics = Arc::new(OutputMetrics::default());
        let pipeline = RenderPipeline::new(consumer, fmt, 480, controls, metrics.clone()).unwrap();
        (RenderWorker::spawn(default.id, pipeline, fmt, 200_000, metrics, false), _producer)
    };
    std::thread::sleep(std::time::Duration::from_millis(300));
    let (mut capture, mut consumer) = LoopbackCapture::new(config).expect("capture");
    capture.start().expect("start");
    let mut buf = vec![0.0f32; max * 2];
    let mut frames = 0u64;
    let mut gaps = 0u64;
    let mut expected: Option<u64> = None;
    let t0 = std::time::Instant::now();
    let mut warm = true;
    while t0.elapsed().as_secs_f64() < 12.0 {
        let _ = capture.poll();
        while let Ok((first, n)) = consumer.pop_into(&mut buf) {
            if warm && t0.elapsed().as_secs_f64() < 2.0 {
                continue;
            }
            if warm {
                warm = false;
                frames = 0;
            }
            if let Some(e) = expected {
                if e != first {
                    gaps += 1;
                }
            }
            expected = Some(first + n as u64);
            frames += n as u64;
        }
        std::thread::sleep(std::time::Duration::from_millis(2));
    }
    let secs = t0.elapsed().as_secs_f64() - 2.0;
    let measured = frames as f64 / secs;
    println!(
        "requested {rate} Hz: delivered {measured:.0} frames/s ({:+.0} ppm), timeline gaps {gaps}",
        (measured / f64::from(rate) - 1.0) * 1e6
    );
}
#[cfg(not(windows))]
fn main() {}

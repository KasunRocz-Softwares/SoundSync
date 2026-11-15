//! Clock-paced render worker for a network speaker. Instead of waiting on a
//! WASAPI event it wakes every 10 ms, renders the frames due by the PC clock
//! since it started, converts them to 48 kHz i16 chunks and hands them to the
//! network server through a `DeviceLink`. The loop never allocates, locks or
//! does I/O; every buffer is created in `spawn`.

use super::convert::StreamConverter;
use super::link::{Chunk, DeviceLink};
use crate::render::{OutputMetrics, RenderPipeline, WorkerPoll, OUTPUT_CHANNELS};
use std::sync::atomic::{AtomicBool, AtomicU8, Ordering};
use std::sync::Arc;
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

const STARTING: u8 = 0;
const RUNNING: u8 = 1;
const FAILED: u8 = 2;
const STOPPED: u8 = 3;

/// Pacing interval of the render loop.
const WAKE: Duration = Duration::from_millis(10);
/// Most audio rendered in one wake. If more is due (after a stall), the
/// worker skips ahead rather than rendering a burst.
const MAX_RENDER_MS: u32 = 100;

pub struct NetworkWorker {
    stop: Arc<AtomicBool>,
    state: Arc<AtomicU8>,
    thread: Option<JoinHandle<()>>,
}

/// Everything the loop owns. Built on the control thread so the loop never
/// allocates.
struct Loop {
    pipeline: RenderPipeline,
    engine_rate: u32,
    link: Arc<DeviceLink>,
    metrics: Arc<OutputMetrics>,
    converter: StreamConverter,
    buffer: Vec<f32>,
    max_frames: u64,
}

impl NetworkWorker {
    pub fn spawn(
        pipeline: RenderPipeline,
        engine_rate: u32,
        link: Arc<DeviceLink>,
        metrics: Arc<OutputMetrics>,
    ) -> Self {
        let engine_rate = engine_rate.max(1);
        let max_frames = (u64::from(engine_rate) * u64::from(MAX_RENDER_MS) / 1_000).max(1);
        let state_loop = Loop {
            pipeline,
            engine_rate,
            link,
            metrics,
            converter: StreamConverter::new(engine_rate),
            buffer: vec![0.0; max_frames as usize * OUTPUT_CHANNELS],
            max_frames,
        };
        let stop = Arc::new(AtomicBool::new(false));
        let state = Arc::new(AtomicU8::new(STARTING));
        let thread = {
            let stop = stop.clone();
            let state = state.clone();
            std::thread::Builder::new()
                .name("syncsound-net-render".into())
                .spawn(move || {
                    state.store(RUNNING, Ordering::Release);
                    state_loop.run(&stop);
                    state.store(STOPPED, Ordering::Release);
                })
                .ok()
        };
        if thread.is_none() {
            state.store(FAILED, Ordering::Release);
        }
        Self {
            stop,
            state,
            thread,
        }
    }

    pub fn poll(&self) -> WorkerPoll {
        match self.state.load(Ordering::Acquire) {
            STARTING => WorkerPoll::Starting,
            RUNNING => WorkerPoll::Running,
            FAILED => WorkerPoll::Failed("Could not start network render thread".into()),
            _ => WorkerPoll::Stopped,
        }
    }
}

impl Drop for NetworkWorker {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Release);
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

impl Loop {
    fn run(mut self, stop: &AtomicBool) {
        let rate = f64::from(self.engine_rate);
        let start = Instant::now();
        let mut rendered: u64 = 0;
        let mut seq: u64 = 0;
        let mut was_connected = false;
        while !stop.load(Ordering::Acquire) {
            // Buffer frames = 1 so Synchronized-mode alignment includes this
            // output (it skips outputs that report no device buffer).
            let latency = (f64::from(self.link.latency_ms()) / 1_000.0 * rate).round() as u64;
            self.metrics.set_device_timing(1, latency);

            let due = (start.elapsed().as_secs_f64() * rate) as u64;
            let mut frames = due.saturating_sub(rendered);
            if frames > self.max_frames {
                // Stalled: drop the backlog and ask the page to re-sync.
                rendered = due - self.max_frames;
                frames = self.max_frames;
                self.converter.reset();
                self.link.flag_resync();
                self.metrics.record_underrun();
            }
            if frames > 0 {
                let out = &mut self.buffer[..frames as usize * OUTPUT_CHANNELS];
                self.pipeline.render(out);
                rendered += frames;
                let connected = self.link.connected();
                if connected {
                    if !was_connected {
                        self.converter.reset();
                    }
                    let link = &self.link;
                    self.converter.push(out, &mut |samples| {
                        link.push(Chunk {
                            seq,
                            samples: *samples,
                        });
                        seq += 1;
                    });
                }
                was_connected = connected;
            }
            std::thread::sleep(WAKE);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::render::{ms_to_frames, AudioMode, OutputControls};
    use crate::{BoundedPcmQueue, PcmFormat, PcmProducer};
    use std::time::{Duration, Instant};

    const RATE: u32 = 48_000;
    const FORMAT: PcmFormat = PcmFormat {
        sample_rate: RATE,
        channels: 2,
    };

    fn setup() -> (PcmProducer, RenderPipeline, Arc<OutputMetrics>) {
        let (producer, consumer) = BoundedPcmQueue::new(FORMAT, 128, 480).unwrap().into_spsc();
        let controls = Arc::new(OutputControls::new(
            1.0,
            false,
            0,
            AudioMode::Balanced.policy(),
            RATE,
        ));
        let metrics = Arc::new(OutputMetrics::default());
        let pipeline =
            RenderPipeline::new(consumer, FORMAT, 480, controls, metrics.clone()).unwrap();
        (producer, pipeline, metrics)
    }

    /// Queues `frames` of a constant 0.25 signal starting at frame 0.
    fn feed(producer: &mut PcmProducer, frames: usize) {
        let block = vec![0.25f32; 480 * 2];
        for i in 0..frames / 480 {
            producer.push_samples((i * 480) as u64, &block);
        }
    }

    fn wait_running(worker: &NetworkWorker) {
        let deadline = Instant::now() + Duration::from_secs(2);
        while worker.poll() != WorkerPoll::Running {
            assert!(Instant::now() < deadline, "worker never reported Running");
            std::thread::sleep(Duration::from_millis(1));
        }
    }

    #[test]
    fn worker_paces_at_real_time() {
        let (mut producer, pipeline, metrics) = setup();
        feed(&mut producer, ms_to_frames(1_000, RATE) as usize);
        let link = Arc::new(DeviceLink::new(50));
        link.set_connected(true);
        let worker = NetworkWorker::spawn(pipeline, RATE, link.clone(), metrics);
        std::thread::sleep(Duration::from_millis(500));
        drop(worker);
        let mut seqs = Vec::new();
        let mut audible = false;
        while let Some(chunk) = link.pop() {
            audible |= chunk.samples.iter().any(|s| *s != 0);
            seqs.push(chunk.seq);
        }
        assert!(
            (21..=29).contains(&seqs.len()),
            "expected about 25 chunks, got {}",
            seqs.len()
        );
        assert!(
            seqs.windows(2).all(|w| w[1] == w[0] + 1),
            "seq not contiguous: {seqs:?}"
        );
        assert!(audible, "the fed signal reaches the chunks");
    }

    #[test]
    fn worker_reports_device_latency() {
        let (_producer, pipeline, metrics) = setup();
        let link = Arc::new(DeviceLink::new(50));
        link.set_connected(true);
        link.report_latency_ms(180.0);
        let worker = NetworkWorker::spawn(pipeline, RATE, link, metrics.clone());
        wait_running(&worker);
        std::thread::sleep(Duration::from_millis(50));
        let frames = metrics.snapshot().device_latency_frames() as f64;
        let expected = 0.180 * RATE as f64;
        let tolerance = 0.001 * RATE as f64;
        assert!(
            (frames - expected).abs() <= tolerance,
            "device latency {frames} frames, expected {expected}"
        );
        drop(worker);
    }

    #[test]
    fn disconnected_worker_discards() {
        let (mut producer, pipeline, metrics) = setup();
        feed(&mut producer, ms_to_frames(500, RATE) as usize);
        let link = Arc::new(DeviceLink::new(50));
        let worker = NetworkWorker::spawn(pipeline, RATE, link.clone(), metrics.clone());
        wait_running(&worker);
        std::thread::sleep(Duration::from_millis(300));
        assert!(link.pop().is_none());
        assert!(
            metrics.snapshot().rendered_frames > 0,
            "the pipeline keeps draining while disconnected"
        );
        drop(worker);
    }

    #[test]
    fn drop_stops_the_worker() {
        let (_producer, pipeline, metrics) = setup();
        let link = Arc::new(DeviceLink::new(50));
        let worker = NetworkWorker::spawn(pipeline, RATE, link, metrics);
        wait_running(&worker);
        let started = Instant::now();
        drop(worker);
        assert!(started.elapsed() < Duration::from_millis(500));
    }
}

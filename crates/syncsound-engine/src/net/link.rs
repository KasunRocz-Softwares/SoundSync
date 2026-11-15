//! Lock-free hand-off between one output's `NetworkWorker` (real-time side)
//! and the network server that sends its chunks to the device.

use super::CHUNK_FRAMES;
use crossbeam_queue::ArrayQueue;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};

/// One 20 ms chunk of 48 kHz 16-bit interleaved stereo, numbered in order.
#[derive(Clone)]
pub struct Chunk {
    pub seq: u64,
    pub samples: [i16; CHUNK_FRAMES * 2],
}

/// Shared between the worker (producer) and the server (consumer). Every
/// method is lock-free and allocation-free.
pub struct DeviceLink {
    queue: ArrayQueue<Chunk>,
    resync: AtomicBool,
    connected: AtomicBool,
    /// Latency the device page reports, as `f32` bits.
    latency_ms_bits: AtomicU32,
}

impl DeviceLink {
    /// `capacity_chunks` of 20 ms each; 50 holds 1 s.
    pub fn new(capacity_chunks: usize) -> Self {
        Self {
            queue: ArrayQueue::new(capacity_chunks.max(1)),
            resync: AtomicBool::new(false),
            connected: AtomicBool::new(false),
            latency_ms_bits: AtomicU32::new(0f32.to_bits()),
        }
    }

    /// Queues a chunk. When the queue is full the oldest chunk is dropped and
    /// the resync flag is set so the device knows audio was skipped.
    pub fn push(&self, chunk: Chunk) {
        if self.queue.force_push(chunk).is_some() {
            self.flag_resync();
        }
    }

    pub fn pop(&self) -> Option<Chunk> {
        self.queue.pop()
    }

    /// Asks the device to re-sync its playback position.
    pub fn flag_resync(&self) {
        self.resync.store(true, Ordering::Release);
    }

    /// Returns true once per resync request, then clears it.
    pub fn take_resync(&self) -> bool {
        self.resync.swap(false, Ordering::AcqRel)
    }

    pub fn set_connected(&self, on: bool) {
        self.connected.store(on, Ordering::Release);
    }

    pub fn connected(&self) -> bool {
        self.connected.load(Ordering::Acquire)
    }

    /// Stores the playback latency the device reports. Negative or non-finite
    /// values are treated as 0.
    pub fn report_latency_ms(&self, ms: f32) {
        let ms = if ms.is_finite() { ms.max(0.0) } else { 0.0 };
        self.latency_ms_bits.store(ms.to_bits(), Ordering::Relaxed);
    }

    pub fn latency_ms(&self) -> f32 {
        f32::from_bits(self.latency_ms_bits.load(Ordering::Relaxed))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn chunk(seq: u64) -> Chunk {
        Chunk {
            seq,
            samples: [0; CHUNK_FRAMES * 2],
        }
    }

    #[test]
    fn full_queue_drops_oldest_and_flags_resync() {
        let link = DeviceLink::new(3);
        assert!(!link.take_resync());
        for seq in 0..4 {
            link.push(chunk(seq));
        }
        assert_eq!(link.pop().map(|c| c.seq), Some(1));
        assert!(link.take_resync());
        assert!(!link.take_resync(), "resync is reported once");
        assert_eq!(link.pop().map(|c| c.seq), Some(2));
        assert_eq!(link.pop().map(|c| c.seq), Some(3));
        assert!(link.pop().is_none());
    }

    #[test]
    fn connection_and_latency_round_trip() {
        let link = DeviceLink::new(2);
        assert!(!link.connected());
        link.set_connected(true);
        assert!(link.connected());
        assert_eq!(link.latency_ms(), 0.0);
        link.report_latency_ms(180.5);
        assert_eq!(link.latency_ms(), 180.5);
        link.report_latency_ms(f32::NAN);
        assert_eq!(link.latency_ms(), 0.0);
    }
}

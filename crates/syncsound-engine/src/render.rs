//! Portable per-output render pipeline: bounded buffering, delay, gain/mute,
//! fill-level drift correction with clamped gradual resampling, and device sample
//! encoding. The Windows worker calls `RenderPipeline::render` from its event loop;
//! that call never allocates, locks, logs, blocks, or performs I/O.
use crate::dsp::DspError;
use crate::spatial::{virtual_channels, GainMatrix, LowPass, SpatialShared, MAX_VIRTUAL};
use crate::{PcmConsumer, PcmFormat};
use serde::{Deserialize, Serialize};
use std::sync::atomic::{AtomicBool, AtomicI64, AtomicU32, AtomicU64, AtomicU8, Ordering};
use std::sync::Arc;

/// v0.1 limit on simultaneously enabled outputs.
pub const MAX_ENABLED_OUTPUTS: usize = 8;
/// Manual per-output delay bound.
pub const MAX_OUTPUT_DELAY_MS: u32 = 1_000;
/// Callbacks averaged after a capture discontinuity before the one-shot resync.
const RESYNC_WINDOW: u32 = 20;
/// Bound on automatic Synchronized-mode alignment delay per output.
pub const MAX_ALIGN_MS: u32 = 500;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum AudioMode {
    Gaming,
    #[default]
    Balanced,
    Synchronized,
}

/// What a single output plays from the stereo source.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ChannelRole {
    #[default]
    Stereo,
    /// Left and right exchanged (e.g. a stereo speaker placed facing you from behind a wall).
    Swapped,
    /// This output plays the left channel on all its speakers.
    LeftOnly,
    RightOnly,
    /// (L + R) / 2 on all speakers.
    Mono,
}

impl ChannelRole {
    fn to_u8(self) -> u8 {
        match self {
            ChannelRole::Stereo => 0,
            ChannelRole::Swapped => 1,
            ChannelRole::LeftOnly => 2,
            ChannelRole::RightOnly => 3,
            ChannelRole::Mono => 4,
        }
    }
    fn from_u8(value: u8) -> Self {
        match value {
            1 => ChannelRole::Swapped,
            2 => ChannelRole::LeftOnly,
            3 => ChannelRole::RightOnly,
            4 => ChannelRole::Mono,
            _ => ChannelRole::Stereo,
        }
    }
    /// Maps one stereo frame in place.
    #[inline]
    pub fn apply(self, frame: &mut [f32]) {
        if frame.len() != 2 {
            return;
        }
        let (l, r) = (frame[0], frame[1]);
        let (a, b) = match self {
            ChannelRole::Stereo => (l, r),
            ChannelRole::Swapped => (r, l),
            ChannelRole::LeftOnly => (l, l),
            ChannelRole::RightOnly => (r, r),
            ChannelRole::Mono => {
                let m = (l + r) * 0.5;
                (m, m)
            }
        };
        frame[0] = a;
        frame[1] = b;
    }
}

/// Scheduling budgets, not acoustic promises.
#[derive(Clone, Copy, Debug, PartialEq, Serialize)]
pub struct ModePolicy {
    /// Software buffer each output keeps ahead of its render endpoint.
    pub target_buffer_ms: u32,
    /// Clamp for drift-correction resampling.
    pub max_correction_ppm: f64,
    /// Requested WASAPI shared-mode render buffer, in 100 ns units.
    pub render_buffer_hns: i64,
    /// Synchronized mode delays faster outputs to the slowest estimated output.
    pub align_outputs: bool,
    /// Gaming mode warns about outputs whose estimated latency exceeds this budget.
    pub latency_warning_ms: Option<u32>,
}

impl AudioMode {
    pub fn policy(self) -> ModePolicy {
        match self {
            AudioMode::Gaming => ModePolicy {
                target_buffer_ms: 20,
                max_correction_ppm: 500.0,
                render_buffer_hns: 100_000,
                align_outputs: false,
                latency_warning_ms: Some(60),
            },
            AudioMode::Balanced => ModePolicy {
                target_buffer_ms: 40,
                max_correction_ppm: 300.0,
                render_buffer_hns: 200_000,
                align_outputs: false,
                latency_warning_ms: None,
            },
            AudioMode::Synchronized => ModePolicy {
                target_buffer_ms: 60,
                max_correction_ppm: 200.0,
                render_buffer_hns: 200_000,
                align_outputs: true,
                latency_warning_ms: None,
            },
        }
    }
}

pub fn ms_to_frames(ms: u32, sample_rate: u32) -> u32 {
    (u64::from(ms) * u64::from(sample_rate) / 1_000).min(u64::from(u32::MAX)) as u32
}

pub fn frames_to_ms(frames: u64, sample_rate: u32) -> f64 {
    if sample_rate == 0 {
        0.0
    } else {
        frames as f64 * 1_000.0 / f64::from(sample_rate)
    }
}

/// Control-plane → audio-path parameters. Plain atomics only.
#[derive(Debug)]
pub struct OutputControls {
    gain_bits: AtomicU32,
    muted: AtomicBool,
    delay_frames: AtomicU32,
    align_frames: AtomicU32,
    target_frames: AtomicU32,
    max_ppm_bits: AtomicU32,
    /// Speaker-placement compensation, applied on top of the user's gain and delay.
    placement_delay_frames: AtomicU32,
    trim_bits: AtomicU32,
    role: AtomicU8,
    /// Index among spatial outputs (its points are 2·slot and 2·slot+1); 255 = none.
    spatial_slot: AtomicU8,
    subwoofer: AtomicBool,
}

impl OutputControls {
    pub fn new(gain: f32, muted: bool, delay_frames: u32, policy: ModePolicy, rate: u32) -> Self {
        Self {
            gain_bits: AtomicU32::new(gain.clamp(0.0, 1.0).to_bits()),
            muted: AtomicBool::new(muted),
            delay_frames: AtomicU32::new(delay_frames),
            align_frames: AtomicU32::new(0),
            target_frames: AtomicU32::new(ms_to_frames(policy.target_buffer_ms, rate)),
            max_ppm_bits: AtomicU32::new((policy.max_correction_ppm as f32).to_bits()),
            placement_delay_frames: AtomicU32::new(0),
            trim_bits: AtomicU32::new(1.0f32.to_bits()),
            role: AtomicU8::new(ChannelRole::Stereo.to_u8()),
            spatial_slot: AtomicU8::new(u8::MAX),
            subwoofer: AtomicBool::new(false),
        }
    }
    pub fn spatial_slot(&self) -> Option<usize> {
        let slot = self.spatial_slot.load(Ordering::Relaxed);
        (slot != u8::MAX).then_some(usize::from(slot))
    }
    pub fn set_spatial_slot(&self, slot: Option<usize>) {
        let value = slot.map_or(u8::MAX, |s| s.min(254) as u8);
        self.spatial_slot.store(value, Ordering::Relaxed);
    }
    pub fn subwoofer(&self) -> bool {
        self.subwoofer.load(Ordering::Relaxed)
    }
    pub fn set_subwoofer(&self, on: bool) {
        self.subwoofer.store(on, Ordering::Relaxed);
    }
    pub fn placement_delay_frames(&self) -> u32 {
        self.placement_delay_frames.load(Ordering::Relaxed)
    }
    pub fn set_placement_delay_frames(&self, frames: u32) {
        self.placement_delay_frames.store(frames, Ordering::Relaxed);
    }
    /// Linear level multiplier (≤ 1) from placement and level trim.
    pub fn trim(&self) -> f32 {
        f32::from_bits(self.trim_bits.load(Ordering::Relaxed))
    }
    pub fn set_trim(&self, factor: f32) {
        self.trim_bits
            .store(factor.clamp(0.0, 1.0).to_bits(), Ordering::Relaxed);
    }
    pub fn role(&self) -> ChannelRole {
        ChannelRole::from_u8(self.role.load(Ordering::Relaxed))
    }
    pub fn set_role(&self, role: ChannelRole) {
        self.role.store(role.to_u8(), Ordering::Relaxed);
    }
    pub fn gain(&self) -> f32 {
        f32::from_bits(self.gain_bits.load(Ordering::Relaxed))
    }
    pub fn set_gain(&self, gain: f32) {
        self.gain_bits
            .store(gain.clamp(0.0, 1.0).to_bits(), Ordering::Relaxed);
    }
    pub fn muted(&self) -> bool {
        self.muted.load(Ordering::Relaxed)
    }
    pub fn set_muted(&self, muted: bool) {
        self.muted.store(muted, Ordering::Relaxed);
    }
    pub fn delay_frames(&self) -> u32 {
        self.delay_frames.load(Ordering::Relaxed)
    }
    pub fn set_delay_frames(&self, frames: u32) {
        self.delay_frames.store(frames, Ordering::Relaxed);
    }
    pub fn align_frames(&self) -> u32 {
        self.align_frames.load(Ordering::Relaxed)
    }
    pub fn set_align_frames(&self, frames: u32) {
        self.align_frames.store(frames, Ordering::Relaxed);
    }
    pub fn target_frames(&self) -> u32 {
        self.target_frames.load(Ordering::Relaxed)
    }
    pub fn max_correction_ppm(&self) -> f64 {
        f64::from(f32::from_bits(self.max_ppm_bits.load(Ordering::Relaxed)))
    }
    pub fn apply_policy(&self, policy: ModePolicy, rate: u32) {
        self.target_frames.store(
            ms_to_frames(policy.target_buffer_ms, rate),
            Ordering::Relaxed,
        );
        self.max_ppm_bits.store(
            (policy.max_correction_ppm as f32).to_bits(),
            Ordering::Relaxed,
        );
    }
}

/// Audio-path → control-plane counters. Written with relaxed atomics only.
#[derive(Debug, Default)]
pub struct OutputMetrics {
    underruns: AtomicU64,
    overruns: AtomicU64,
    discontinuities: AtomicU64,
    buffered_frames: AtomicU64,
    rendered_frames: AtomicU64,
    correction_ppm_milli: AtomicI64,
    device_buffer_frames: AtomicU64,
    device_padding_frames: AtomicU64,
    device_stream_latency_frames: AtomicU64,
    primed: AtomicBool,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct OutputMetricsSnapshot {
    pub underruns: u64,
    pub overruns: u64,
    pub discontinuities: u64,
    pub buffered_frames: u64,
    pub rendered_frames: u64,
    /// Applied resampling correction relative to the capture clock.
    pub correction_ppm: f64,
    pub device_buffer_frames: u64,
    pub device_padding_frames: u64,
    pub device_stream_latency_frames: u64,
    pub primed: bool,
}

impl OutputMetricsSnapshot {
    /// Software queue + data still queued in the WASAPI endpoint buffer + reported
    /// stream latency. An estimate from Windows-reported values, not an acoustic measurement.
    pub fn estimated_latency_frames(&self) -> u64 {
        self.buffered_frames
            .saturating_add(self.device_padding_frames)
            .saturating_add(self.device_stream_latency_frames)
    }
    /// Endpoint-side latency only (excludes the adjustable software buffer).
    pub fn device_latency_frames(&self) -> u64 {
        self.device_buffer_frames
            .saturating_add(self.device_stream_latency_frames)
    }
}

impl OutputMetrics {
    pub fn snapshot(&self) -> OutputMetricsSnapshot {
        OutputMetricsSnapshot {
            underruns: self.underruns.load(Ordering::Relaxed),
            overruns: self.overruns.load(Ordering::Relaxed),
            discontinuities: self.discontinuities.load(Ordering::Relaxed),
            buffered_frames: self.buffered_frames.load(Ordering::Relaxed),
            rendered_frames: self.rendered_frames.load(Ordering::Relaxed),
            correction_ppm: self.correction_ppm_milli.load(Ordering::Relaxed) as f64 / 1_000.0,
            device_buffer_frames: self.device_buffer_frames.load(Ordering::Relaxed),
            device_padding_frames: self.device_padding_frames.load(Ordering::Relaxed),
            device_stream_latency_frames: self.device_stream_latency_frames.load(Ordering::Relaxed),
            primed: self.primed.load(Ordering::Relaxed),
        }
    }
    pub fn record_overrun(&self) {
        self.overruns.fetch_add(1, Ordering::Relaxed);
    }
    pub fn record_underrun(&self) {
        self.underruns.fetch_add(1, Ordering::Relaxed);
    }
    pub fn record_discontinuity(&self) {
        self.discontinuities.fetch_add(1, Ordering::Relaxed);
    }
    pub fn set_device_timing(&self, buffer_frames: u64, stream_latency_frames: u64) {
        self.device_buffer_frames
            .store(buffer_frames, Ordering::Relaxed);
        self.device_stream_latency_frames
            .store(stream_latency_frames, Ordering::Relaxed);
    }
    pub fn set_device_padding(&self, frames: u64) {
        self.device_padding_frames.store(frames, Ordering::Relaxed);
    }
}

/// Per-output consumer. Owns a preallocated sample ring sized for the maximum
/// delay so delay changes never allocate. Not shared across outputs.
pub struct RenderPipeline {
    consumer: PcmConsumer,
    channels: usize,
    sample_rate: u32,
    block: Vec<f32>,
    ring: Vec<f32>,
    ring_frames: usize,
    read: usize,
    len: usize,
    phase: f64,
    expected_next: Option<u64>,
    primed: bool,
    silence_pending: usize,
    applied_extra: u32,
    smoothed_error: f64,
    correction_ppm: f64,
    resync_callbacks: u32,
    resync_error_sum: f64,
    gain: f32,
    controls: Arc<OutputControls>,
    metrics: Arc<OutputMetrics>,
    spatial: Option<Arc<SpatialShared>>,
    lfe: LowPass,
    lfe_coefficient: f32,
}

impl RenderPipeline {
    /// Control-thread constructor: every buffer used by `render` is allocated here.
    pub fn new(
        consumer: PcmConsumer,
        format: PcmFormat,
        max_block_frames: usize,
        controls: Arc<OutputControls>,
        metrics: Arc<OutputMetrics>,
    ) -> Result<Self, DspError> {
        if format.channels == 0 || format.sample_rate == 0 || max_block_frames == 0 {
            return Err(DspError::InvalidInput);
        }
        let channels = usize::from(format.channels);
        // Room for the largest target: base buffer + manual delay + alignment + 1 s headroom.
        let ring_frames = ms_to_frames(
            MAX_OUTPUT_DELAY_MS + MAX_ALIGN_MS + 1_000 + 200,
            format.sample_rate,
        ) as usize;
        let ring_samples = ring_frames
            .checked_mul(channels)
            .ok_or(DspError::InvalidInput)?;
        Ok(Self {
            consumer,
            channels,
            sample_rate: format.sample_rate,
            block: vec![0.0; max_block_frames * channels],
            ring: vec![0.0; ring_samples],
            ring_frames,
            read: 0,
            len: 0,
            phase: 0.0,
            expected_next: None,
            primed: false,
            silence_pending: 0,
            applied_extra: 0,
            smoothed_error: 0.0,
            correction_ppm: 0.0,
            resync_callbacks: 0,
            resync_error_sum: 0.0,
            gain: controls.gain(),
            controls,
            metrics,
            spatial: None,
            lfe: LowPass::default(),
            lfe_coefficient: LowPass::coefficient(format.sample_rate),
        })
    }

    /// Joins the shared spatial field (room map, 7.1 upmix, 8D rotation).
    pub fn with_spatial(mut self, spatial: Arc<SpatialShared>) -> Self {
        self.spatial = Some(spatial);
        self
    }

    pub fn buffered_frames(&self) -> usize {
        self.len
    }
    pub fn storage_capacity_samples(&self) -> usize {
        self.ring.capacity() + self.block.capacity()
    }
    pub fn correction_ppm(&self) -> f64 {
        self.correction_ppm
    }

    fn drop_front(&mut self, frames: usize) {
        let frames = frames.min(self.len);
        self.read = (self.read + frames) % self.ring_frames;
        self.len -= frames;
    }

    fn make_room(&mut self, frames: usize) {
        let overflow = (self.len + frames).saturating_sub(self.ring_frames);
        if overflow > 0 {
            self.metrics.record_overrun();
            self.drop_front(overflow);
        }
    }

    /// Appends `block[offset..offset + frames]` to the ring.
    fn write_block(&mut self, offset: usize, frames: usize) {
        self.make_room(frames);
        let ch = self.channels;
        let write = (self.read + self.len) % self.ring_frames;
        let first_part = frames.min(self.ring_frames - write);
        self.ring[write * ch..(write + first_part) * ch]
            .copy_from_slice(&self.block[offset * ch..(offset + first_part) * ch]);
        if first_part < frames {
            let rest = frames - first_part;
            self.ring[..rest * ch]
                .copy_from_slice(&self.block[(offset + first_part) * ch..(offset + frames) * ch]);
        }
        self.len += frames;
    }

    fn restart_timeline(&mut self) {
        self.len = 0;
        self.phase = 0.0;
        self.primed = false;
        self.smoothed_error = 0.0;
    }

    /// A capture timestamp discontinuity (paused source, flushed or lost packets)
    /// schedules a one-shot resync to the target buffer instead of slewing for
    /// minutes at the drift-correction clamp. No silence is synthesised here: if the
    /// source paused long enough to drain the buffer, the underrun already played it.
    /// Jumps beyond ~0.5 s restart the timeline and re-prime.
    fn ingest(&mut self) {
        let max_gap = u64::from(self.sample_rate / 2);
        loop {
            let (first, frames) = match self.consumer.pop_into(&mut self.block) {
                Ok(block) => block,
                Err(_) => break,
            };
            if frames == 0 || frames > self.ring_frames {
                continue;
            }
            if let Some(next) = self.expected_next {
                if first != next {
                    self.metrics.record_discontinuity();
                    if first.abs_diff(next) > max_gap {
                        self.restart_timeline();
                    }
                    self.resync_callbacks = RESYNC_WINDOW;
                    self.resync_error_sum = 0.0;
                }
            }
            self.expected_next = first.checked_add(frames as u64);
            self.write_block(0, frames);
        }
    }

    fn apply_delay_change(&mut self) {
        let extra = self
            .controls
            .delay_frames()
            .saturating_add(self.controls.align_frames())
            .saturating_add(self.controls.placement_delay_frames());
        if extra == self.applied_extra {
            return;
        }
        if extra > self.applied_extra {
            // Hold the read position: incoming audio accumulates by exactly this amount.
            self.silence_pending += (extra - self.applied_extra) as usize;
        } else {
            let mut reduce = (self.applied_extra - extra) as usize;
            let from_silence = reduce.min(self.silence_pending);
            self.silence_pending -= from_silence;
            reduce -= from_silence;
            self.drop_front(reduce);
        }
        self.applied_extra = extra;
        self.smoothed_error = 0.0;
    }

    fn target_frames(&self) -> usize {
        (self.controls.target_frames() as usize + self.applied_extra as usize)
            .min(self.ring_frames - 1)
    }

    /// Fills `output` (interleaved, source channel layout) with exactly
    /// `output.len() / channels` frames. Underruns render silence and re-prime.
    pub fn render(&mut self, output: &mut [f32]) {
        let ch = self.channels;
        let frames_out = output.len() / ch;
        self.ingest();
        self.apply_delay_change();
        let target = self.target_frames();

        if !self.primed {
            // Pending silence counts toward the target: it represents audio yet to be played.
            if self.len + self.silence_pending >= target.max(2) {
                self.primed = true;
                self.smoothed_error = 0.0;
                self.phase = 0.0;
            } else {
                output.fill(0.0);
                self.publish_metrics(frames_out, false);
                return;
            }
        }

        // Far beyond target (e.g. after a stall): resynchronise once instead of
        // slewing for minutes. Routine drift never reaches this threshold.
        let effective = self.len + self.silence_pending;
        let hard_limit = target + (self.sample_rate as usize / 4).max(target);
        if effective > hard_limit {
            self.drop_front(effective - target);
            self.metrics.record_discontinuity();
            self.smoothed_error = 0.0;
        }

        let mut error = (self.len + self.silence_pending) as f64 - target as f64;
        if self.resync_callbacks > 0 {
            self.resync_error_sum += error;
            self.resync_callbacks -= 1;
            if self.resync_callbacks == 0 {
                let mean = self.resync_error_sum / f64::from(RESYNC_WINDOW);
                // Ignore sub-millisecond offsets; the drift controller handles those.
                if mean.abs() > f64::from(self.sample_rate) / 1_000.0 {
                    let frames = mean.abs().round() as usize;
                    if mean > 0.0 {
                        self.drop_front(frames);
                    } else {
                        self.silence_pending += frames;
                    }
                    error -= mean;
                    self.smoothed_error = 0.0;
                    self.correction_ppm = 0.0;
                }
            }
        }
        self.smoothed_error += (error - self.smoothed_error) * 0.05;
        // Remove a buffer error over roughly five seconds, clamped and slew-limited.
        let max_ppm = self.controls.max_correction_ppm();
        let desired = (self.smoothed_error / f64::from(self.sample_rate) * 1_000_000.0 / 5.0)
            .clamp(-max_ppm, max_ppm);
        self.correction_ppm += (desired - self.correction_ppm).clamp(-5.0, 5.0);
        let ratio = 1.0 + self.correction_ppm * 0.000_001;

        let target_gain = if self.controls.muted() {
            0.0
        } else {
            self.controls.gain() * self.controls.trim()
        };
        let role = self.controls.role();
        // Spatial gains at the start and end of this callback, keyed to the
        // source frame position so every output sees the same rotation.
        let spatial = match (&self.spatial, self.controls.spatial_slot()) {
            (Some(shared), Some(slot)) if shared.enabled() && self.channels == 2 => {
                let read_frame = self
                    .expected_next
                    .unwrap_or(0)
                    .saturating_sub(self.len as u64);
                let rate = self.sample_rate;
                let start = GainMatrix::compute(shared, slot, shared.rotation_at(read_frame, rate));
                let end = GainMatrix::compute(
                    shared,
                    slot,
                    shared.rotation_at(read_frame + frames_out as u64, rate),
                );
                Some((shared.layout(), start, end, self.controls.subwoofer()))
            }
            (Some(shared), None) if shared.enabled() && self.controls.subwoofer() => Some((
                shared.layout(),
                GainMatrix::default(),
                GainMatrix::default(),
                true,
            )),
            _ => None,
        };
        let mut virtual_frame = [0.0f32; MAX_VIRTUAL];
        let gain_step = if frames_out > 0 {
            (target_gain - self.gain) / frames_out as f32
        } else {
            0.0
        };

        let mut frame = 0;
        while frame < frames_out {
            self.gain += gain_step;
            let out = &mut output[frame * ch..(frame + 1) * ch];
            if self.silence_pending > 0 {
                out.fill(0.0);
                self.silence_pending -= 1;
                frame += 1;
                continue;
            }
            if self.len < 2 {
                output[frame * ch..].fill(0.0);
                self.metrics.record_underrun();
                self.primed = false;
                break;
            }
            let a = self.read;
            let b = (self.read + 1) % self.ring_frames;
            let fraction = self.phase as f32;
            for c in 0..ch {
                let left = self.ring[a * ch + c];
                let right = self.ring[b * ch + c];
                out[c] = (left + (right - left) * fraction) * self.gain;
            }
            match &spatial {
                Some((_, _, _, true)) => {
                    let mono = self
                        .lfe
                        .process(0.5 * (out[0] + out[1]), self.lfe_coefficient);
                    out[0] = mono;
                    out[1] = mono;
                }
                Some((layout, start, end, false)) => {
                    let t = frame as f32 / frames_out.max(1) as f32;
                    let n = virtual_channels(*layout, out[0], out[1], &mut virtual_frame);
                    let (mut l, mut r) = (0.0, 0.0);
                    for v in 0..n {
                        let gl = start.left[v] + (end.left[v] - start.left[v]) * t;
                        let gr = start.right[v] + (end.right[v] - start.right[v]) * t;
                        l += gl * virtual_frame[v];
                        r += gr * virtual_frame[v];
                    }
                    out[0] = l;
                    out[1] = r;
                }
                None => role.apply(out),
            }
            self.phase += ratio;
            while self.phase >= 1.0 && self.len > 0 {
                self.phase -= 1.0;
                self.drop_front(1);
            }
            frame += 1;
        }
        self.gain = target_gain;
        self.publish_metrics(frames_out, self.primed);
    }

    fn publish_metrics(&self, frames: usize, primed: bool) {
        let m = &self.metrics;
        m.buffered_frames
            .store((self.len + self.silence_pending) as u64, Ordering::Relaxed);
        m.rendered_frames
            .fetch_add(frames as u64, Ordering::Relaxed);
        m.correction_ppm_milli
            .store((self.correction_ppm * 1_000.0) as i64, Ordering::Relaxed);
        m.primed.store(primed, Ordering::Relaxed);
    }
}

/// Sample container negotiated with a WASAPI render client.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum DeviceSampleFormat {
    F32,
    I16,
    /// Packed 24-bit little-endian in 3 bytes.
    I24,
    I32,
}

impl DeviceSampleFormat {
    pub fn bytes_per_sample(self) -> usize {
        match self {
            DeviceSampleFormat::F32 | DeviceSampleFormat::I32 => 4,
            DeviceSampleFormat::I16 => 2,
            DeviceSampleFormat::I24 => 3,
        }
    }
}

/// Clamps and converts float PCM into a device buffer. Returns bytes written.
pub fn encode_samples(
    input: &[f32],
    format: DeviceSampleFormat,
    output: &mut [u8],
) -> Result<usize, DspError> {
    let width = format.bytes_per_sample();
    let bytes = input
        .len()
        .checked_mul(width)
        .ok_or(DspError::InvalidInput)?;
    if output.len() < bytes {
        return Err(DspError::OutputTooSmall);
    }
    for (sample, out) in input.iter().zip(output.chunks_exact_mut(width)) {
        let value = if sample.is_finite() {
            sample.clamp(-1.0, 1.0)
        } else {
            0.0
        };
        match format {
            DeviceSampleFormat::F32 => out.copy_from_slice(&value.to_le_bytes()),
            DeviceSampleFormat::I16 => {
                out.copy_from_slice(&((value * 32_767.0).round() as i16).to_le_bytes())
            }
            DeviceSampleFormat::I24 => {
                let v = (value * 8_388_607.0).round() as i32;
                out.copy_from_slice(&v.to_le_bytes()[..3]);
            }
            DeviceSampleFormat::I32 => {
                let v = (f64::from(value) * 2_147_483_647.0).round() as i32;
                out.copy_from_slice(&v.to_le_bytes());
            }
        }
    }
    Ok(bytes)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{BoundedPcmQueue, PcmProducer, QueueStatus};

    const FORMAT: PcmFormat = PcmFormat {
        sample_rate: 48_000,
        channels: 2,
    };

    fn output(
        gain: f32,
        delay_ms: u32,
    ) -> (
        PcmProducer,
        RenderPipeline,
        Arc<OutputControls>,
        Arc<OutputMetrics>,
    ) {
        let (producer, consumer) = BoundedPcmQueue::new(FORMAT, 64, 480).unwrap().into_spsc();
        let controls = Arc::new(OutputControls::new(
            gain,
            false,
            ms_to_frames(delay_ms, 48_000),
            AudioMode::Balanced.policy(),
            48_000,
        ));
        let metrics = Arc::new(OutputMetrics::default());
        let pipeline =
            RenderPipeline::new(consumer, FORMAT, 480, controls.clone(), metrics.clone()).unwrap();
        (producer, pipeline, controls, metrics)
    }

    fn push_constant(producer: &mut PcmProducer, first: u64, frames: usize, value: f32) {
        let samples = vec![value; frames * 2];
        assert!(matches!(
            producer.push_samples(first, &samples),
            QueueStatus::Ok | QueueStatus::Discontinuity
        ));
    }

    #[test]
    fn primes_to_target_then_plays_with_gain() {
        let (mut producer, mut pipeline, _, metrics) = output(0.5, 0);
        let target = ms_to_frames(40, 48_000) as usize; // Balanced
        let mut out = vec![1.0; 480 * 2];
        pipeline.render(&mut out);
        assert!(out.iter().all(|s| *s == 0.0), "silence while priming");
        for i in 0..(target / 480 + 2) {
            push_constant(&mut producer, (i * 480) as u64, 480, 1.0);
        }
        pipeline.render(&mut out); // ramps from 0.5 (initial gain) — constant gain
        assert!(
            out.iter().all(|s| (*s - 0.5).abs() < 1e-6),
            "{:?}",
            &out[..4]
        );
        assert!(metrics.snapshot().primed);
    }

    #[test]
    fn mute_ramps_to_silence_and_gain_changes_are_click_free() {
        let (mut producer, mut pipeline, controls, _) = output(1.0, 0);
        for i in 0..10 {
            push_constant(&mut producer, i * 480, 480, 1.0);
        }
        let mut out = vec![0.0; 240 * 2];
        pipeline.render(&mut out);
        controls.set_muted(true);
        pipeline.render(&mut out);
        // Linear ramp from 1 to 0 over the callback: monotonic, ends at 0.
        let left: Vec<f32> = out.chunks(2).map(|f| f[0]).collect();
        assert!(left.windows(2).all(|w| w[1] <= w[0] + 1e-6));
        assert!(left.last().unwrap().abs() < 1e-6);
        pipeline.render(&mut out);
        assert!(out.iter().all(|s| *s == 0.0));
    }

    #[test]
    fn two_outputs_have_independent_queues_gain_and_delay() {
        let (mut pa, mut a, _, _) = output(1.0, 0);
        let (mut pb, mut b, _, _) = output(0.25, 100);
        for i in 0..20u64 {
            let samples: Vec<f32> = (0..480 * 2)
                .map(|n| ((i * 480) as f32 + (n / 2) as f32) / 100_000.0)
                .collect();
            pa.push_samples(i * 480, &samples);
            pb.push_samples(i * 480, &samples);
        }
        let mut out_a = vec![0.0; 480 * 2];
        let mut out_b = vec![0.0; 480 * 2];
        a.render(&mut out_a);
        b.render(&mut out_b);
        assert!(out_a[0] == 0.0 && out_a[2] > 0.0, "A plays from the start");
        // B adds 100 ms of delay: first callback is held silence.
        assert!(out_b.iter().all(|s| *s == 0.0));
        assert!(b.buffered_frames() > a.buffered_frames());
    }

    #[test]
    fn delay_change_shifts_output_by_exact_frames() {
        let (mut producer, mut pipeline, controls, _) = output(1.0, 0);
        let mut next = 0u64;
        for _ in 0..8 {
            push_constant(&mut producer, next, 480, 1.0);
            next += 480;
        }
        let mut out = vec![0.0; 480 * 2];
        pipeline.render(&mut out);
        let before = pipeline.buffered_frames();
        controls.set_delay_frames(240);
        let mut out = vec![0.0; 240 * 2];
        pipeline.render(&mut out);
        assert!(out.iter().all(|s| *s == 0.0), "inserted delay is silence");
        assert_eq!(pipeline.buffered_frames(), before);
        controls.set_delay_frames(0);
        pipeline.render(&mut out);
        assert_eq!(pipeline.buffered_frames(), before - 480);
    }

    #[test]
    fn capture_discontinuity_resyncs_buffer_to_target() {
        for lost_blocks in [2u64, 0] {
            // lost_blocks = 0 with a forward timestamp jump models a re-anchored
            // timeline (no audio lost); 2 models dropped packets.
            let (mut producer, mut pipeline, controls, metrics) = output(1.0, 0);
            let mut out = vec![0.0; 480 * 2];
            let mut stamp = 0u64;
            for _ in 0..4 {
                push_constant(&mut producer, stamp, 480, 1.0);
                stamp += 480;
            }
            // The setpoint is the level *before* a callback, so right after a
            // 480-frame callback a resynced buffer sits at target − 480.
            let expected = controls.target_frames() as i64 - 480;
            for n in 0..200 {
                if n == 100 {
                    stamp += 3 * 480; // timestamp jump
                    if lost_blocks > 0 {
                        pipeline.render(&mut out);
                        pipeline.render(&mut out); // consumed while packets were missing
                    }
                }
                push_constant(&mut producer, stamp, 480, 1.0);
                stamp += 480;
                pipeline.render(&mut out);
            }
            assert!(metrics.snapshot().discontinuities >= 1);
            let after = pipeline.buffered_frames() as i64;
            assert!(
                (after - expected).abs() <= 60,
                "lost {lost_blocks}: expected {expected}, after {after}"
            );
        }
    }

    #[test]
    fn long_capture_gap_restarts_and_reprimes() {
        let (mut producer, mut pipeline, _, metrics) = output(1.0, 0);
        for i in 0..6 {
            push_constant(&mut producer, i * 480, 480, 1.0);
        }
        let mut out = vec![0.0; 480 * 2];
        pipeline.render(&mut out);
        push_constant(&mut producer, 48_000 * 10, 480, 1.0);
        pipeline.render(&mut out);
        assert!(!metrics.snapshot().primed);
        assert_eq!(pipeline.buffered_frames(), 480);
    }

    #[test]
    fn underrun_renders_silence_counts_and_reprimes() {
        let (mut producer, mut pipeline, _, metrics) = output(1.0, 0);
        for i in 0..5 {
            push_constant(&mut producer, i * 480, 480, 1.0);
        }
        let mut out = vec![0.0; 480 * 2];
        for _ in 0..6 {
            pipeline.render(&mut out);
        }
        let snapshot = metrics.snapshot();
        assert!(snapshot.underruns >= 1);
        assert!(!snapshot.primed);
        assert_eq!(*out.last().unwrap(), 0.0);
        assert!(pipeline.storage_capacity_samples() > 0);
    }

    #[test]
    fn storage_is_fixed_after_construction() {
        let (mut producer, mut pipeline, controls, _) = output(1.0, 0);
        let initial = pipeline.storage_capacity_samples();
        let mut out = vec![0.0; 441 * 2];
        for i in 0..2_000u64 {
            push_constant(&mut producer, i * 480, 480, 0.1);
            if i % 300 == 0 {
                controls.set_delay_frames(((i / 300) * 4_800) as u32);
            }
            pipeline.render(&mut out);
            assert_eq!(pipeline.storage_capacity_samples(), initial);
        }
    }

    /// Simulated independent clocks: the source runs fast or slow by ±300 ppm
    /// with ±1-packet jitter. Correction must converge in sign and keep the
    /// buffer bounded near target without underruns.
    #[test]
    fn drift_correction_tracks_independent_clock_with_jitter() {
        for source_ppm in [300.0_f64, -300.0] {
            let (mut producer, mut pipeline, controls, metrics) = output(1.0, 0);
            let target = controls.target_frames() as f64;
            let mut produced = 0u64;
            let mut out = vec![0.0; 480 * 2];
            let callbacks = 12_000; // 120 s of 10 ms callbacks
            for n in 1..=callbacks {
                let ideal = (n as f64 * 480.0 * (1.0 + source_ppm * 1e-6)) as u64;
                // Jitter: deliver late on odd callbacks, catch up on even ones.
                let deliver_to = if n % 2 == 1 {
                    ideal.saturating_sub(200)
                } else {
                    ideal
                };
                while produced + 1 <= deliver_to {
                    let frames = (deliver_to - produced).min(480) as usize;
                    push_constant(&mut producer, produced, frames, 0.2);
                    produced += frames as u64;
                }
                pipeline.render(&mut out);
            }
            let snapshot = metrics.snapshot();
            assert!(
                (pipeline.correction_ppm() - source_ppm).abs() < 60.0,
                "source {source_ppm}: correction {}",
                pipeline.correction_ppm()
            );
            let error = pipeline.buffered_frames() as f64 - target;
            assert!(
                error.abs() < 600.0,
                "source {source_ppm}: buffer error {error}"
            );
            assert!(snapshot.underruns <= 1, "underruns {}", snapshot.underruns);
        }
    }

    #[test]
    fn correction_is_clamped_to_mode_limit() {
        let (mut producer, mut pipeline, controls, _) = output(1.0, 0);
        controls.apply_policy(AudioMode::Synchronized.policy(), 48_000);
        let mut produced = 0u64;
        let mut out = vec![0.0; 480 * 2];
        for n in 1..=3_000u64 {
            let ideal = (n as f64 * 480.0 * 1.000_9) as u64; // 900 ppm fast
            while produced < ideal {
                let frames = (ideal - produced).min(480) as usize;
                push_constant(&mut producer, produced, frames, 0.2);
                produced += frames as u64;
            }
            pipeline.render(&mut out);
            assert!(pipeline.correction_ppm() <= 200.0 + 1e-9);
        }
    }

    #[test]
    fn channel_roles_map_stereo_frames() {
        let cases = [
            (ChannelRole::Stereo, [1.0, 0.0]),
            (ChannelRole::Swapped, [0.0, 1.0]),
            (ChannelRole::LeftOnly, [1.0, 1.0]),
            (ChannelRole::RightOnly, [0.0, 0.0]),
            (ChannelRole::Mono, [0.5, 0.5]),
        ];
        for (role, expected) in cases {
            let mut frame = [1.0, 0.0];
            role.apply(&mut frame);
            assert_eq!(frame, expected, "{role:?}");
            assert_eq!(ChannelRole::from_u8(role.to_u8()), role);
        }
        let mut mono = [0.3];
        ChannelRole::LeftOnly.apply(&mut mono);
        assert_eq!(mono, [0.3]);
    }

    #[test]
    fn role_trim_and_placement_delay_apply_in_the_render_path() {
        let (mut producer, mut pipeline, controls, _) = output(1.0, 0);
        for i in 0..12u64 {
            let samples: Vec<f32> = (0..480).flat_map(|_| [0.8, 0.2]).collect();
            producer.push_samples(i * 480, &samples);
        }
        controls.set_role(ChannelRole::RightOnly);
        controls.set_trim(0.5);
        let mut out = vec![0.0; 480 * 2];
        pipeline.render(&mut out);
        pipeline.render(&mut out); // gain ramp settled
        assert!(out
            .chunks(2)
            .all(|f| (f[0] - 0.1).abs() < 1e-6 && (f[1] - 0.1).abs() < 1e-6));
        let before = pipeline.buffered_frames();
        controls.set_placement_delay_frames(240);
        let mut short = vec![0.0; 240 * 2];
        pipeline.render(&mut short);
        assert!(
            short.iter().all(|s| *s == 0.0),
            "placement delay is held silence"
        );
        assert_eq!(pipeline.buffered_frames(), before);
    }

    #[test]
    fn spatial_mode_pans_virtual_left_into_the_left_ear() {
        use crate::spatial::{SourceLayout, SpatialConfig};
        let shared = Arc::new(SpatialShared::default());
        shared.set(
            SpatialConfig {
                enabled: true,
                layout: SourceLayout::Stereo,
                rotation_s: None,
                clockwise: true,
            },
            &[-90.0, 90.0],
        );
        let (mut producer, pipeline, controls, _) = output(1.0, 0);
        let mut pipeline = pipeline.with_spatial(shared.clone());
        controls.set_spatial_slot(Some(0));
        for i in 0..12u64 {
            let samples: Vec<f32> = (0..480).flat_map(|_| [1.0, 0.0]).collect();
            producer.push_samples(i * 480, &samples);
        }
        let mut out = vec![0.0; 480 * 2];
        pipeline.render(&mut out);
        // Virtual L (−30°) sits between the ears (−90°, +90°), nearer the left.
        assert!(out[0] > out[1] && out[1] > 0.0, "{:?}", &out[..2]);
        // Subwoofer output: low-passed mono.
        controls.set_spatial_slot(None);
        controls.set_subwoofer(true);
        pipeline.render(&mut out);
        let last = &out[out.len() - 2..];
        assert!((last[0] - last[1]).abs() < 1e-6 && last[0] > 0.0);
    }

    #[test]
    fn encodes_float_and_integer_device_formats() {
        let input = [1.0, -1.0, 0.5, f32::NAN, 2.0];
        let mut bytes = [0u8; 20];
        assert_eq!(
            encode_samples(&input, DeviceSampleFormat::I16, &mut bytes),
            Ok(10)
        );
        assert_eq!(i16::from_le_bytes([bytes[0], bytes[1]]), 32_767);
        assert_eq!(i16::from_le_bytes([bytes[2], bytes[3]]), -32_767);
        assert_eq!(i16::from_le_bytes([bytes[4], bytes[5]]), 16_384);
        assert_eq!(i16::from_le_bytes([bytes[6], bytes[7]]), 0, "NaN → 0");
        assert_eq!(i16::from_le_bytes([bytes[8], bytes[9]]), 32_767, "clamped");
        assert_eq!(
            encode_samples(&input, DeviceSampleFormat::I24, &mut bytes),
            Ok(15)
        );
        assert_eq!(&bytes[..3], &[0xFF, 0xFF, 0x7F]);
        assert_eq!(
            encode_samples(&input, DeviceSampleFormat::I32, &mut bytes),
            Ok(20)
        );
        assert_eq!(i32::from_le_bytes(bytes[..4].try_into().unwrap()), i32::MAX);
        assert_eq!(
            encode_samples(&input, DeviceSampleFormat::F32, &mut bytes),
            Ok(20)
        );
        assert_eq!(f32::from_le_bytes(bytes[8..12].try_into().unwrap()), 0.5);
        assert_eq!(
            encode_samples(&input, DeviceSampleFormat::F32, &mut bytes[..8]),
            Err(DspError::OutputTooSmall)
        );
    }

    #[test]
    fn mode_policies_are_ordered_by_buffer_budget() {
        let gaming = AudioMode::Gaming.policy();
        let balanced = AudioMode::Balanced.policy();
        let synced = AudioMode::Synchronized.policy();
        assert!(gaming.target_buffer_ms < balanced.target_buffer_ms);
        assert!(balanced.target_buffer_ms < synced.target_buffer_ms);
        assert!(synced.align_outputs && !balanced.align_outputs);
        assert!(gaming.latency_warning_ms.is_some());
    }
}

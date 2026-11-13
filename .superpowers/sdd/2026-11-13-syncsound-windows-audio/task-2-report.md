# Task 2 report — platform-independent PCM and timing core

## Status

Implemented the Task 2 core on top of Task 1 commit `e1920b2`. The new modules contain no Windows API types. The existing diagnostic-tone engine remains a prototype; this task does not implement system-audio capture or synchronized playback.

## RED → GREEN evidence

| Step | RED observed | GREEN observed |
| --- | --- | --- |
| PCM transport | `cargo test -p syncsound-engine ring_buffer -- --nocapture` failed with unresolved `PcmBlock`, `PcmFormat`, `BoundedPcmQueue`, and `QueueStatus` imports. | The focused queue suite passed after fixed-capacity storage and queue status handling were added. |
| Timing and DSP | Focused `timeline` and `dsp` test commands failed with unresolved `MasterTimeline`, `ClockEstimator`, `resample_interleaved`, `map_channels`, and `DelayLine` imports. | Both focused suites passed after implementation. |
| Negative QPC offsets | The full engine suite failed `qpc_maps_to_integer_frames_and_back`: actual 236, expected 235. | Floor division fixed mapping before the anchor; focused timing tests passed. |
| Borrowed capture input | Focused queue test failed because `push_samples` did not exist. | Focused queue suite passed with borrowed-slice input and no `PcmBlock` construction on the callback path. |
| Gradual drift correction | `correction_slews_when_drift_changes_direction` failed because the ratio changed immediately from +200 to -200 ppm. | Focused timing suite passed after adding a 50 ppm limit per accepted observation. |

## Files changed

- `crates/syncsound-engine/src/audio_format.rs`: serializable PCM format and timestamped block; interleaved frame validation.
- `crates/syncsound-engine/src/ring_buffer.rs`: preallocated block slots and sample storage, FIFO transport, explicit statuses, discontinuity reset, borrowed-slice push, and fanout tests.
- `crates/syncsound-engine/src/timeline.rs`: integer QPC/frame mapping, discontinuity reanchor, bounded clock estimate, outlier rejection, and correction slew.
- `crates/syncsound-engine/src/dsp.rs`: explicit mono/stereo mapping, interleaved linear interpolation, and fixed-storage delay line.
- `crates/syncsound-engine/src/metrics.rs`: callback-local saturating counters and serializable snapshot fields.
- `crates/syncsound-engine/src/lib.rs`: public module and interface exports.

## Verification

- `cargo test -p syncsound-engine`: 17 passed, 0 failed.
- `cargo test --workspace`: 17 passed, 0 failed; desktop and doc-test targets also passed.
- `cargo check --workspace`: passed on Windows.
- `npm run build`: passed (TypeScript and Vite).
- `git diff --check`: passed.

## Self-review

Queue allocation occurs only in `new`; `push`, `push_samples`, `pop_into`, and `reset` use existing storage and contain no locks, logging, disk I/O, or blocking operations. `DelayLine` likewise allocates only in `new`. Queue capacity, input block size, output size, format, and timestamp continuity are checked explicitly. Each output can own a separate queue for independent fanout. The clock correction ratio is limited to the configured ppm range and moves by at most 50 ppm on each accepted observation. The resampler uses caller-owned buffers and deterministic per-channel interpolation; callers must provide a contiguous input window with the extra frame required at its edge and maintain phase between windows.

## Concerns and follow-up

- `BoundedPcmQueue` is a single-owner primitive. Task 3 must provide a lock-free cross-thread handoff or another callback-safe ownership model before separate capture and render callbacks can use it concurrently.
- The 50 ppm slew limit is per accepted observation, so its wall-clock response depends on the caller's observation cadence. Real endpoint timing and jitter still need integration tests before claiming synchronization accuracy.
- No physical audio endpoint or end-to-end playback test was performed in this core-only task.

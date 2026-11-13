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

## Fix round — concurrent handoff and clock cadence

The review found three gaps in commit `1fa8767`. This section appends the fix evidence; the original report above remains as written.

### Changes

- `BoundedPcmQueue::into_spsc` now moves its preallocated PCM slots into a bounded, lock-free two-queue pool. It returns a unique `PcmProducer` and `PcmConsumer` that can move to separate threads. `push_samples` and `pop_into` copy between caller-owned slices and slots; they do not allocate, lock, log, or block. Full and empty states return explicit statuses. A generation counter invalidates stale blocks when the producer resets or detects a timestamp discontinuity. The original direct queue API remains available.
- `ClockEstimator` now observes a minimum 48,000-master-frame window, accepting monotonic callback observations between estimates. This prevents 480-frame integer rounding from appearing as 0 or over 2,000 ppm. Both the first and later accepted estimates change the applied correction by at most 50 ppm per estimate.
- `crossbeam-queue` was added as a platform-independent lock-free dependency. `Cargo.lock`, the engine manifest, exports, queue implementation, timeline implementation, and their tests changed.

### Focused RED evidence (exact commands and diagnostic output)

`cargo test -p syncsound-engine ring_buffer -- --nocapture` exited 1:

```text
error[E0599]: no method named `into_spsc` found for struct `BoundedPcmQueue` in the current scope
   --> crates\syncsound-engine\src\ring_buffer.rs:240:50
error[E0599]: no method named `into_spsc` found for struct `BoundedPcmQueue` in the current scope
   --> crates\syncsound-engine\src\ring_buffer.rs:277:90
error: could not compile `syncsound-engine` (lib test) due to 2 previous errors
```

`cargo test -p syncsound-engine callback_cadence -- --nocapture` exited 1:

```text
running 1 test
thread 'timeline::tests::callback_cadence_estimates_hundred_ppm_from_integer_frames' (42524) panicked at crates\syncsound-engine\src\timeline.rs:193:17:
callback 21
test timeline::tests::callback_cadence_estimates_hundred_ppm_from_integer_frames ... FAILED
test result: FAILED. 0 passed; 1 failed; 0 ignored; 0 measured; 20 filtered out; finished in 0.00s
```

`cargo test -p syncsound-engine first_estimate_slews -- --nocapture` exited 1:

```text
running 1 test
thread 'timeline::tests::first_estimate_slews_from_unity_before_reaching_clamp' (61744) panicked at crates\syncsound-engine\src\timeline.rs:205:9:
assertion failed: (estimator.correction_ratio() - 1.0 / 1.00005).abs() < 1e-9
test timeline::tests::first_estimate_slews_from_unity_before_reaching_clamp ... FAILED
test result: FAILED. 0 passed; 1 failed; 0 ignored; 0 measured; 20 filtered out; finished in 0.00s
```

### Focused GREEN evidence (exact commands and result output)

`cargo test -p syncsound-engine ring_buffer -- --nocapture` exited 0:

```text
running 7 tests
test ring_buffer::tests::callback_can_push_borrowed_samples_without_constructing_a_block ... ok
test ring_buffer::tests::fanout_copies_without_shared_consumer_state ... ok
test ring_buffer::tests::split_queue_discards_stale_audio_on_discontinuity ... ok
test ring_buffer::tests::storage_never_exceeds_configured_bounds ... ok
test ring_buffer::tests::fifo_and_exact_capacity_with_full_rejection ... ok
test ring_buffer::tests::discontinuity_discards_stale_blocks_and_reset_clears_state ... ok
test ring_buffer::tests::split_queue_hands_off_pcm_across_threads_in_order ... ok
test result: ok. 7 passed; 0 failed; 0 ignored; 0 measured; 14 filtered out; finished in 0.01s
```

`cargo test -p syncsound-engine timeline -- --nocapture` exited 0:

```text
running 7 tests
test timeline::tests::callback_cadence_estimates_hundred_ppm_from_integer_frames ... ok
test timeline::tests::correction_slews_when_drift_changes_direction ... ok
test timeline::tests::rejects_outliers_and_clamps_correction ... ok
test timeline::tests::qpc_maps_to_integer_frames_and_back ... ok
test timeline::tests::first_estimate_slews_from_unity_before_reaching_clamp ... ok
test timeline::tests::estimates_positive_and_negative_hundred_ppm ... ok
test timeline::tests::reset_reanchors_after_discontinuity ... ok
test result: ok. 7 passed; 0 failed; 0 ignored; 0 measured; 14 filtered out; finished in 0.00s
```

### Full verification (exact commands and result output)

`cargo test -p syncsound-engine` exited 0:

```text
running 21 tests
test result: ok. 21 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.01s
Doc-tests syncsound_engine
running 0 tests
test result: ok. 0 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.00s
```

`cargo test --workspace` exited 0:

```text
Running unittests src\lib.rs (target\debug\deps\syncsound_desktop_lib-73a6932cfc30a91e.exe)
running 0 tests
test result: ok. 0 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.00s
Running unittests src\main.rs (target\debug\deps\syncsound_desktop-5b8b70ef259154ef.exe)
running 0 tests
test result: ok. 0 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.00s
Running unittests src\lib.rs (target\debug\deps\syncsound_engine-f53ba28d9051c9b2.exe)
running 21 tests
test audio_format::tests::frame_count_uses_interleaved_channels ... ok
test dsp::tests::interpolation_uses_both_channels_without_cross_talk ... ok
test dsp::tests::delay_compensates_and_reset_discards_history ... ok
test ring_buffer::tests::callback_can_push_borrowed_samples_without_constructing_a_block ... ok
test dsp::tests::maps_mono_and_stereo_explicitly ... ok
test audio_format::tests::incomplete_frame_is_rejected ... ok
test dsp::tests::resamples_44100_48000_and_96000_to_48000 ... ok
test timeline::tests::first_estimate_slews_from_unity_before_reaching_clamp ... ok
test ring_buffer::tests::fanout_copies_without_shared_consumer_state ... ok
test ring_buffer::tests::fifo_and_exact_capacity_with_full_rejection ... ok
test ring_buffer::tests::split_queue_discards_stale_audio_on_discontinuity ... ok
test ring_buffer::tests::storage_never_exceeds_configured_bounds ... ok
test metrics::tests::counts_queue_events_without_growing_storage ... ok
test timeline::tests::correction_slews_when_drift_changes_direction ... ok
test timeline::tests::estimates_positive_and_negative_hundred_ppm ... ok
test timeline::tests::qpc_maps_to_integer_frames_and_back ... ok
test ring_buffer::tests::discontinuity_discards_stale_blocks_and_reset_clears_state ... ok
test timeline::tests::rejects_outliers_and_clamps_correction ... ok
test timeline::tests::reset_reanchors_after_discontinuity ... ok
test timeline::tests::callback_cadence_estimates_hundred_ppm_from_integer_frames ... ok
test ring_buffer::tests::split_queue_hands_off_pcm_across_threads_in_order ... ok
test result: ok. 21 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.01s
Doc-tests syncsound_desktop_lib
running 0 tests
test result: ok. 0 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.00s
Doc-tests syncsound_engine
running 0 tests
test result: ok. 0 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.00s
```

`cargo check --workspace` exited 0:

```text
    Checking crossbeam-queue v0.3.14
    Checking syncsound-engine v0.1.0 (E:\KR Softwares\SyncSound\syncsound\crates\syncsound-engine)
    Checking syncsound-desktop v0.1.0 (E:\KR Softwares\SyncSound\syncsound\apps\desktop\src-tauri)
    Finished `dev` profile [unoptimized + debuginfo] target(s) in 2.53s
```

`npm run build` exited 0:

```text
> syncsound@0.1.0 build
> tsc && vite build
vite v6.4.6 building for production...
✓ 1578 modules transformed.
dist/index.html                   0.35 kB │ gzip:  0.26 kB
dist/assets/index-CuaKwGLb.css    2.52 kB │ gzip:  0.96 kB
dist/assets/index-wrCE1kq4.js   150.84 kB │ gzip: 48.63 kB
✓ built in 1.79s
```

### Self-review and remaining concerns

The split handles are unique and `Send`; producer and consumer use bounded `ArrayQueue` operations. PCM slot vectors are allocated in `BoundedPcmQueue::new` and moved, without a second PCM allocation, by `into_spsc`. A producer reset marks an atomic generation and drains ready old slots; a consumer checks generation before returning audio, including when it holds a block after `OutputTooSmall`. Queue memory remains `capacity_blocks × max_frames × channels` samples. The observation window is fixed at 48,000 master frames, which is about one second at 48 kHz; its wall-clock length varies with other rates. The correction slew is 50 ppm per accepted window. Device callback scheduling and acoustic sync still require integration validation; no physical endpoint test was run here.

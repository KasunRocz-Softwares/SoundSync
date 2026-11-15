// Unit tests for the device page's chunk scheduler. Run: npm run test:page
import { test } from "node:test";
import assert from "node:assert/strict";
import { createScheduler } from "./scheduler.js";

const RESYNC = 1;
const near = (actual, expected, eps = 1e-4) =>
  assert.ok(Math.abs(actual - expected) < eps, `expected ${expected}, got ${actual}`);

test("first chunk starts at target", () => {
  const s = createScheduler({ targetMs: 150 });
  const r = s.enqueue(1, 0, 10);
  near(r.startAt, 10.15);
  assert.equal(r.playbackRate, 1);
});

test("chunks play back to back", () => {
  const s = createScheduler({ targetMs: 150 });
  const a = s.enqueue(1, 0, 10);
  const b = s.enqueue(2, 0, 10.001);
  near(b.startAt, a.startAt + 0.02);
});

test("rate speeds up when buffered over target", () => {
  const s = createScheduler({ targetMs: 150 });
  s.enqueue(1, 0, 10);
  let r;
  let t = 10;
  // Chunks arrive every 6 ms (faster than they play, but not a burst), so
  // the schedule grows by about 14 ms each time until it is near 300 ms.
  for (let seq = 2; seq <= 11; seq++) {
    t += 0.006;
    r = s.enqueue(seq, 0, t);
  }
  assert.ok(s.bufferedMs(t) > 300, `buffered ${s.bufferedMs(t)}`);
  assert.ok(r.playbackRate > 1, `rate ${r.playbackRate}`);
  assert.ok(r.playbackRate <= 1.001, `rate ${r.playbackRate}`);
});

test("rate slows when under target", () => {
  const s = createScheduler({ targetMs: 150 });
  s.enqueue(1, 0, 10);
  const r = s.enqueue(2, 0, 10.1); // only 70 ms left in the schedule
  assert.ok(r.playbackRate >= 0.999, `rate ${r.playbackRate}`);
  assert.ok(r.playbackRate < 1, `rate ${r.playbackRate}`);
});

test("scheduler resyncs after a stall", () => {
  const s = createScheduler({ targetMs: 150 });
  s.enqueue(1, 0, 10);
  const r = s.enqueue(2, 0, 12);
  near(r.startAt, 12.15);
});

test("resync flag restarts at target", () => {
  const s = createScheduler({ targetMs: 150 });
  s.enqueue(1, 0, 10);
  s.enqueue(2, 0, 10.01);
  const r = s.enqueue(3, RESYNC, 10.02);
  near(r.startAt, 10.17);
  assert.equal(r.playbackRate, 1);
});

test("late chunk is dropped", () => {
  const s = createScheduler({ targetMs: 150 });
  s.enqueue(1, 0, 10); // plays 10.15 .. 10.17
  assert.equal(s.enqueue(2, 0, 10.2), null);
  // The next one starts fresh at the target again.
  near(s.enqueue(3, 0, 10.21).startAt, 10.36);
});

test("weak Wi-Fi target of 300 ms", () => {
  const s = createScheduler({ targetMs: 300 });
  near(s.enqueue(1, 0, 5).startAt, 5.3);
});

test("buffered and latency figures", () => {
  const s = createScheduler({ targetMs: 150 });
  assert.equal(s.bufferedMs(10), 0);
  s.enqueue(1, 0, 10);
  near(s.bufferedMs(10), 170, 0.01);
  near(s.latencyMs(10, 0.03), 200, 0.01);
  near(s.latencyMs(10, undefined), 170, 0.01);
  assert.equal(s.bufferedMs(11), 0);
});

test("burst far over target is trimmed", () => {
  const s = createScheduler({ targetMs: 150 });
  s.enqueue(1, 0, 10);
  let dropped = 0;
  // 40 chunks arrive at once (800 ms of audio after a Wi-Fi hiccup).
  for (let seq = 2; seq <= 40; seq++) if (s.enqueue(seq, 0, 10) === null) dropped++;
  assert.ok(dropped > 0, "some chunks are dropped");
  assert.ok(s.bufferedMs(10) <= 2 * 150 + 100 + 20, `buffered ${s.bufferedMs(10)}`);
  // Dropping does not restart the schedule: the next chunk still follows on
  // (a resync would start it at 10.1 + 0.15).
  const r = s.enqueue(41, 0, 10.1);
  assert.ok(r !== null && r.startAt < 10.1 + 0.15 - 0.01, `start ${r && r.startAt}`);
});

test("burst after a stall settles near the target", () => {
  const s = createScheduler({ targetMs: 150 });
  s.enqueue(1, 0, 10);
  // Wi-Fi stalls for a second, then the PC flushes 30 queued chunks at once.
  for (let seq = 2; seq <= 31; seq++) s.enqueue(seq, 0, 11);
  assert.ok(s.bufferedMs(11) >= 150, `buffered ${s.bufferedMs(11)}`);
  assert.ok(s.bufferedMs(11) <= 150 + 40 + 0.5, `buffered ${s.bufferedMs(11)}`);
  assert.ok(s.latencyMs(11, 0) <= 150 + 40 + 0.5, `latency ${s.latencyMs(11, 0)}`);
});

test("a 260 ms gap with a 300 ms target does not resync", () => {
  const s = createScheduler({ targetMs: 300 });
  near(s.enqueue(1, 0, 10).startAt, 10.3);
  near(s.enqueue(2, 0, 10.26).startAt, 10.32);
});

test("a close pair in steady playback is kept", () => {
  const s = createScheduler({ targetMs: 150 });
  s.enqueue(1, 0, 10);
  s.enqueue(2, 0, 10.015); // a normal gap ends the start-up burst
  // The audio clock is coarse: two chunks often read the same time.
  const r = s.enqueue(3, 0, 10.015);
  assert.ok(r !== null, "not dropped");
  near(r.startAt, 10.19);
});

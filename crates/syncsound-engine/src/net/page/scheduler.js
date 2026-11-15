// Plays 20 ms PCM chunks back to back on an AudioContext timeline.
// No DOM: the page and `node --test` both import this module.

const CHUNK_FRAMES = 960;
const RESYNC_FLAG = 1;
/** A pause longer than this between chunks (context time) restarts the schedule. */
const STALL_SEC = 0.25;
/** Drift correction: rate moves 0.2 % per 100 % buffer error, capped at 0.1 %. */
const RATE_GAIN = 0.002;
const RATE_LIMIT = 0.001;
/** Above twice the target plus this much, new chunks are dropped (burst trim). */
const OVERFLOW_SEC = 0.1;

function clamp(v, lo, hi) {
  return v < lo ? lo : v > hi ? hi : v;
}

/**
 * @param {{ targetMs?: number, rate?: number }} [opts]
 * targetMs: how far ahead of "now" playback starts after a (re)sync.
 * rate: stream sample rate; a chunk lasts CHUNK_FRAMES / rate seconds.
 */
export function createScheduler(opts) {
  const o = opts || {};
  const targetSec = (o.targetMs === undefined ? 150 : o.targetMs) / 1000;
  const chunkSec = CHUNK_FRAMES / (o.rate === undefined ? 48000 : o.rate);
  const maxSec = 2 * targetSec + OVERFLOW_SEC;
  let nextStart = null; // context time where the next chunk begins
  let lastNow = null; // context time of the previous enqueue

  function bufferedSec(now) {
    return nextStart === null ? 0 : Math.max(0, nextStart - now);
  }

  return {
    /**
     * Places one chunk. Returns when to start it and at what playbackRate,
     * or null when it is dropped: it would start in the past (the next one
     * resyncs), or the schedule is already far over target (burst trim).
     */
    enqueue(seq, flags, now) {
      const stalled = lastNow !== null && now - lastNow > STALL_SEC;
      lastNow = now;
      if (nextStart === null || (flags & RESYNC_FLAG) !== 0 || stalled) {
        nextStart = now + targetSec;
        const startAt = nextStart;
        nextStart += chunkSec;
        return { startAt, playbackRate: 1 };
      }
      if (nextStart < now) {
        nextStart = null;
        return null;
      }
      // A burst after a network stall would add delay that drift correction
      // takes minutes to remove; skipping chunks brings it back at once.
      if (bufferedSec(now) + chunkSec > maxSec) {
        return null;
      }
      const error = (bufferedSec(now) - targetSec) / targetSec;
      const playbackRate = 1 + clamp(error * RATE_GAIN, -RATE_LIMIT, RATE_LIMIT);
      const startAt = nextStart;
      nextStart += chunkSec / playbackRate;
      return { startAt, playbackRate };
    },

    /** Milliseconds of audio scheduled ahead of `now`. */
    bufferedMs(now) {
      return bufferedSec(now) * 1000;
    },

    /** Buffered time plus the output device latency (seconds, may be missing). */
    latencyMs(now, outputLatencySec) {
      const out = typeof outputLatencySec === "number" && outputLatencySec > 0 ? outputLatencySec : 0;
      return (bufferedSec(now) + out) * 1000;
    },
  };
}

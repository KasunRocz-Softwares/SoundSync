// Plays 20 ms PCM chunks back to back on an AudioContext timeline.
// No DOM: the page and `node --test` both import this module.

const CHUNK_FRAMES = 960;
const RESYNC_FLAG = 1;
/**
 * A pause between chunks (context time) longer than this, or than the
 * target, restarts the schedule. Shorter pauses only restart it when the
 * buffer actually ran dry.
 */
const STALL_SEC = 0.25;
/**
 * Enqueues closer together than this are "close". A burst (a flushed
 * backlog) is the run of close enqueues right after a resync, or the third
 * and later chunks of any close run. A close pair alone is normal: the
 * audio clock advances in steps, so two chunks often read the same time.
 */
const BURST_GAP_SEC = 0.005;
const BURST_RUN = 2;
/** During a burst, chunks past target + this are dropped. */
const BURST_SLACK_SEC = 0.04;
/** Drift correction: rate moves 0.2 % per 100 % buffer error, capped at 0.1 %. */
const RATE_GAIN = 0.002;
const RATE_LIMIT = 0.001;
/** Above twice the target plus this much, new chunks are always dropped. */
const OVERFLOW_SEC = 0.1;
/** Float slack for the limits above. */
const EPS = 1e-9;

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
  const stallSec = Math.max(STALL_SEC, targetSec);
  const burstMaxSec = targetSec + BURST_SLACK_SEC;
  let nextStart = null; // context time where the next chunk begins
  let lastNow = null; // context time of the previous enqueue
  let afterResync = false; // still inside the close run that follows a resync
  let closeRun = 0; // close enqueues in a row

  function bufferedSec(now) {
    return nextStart === null ? 0 : Math.max(0, nextStart - now);
  }

  return {
    /**
     * Places one chunk. Returns when to start it and at what playbackRate,
     * or null when it is dropped: it would start in the past (the next one
     * resyncs), or it is part of a burst past target + 40 ms, or the
     * schedule is already far over target.
     */
    enqueue(seq, flags, now) {
      const gap = lastNow === null ? 0 : now - lastNow;
      lastNow = now;
      const close = gap < BURST_GAP_SEC;
      closeRun = close ? closeRun + 1 : 0;
      if (!close) afterResync = false;
      if (nextStart === null || (flags & RESYNC_FLAG) !== 0 || gap > stallSec) {
        afterResync = true;
        closeRun = 0;
        nextStart = now + targetSec;
        const startAt = nextStart;
        nextStart += chunkSec;
        return { startAt, playbackRate: 1 };
      }
      // The buffer ran dry: this chunk is late. Drop it; the next one resyncs.
      if (nextStart <= now) {
        nextStart = null;
        return null;
      }
      const after = bufferedSec(now) + chunkSec;
      // A backlog flushed after a stall arrives as a burst. Keeping all of it
      // would leave the device late for minutes (drift correction moves only
      // 0.1 %), so a burst may only fill the schedule to target + 40 ms.
      const burst = close && (afterResync || closeRun >= BURST_RUN);
      if (burst && after > burstMaxSec + EPS) {
        return null;
      }
      if (after > maxSec + EPS) {
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

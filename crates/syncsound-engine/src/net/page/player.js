// SyncSound device page: joins the PC over a WebSocket and plays its audio
// on an AudioContext, chunk by chunk, in step with the PC's speakers.
// Plain HTTP on a LAN address is not a secure context, so this uses only
// APIs that work without one (no AudioWorklet, Wake Lock or randomUUID).

import { createScheduler } from "./scheduler.js";

const RATE = 48000;
const CHUNK_FRAMES = 960;
const HEADER_BYTES = 10;
const FRAME_BYTES = HEADER_BYTES + CHUNK_FRAMES * 2 * 2;
const KIND_AUDIO = 1;
const FLAG_RESYNC = 1;
const TARGET_MS = 150;
const WEAK_TARGET_MS = 300;
const BACKOFF_MS = [1000, 2000, 4000, 8000];
const CONNECT_TIMEOUT_MS = 10000;
const KEY_STORE = "syncsound.deviceKey";
const WEAK_STORE = "syncsound.weakWifi";

// Tiny black looping video for the no-sleep fallback (Wake Lock needs HTTPS).
const NOSLEEP_MP4 = "data:video/mp4;base64,AAAAIGZ0eXBpc29tAAACAGlzb21pc28yYXZjMW1wNDEAAAM0bW9vdgAAAGxtdmhkAAAAAAAAAAAAAAAAAAAD6AAAB9AAAQAAAQAAAAAAAAAAAAAAAAEAAAAAAAAAAAAAAAAAAAABAAAAAAAAAAAAAAAAAABAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAgAAAl90cmFrAAAAXHRraGQAAAADAAAAAAAAAAAAAAABAAAAAAAAB9AAAAAAAAAAAAAAAAAAAAAAAAEAAAAAAAAAAAAAAAAAAAABAAAAAAAAAAAAAAAAAABAAAAAAEAAAABAAAAAAAAkZWR0cwAAABxlbHN0AAAAAAAAAAEAAAfQAAAAAAABAAAAAAHXbWRpYQAAACBtZGhkAAAAAAAAAAAAAAAAAABAAAAAgABVxAAAAAAALWhkbHIAAAAAAAAAAHZpZGUAAAAAAAAAAAAAAABWaWRlb0hhbmRsZXIAAAABgm1pbmYAAAAUdm1oZAAAAAEAAAAAAAAAAAAAACRkaW5mAAAAHGRyZWYAAAAAAAAAAQAAAAx1cmwgAAAAAQAAAUJzdGJsAAAAunN0c2QAAAAAAAAAAQAAAKphdmMxAAAAAAAAAAEAAAAAAAAAAAAAAAAAAAAAAEAAQABIAAAASAAAAAAAAAABFExhdmM2My4xLjEwMiBsaWJ4MjY0AAAAAAAAAAAAAAAAGP//AAAAMGF2Y0MBQsAe/+EAF2dCwB7ZBCbARAAAAwAEAAADABA8WLkgAQAGaMuAZRMgAAAAEHBhc3AAAAABAAAAAQAAABRidHJ0AAAAAAAACsQAAAAAAAAAGHN0dHMAAAAAAAAAAQAAAAQAACAAAAAAFHN0c3MAAAAAAAAAAQAAAAEAAAAcc3RzYwAAAAAAAAABAAAAAQAAAAQAAAABAAAAJHN0c3oAAAAAAAAAAAAAAAQAAAKPAAAACwAAAAwAAAALAAAAFHN0Y28AAAAAAAAAAQAAA2QAAABhdWR0YQAAAFltZXRhAAAAAAAAACFoZGxyAAAAAAAAAABtZGlyYXBwbAAAAAAAAAAAAAAAACxpbHN0AAAAJKl0b28AAAAcZGF0YQAAAAEAAAAATGF2ZjYzLjEuMTAyAAAACGZyZWUAAAK5bWRhdAAAAm8GBf//a9xF6b3m2Ui3lizYINkj7u94MjY0IC0gY29yZSAxNjUgcjMyMjMgMDQ4MGNiMCAtIEguMjY0L01QRUctNCBBVkMgY29kZWMgLSBDb3B5bGVmdCAyMDAzLTIwMjUgLSBodHRwOi8vd3d3LnZpZGVvbGFuLm9yZy94MjY0Lmh0bWwgLSBvcHRpb25zOiBjYWJhYz0wIHJlZj0zIGRlYmxvY2s9MTotMzotMyBhbmFseXNlPTB4MToweDExMSBtZT1oZXggc3VibWU9NyBwc3k9MSBwc3lfcmQ9Mi4wMDowLjcwIG1peGVkX3JlZj0xIG1lX3JhbmdlPTE2IGNocm9tYV9tZT0xIHRyZWxsaXM9MSA4eDhkY3Q9MCBjcW09MCBkZWFkem9uZT0yMSwxMSBmYXN0X3Bza2lwPTEgY2hyb21hX3FwX29mZnNldD0tNCB0aHJlYWRzPTIgbG9va2FoZWFkX3RocmVhZHM9MSBzbGljZWRfdGhyZWFkcz0wIG5yPTAgZGVjaW1hdGU9MSBpbnRlcmxhY2VkPTAgYmx1cmF5X2NvbXBhdD0wIGNvbnN0cmFpbmVkX2ludHJhPTAgYmZyYW1lcz0wIHdlaWdodHA9MCBrZXlpbnQ9NCBrZXlpbnRfbWluPTEgc2NlbmVjdXQ9NDAgaW50cmFfcmVmcmVzaD0wIHJjX2xvb2thaGVhZD00IHJjPWNyZiBtYnRyZWU9MSBjcmY9NTEuMCBxY29tcD0wLjYwIHFwbWluPTAgcXBtYXg9NjkgcXBzdGVwPTQgaXBfcmF0aW89MS40MCBhcT0xOjEuMjAAgAAAABhliIQF85yYoAAhfycnJ1111111111114AAAAAHQZo4C+c4RgAAAAhBmlQC+c4RgAAAAAdBmmAVznCM";
const NOSLEEP_WEBM = "data:video/webm;base64,GkXfo59ChoEBQveBAULygQRC84EIQoKEd2VibUKHgQJChYECGFOAZwEAAAAAAAJDEU2bdLpNu4tTq4QVSalmU6yBoU27i1OrhBZUrmtTrIHWTbuMU6uEElTDZ1OsggEyTbuMU6uEHFO7a1OsggIt7AEAAAAAAABZAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAVSalmsCrXsYMPQkBNgIxMYXZmNjMuMS4xMDJXQYxMYXZmNjMuMS4xMDJEiYhAn0AAAAAAABZUrmvXrgEAAAAAAABO14EBc8WIN8F+SwOPL/2cgQAitZyDdW5kiIEAhoVWX1ZQOIOBASPjg4QdzWUA4JCwgUC6gUCagQJVsIRVuYEBVe6BAOwBAAAAAAAAAgAAElTDZ/pzc59jwIBnyJlFo4dFTkNPREVSRIeMTGF2ZjYzLjEuMTAyc3PVY8CLY8WIN8F+SwOPL/1nyKBFo4dFTkNPREVSRIeTTGF2YzYzLjEuMTAyIGxpYnZweGfIoUWjiERVUkFUSU9ORIeTMDA6MDA6MDIuMDAwMDAwMDAwAB9DtnX354EAo6qBAACA8AIAnQEqQABAAABHCIWFiIWEiAICAAZwPEJgCrIg9zAA/v+rUICjloEB9ADRAQABEBAAGAAYWC/0AAiOgACjloED6ADRAQABEBAAGAAYWC/0AAiOgACjloEF3ADRAQABEBAAGAAYWC/0AAiOgAAcU7trkbuPs4EAt4r3gQHxggGx8IED";

const el = {
  app: document.getElementById("app"),
  play: document.getElementById("play"),
  cta: document.getElementById("cta"),
  status: document.getElementById("status"),
  latency: document.getElementById("latency"),
  sub: document.getElementById("sub"),
  weak: document.getElementById("weak"),
  stop: document.getElementById("stop"),
  device: document.getElementById("device"),
};

// ---------- small helpers ----------

function load(name) {
  try {
    return window.localStorage.getItem(name) || "";
  } catch (e) {
    return "";
  }
}

function save(name, value) {
  try {
    window.localStorage.setItem(name, value);
  } catch (e) {
    // Private mode or storage blocked: the page still works for this visit.
  }
}

function randomHex(bytes) {
  const b = new Uint8Array(bytes);
  if (window.crypto && window.crypto.getRandomValues) {
    window.crypto.getRandomValues(b);
  } else {
    for (let i = 0; i < bytes; i++) b[i] = Math.floor(Math.random() * 256);
  }
  let s = "";
  for (let i = 0; i < b.length; i++) s += (b[i] < 16 ? "0" : "") + b[i].toString(16);
  return s;
}

/** This browser's identity: 16 random bytes, kept so it rejoins without asking. */
function deviceKey() {
  let key = load(KEY_STORE);
  if (!/^[0-9a-f]{32}$/.test(key)) {
    key = randomHex(16);
    save(KEY_STORE, key);
  }
  return key;
}

/** One-time QR token from `?join=`, or "" when the address was typed. */
function readJoinToken() {
  const m = /[?&]join=([0-9a-fA-F-]{1,128})(?:&|$)/.exec(window.location.search);
  return m ? m[1].toLowerCase() : "";
}

/** A short silent 8-bit WAV, played on a loop so iOS ignores the silent switch. */
function silentWavUri() {
  const samples = 2000; // 0.25 s at 8 kHz
  const bytes = new Uint8Array(44 + samples);
  const dv = new DataView(bytes.buffer);
  const text = (off, s) => {
    for (let i = 0; i < s.length; i++) bytes[off + i] = s.charCodeAt(i);
  };
  text(0, "RIFF");
  dv.setUint32(4, 36 + samples, true);
  text(8, "WAVE");
  text(12, "fmt ");
  dv.setUint32(16, 16, true);
  dv.setUint16(20, 1, true); // PCM
  dv.setUint16(22, 1, true); // mono
  dv.setUint32(24, 8000, true);
  dv.setUint32(28, 8000, true);
  dv.setUint16(32, 1, true);
  dv.setUint16(34, 8, true);
  text(36, "data");
  dv.setUint32(40, samples, true);
  for (let i = 0; i < samples; i++) bytes[44 + i] = 128;
  let bin = "";
  for (let i = 0; i < bytes.length; i++) bin += String.fromCharCode(bytes[i]);
  return "data:audio/wav;base64," + window.btoa(bin);
}

function quietPlay(media) {
  try {
    const p = media.play();
    if (p && p.catch) p.catch(() => {});
  } catch (e) {
    // Not allowed here: playback still works on most browsers.
  }
}

// ---------- state ----------

const key = deviceKey();
let joinToken = readJoinToken();
let weak = load(WEAK_STORE) === "1";
let pcName = "your PC";

let ctx = null;
let output = null;
let scheduler = createScheduler({ targetMs: weak ? WEAK_TARGET_MS : TARGET_MS, rate: RATE });
const sources = new Set();

let unmute = null;
let nosleep = null;

let ws = null;
let wanted = false; // the viewer asked to play
let streaming = false; // the PC is sending audio on this socket
let attempt = 0;
let retryTimer = 0;
let connectTimer = 0;
let lastLatencyMs = 0;

const AudioContextClass = window.AudioContext || window.webkitAudioContext;
const supported = !!AudioContextClass && typeof window.WebSocket === "function";

// ---------- view ----------

const VIEW = {
  idle: { cta: "Tap to play", status: "Play your PC's sound here, in sync." },
  connecting: { cta: "", status: "Connecting to your PC…" },
  waiting: { cta: "", status: "Waiting for the PC to allow this device…", sub: "Choose Allow in SyncSound on your PC." },
  playing: { cta: "", status: "" },
  paused: { cta: "Tap to play", status: "Sound paused on this device." },
  reconnecting: { cta: "", status: "Disconnected, reconnecting…" },
  denied: { cta: "Tap to try again", status: "The PC didn't allow this device." },
  unsupported: { cta: "", status: "This browser can't play SyncSound audio" },
};

let view = "idle";

function show(state) {
  view = state;
  const v = VIEW[state];
  el.app.setAttribute("data-state", state);
  el.cta.textContent = v.cta;
  el.status.textContent = state === "playing" ? "Playing · in sync with " + pcName : v.status;
  el.sub.textContent = v.sub || "";
  const tappable = state === "idle" || state === "paused" || state === "denied";
  el.play.disabled = state === "unsupported";
  el.play.setAttribute("aria-label", tappable ? v.cta : v.status);
  el.play.setAttribute("aria-disabled", tappable ? "false" : "true");
  el.stop.hidden = state === "idle" || state === "denied" || state === "unsupported";
  renderLatency();
}

function renderLatency() {
  const ms = Math.round(lastLatencyMs);
  el.latency.textContent = view === "playing" && ms > 0 ? " · " + ms + " ms" : "";
}

function renderWeak() {
  el.weak.setAttribute("aria-checked", weak ? "true" : "false");
  el.weak.title = weak ? "More delay, steadier sound" : "Use on a slow or busy Wi-Fi";
}

// ---------- audio ----------

/** Must run inside the tap: iOS only unlocks audio from a user gesture. */
function unlockAudio() {
  if (!ctx) {
    try {
      ctx = new AudioContextClass({ sampleRate: RATE });
    } catch (e) {
      ctx = new AudioContextClass();
    }
    output = ctx.createGain();
    output.connect(ctx.destination);
    ctx.onstatechange = onContextState;
  }
  if (ctx.state !== "running" && ctx.resume) {
    const p = ctx.resume();
    if (p && p.catch) p.catch(() => {});
  }
  // A one-sample silent buffer started in the gesture finishes the unlock
  // on older iOS versions.
  const blank = ctx.createBuffer(1, 1, ctx.sampleRate);
  const src = ctx.createBufferSource();
  src.buffer = blank;
  src.connect(ctx.destination);
  src.start(0);

  if (!unmute) {
    unmute = document.createElement("audio");
    unmute.setAttribute("x-webkit-airplay", "deny");
    unmute.loop = true;
    unmute.preload = "auto";
    unmute.src = silentWavUri();
    unmute.className = "keepalive";
    document.body.appendChild(unmute);
  }
  quietPlay(unmute);

  if (!nosleep) {
    nosleep = document.createElement("video");
    nosleep.muted = true;
    nosleep.loop = true;
    nosleep.setAttribute("muted", "");
    nosleep.setAttribute("playsinline", "");
    nosleep.setAttribute("webkit-playsinline", "");
    nosleep.setAttribute("aria-hidden", "true");
    nosleep.className = "keepalive";
    const webm = document.createElement("source");
    webm.src = NOSLEEP_WEBM;
    webm.type = "video/webm";
    const mp4 = document.createElement("source");
    mp4.src = NOSLEEP_MP4;
    mp4.type = "video/mp4";
    nosleep.appendChild(webm);
    nosleep.appendChild(mp4);
    document.body.appendChild(nosleep);
  }
  quietPlay(nosleep);
}

function releaseKeepAlive() {
  if (unmute) unmute.pause();
  if (nosleep) nosleep.pause();
}

/** Stops everything scheduled and starts a fresh timeline on the next chunk. */
function flushAudio() {
  sources.forEach((s) => {
    try {
      s.stop();
    } catch (e) {
      // Already ended.
    }
  });
  sources.clear();
  scheduler = createScheduler({ targetMs: weak ? WEAK_TARGET_MS : TARGET_MS, rate: RATE });
}

function onContextState() {
  if (!ctx || !wanted) return;
  if (ctx.state === "running") {
    if (view === "paused") show(streaming ? "playing" : "connecting");
  } else {
    // iOS "interrupted" (a call, Siri) or "suspended": the clock stopped, so
    // what is scheduled is stale. A tap resumes.
    flushAudio();
    if (streaming) show("paused");
  }
}

function outputLatencySec() {
  if (typeof ctx.outputLatency === "number" && ctx.outputLatency > 0) return ctx.outputLatency;
  return typeof ctx.baseLatency === "number" ? ctx.baseLatency : 0;
}

function onAudioFrame(buf) {
  if (!ctx || ctx.state !== "running" || buf.byteLength !== FRAME_BYTES) return;
  const dv = new DataView(buf);
  if (dv.getUint8(0) !== KIND_AUDIO) return;
  const seq = dv.getUint32(1, true) + dv.getUint32(5, true) * 4294967296;
  const flags = dv.getUint8(9);
  if ((flags & FLAG_RESYNC) !== 0 && sources.size > 0) flushAudio();
  const slot = scheduler.enqueue(seq, flags, ctx.currentTime);
  if (!slot) return;

  const audio = ctx.createBuffer(2, CHUNK_FRAMES, RATE);
  const left = audio.getChannelData(0);
  const right = audio.getChannelData(1);
  for (let i = 0, o = HEADER_BYTES; i < CHUNK_FRAMES; i++, o += 4) {
    left[i] = dv.getInt16(o, true) / 32768;
    right[i] = dv.getInt16(o + 2, true) / 32768;
  }
  const src = ctx.createBufferSource();
  src.buffer = audio;
  src.playbackRate.value = slot.playbackRate;
  src.connect(output);
  src.onended = () => sources.delete(src);
  sources.add(src);
  src.start(slot.startAt);
}

// ---------- connection ----------

function connect() {
  clearTimeout(retryTimer);
  if (!wanted) return;
  show(attempt === 0 ? "connecting" : "reconnecting");
  const scheme = window.location.protocol === "https:" ? "wss://" : "ws://";
  let sock;
  try {
    sock = new WebSocket(scheme + window.location.host + "/ws");
  } catch (e) {
    scheduleRetry();
    return;
  }
  ws = sock;
  sock.binaryType = "arraybuffer";
  connectTimer = setTimeout(() => {
    if (sock.readyState !== WebSocket.OPEN) sock.close();
  }, CONNECT_TIMEOUT_MS);

  sock.onopen = () => {
    clearTimeout(connectTimer);
    const hello = { type: "hello", key: key };
    if (joinToken) hello.join = joinToken;
    sock.send(JSON.stringify(hello));
  };

  sock.onmessage = (e) => {
    if (sock !== ws) return;
    if (typeof e.data !== "string") {
      if (streaming) onAudioFrame(e.data);
      return;
    }
    let msg;
    try {
      msg = JSON.parse(e.data);
    } catch (err) {
      return;
    }
    if (msg.type === "welcome") onWelcome(msg);
    else if (msg.type === "denied") onDenied();
  };

  sock.onclose = () => {
    clearTimeout(connectTimer);
    if (sock !== ws) return;
    ws = null;
    streaming = false;
    flushAudio();
    if (wanted) scheduleRetry();
  };
}

function onWelcome(msg) {
  // The token works once: never send it again, and drop it from the address
  // so a reload does not count as a failed join.
  if (joinToken) {
    joinToken = "";
    try {
      window.history.replaceState(null, "", window.location.pathname);
    } catch (e) {
      // Old browsers: harmless.
    }
  }
  if (typeof msg.pc === "string" && msg.pc) pcName = msg.pc;
  if (typeof msg.name === "string" && msg.name) el.device.textContent = "This device: " + msg.name;
  attempt = 0;
  if (msg.state === "playing") {
    streaming = true;
    flushAudio();
    show(ctx && ctx.state !== "running" ? "paused" : "playing");
  } else {
    show("waiting");
  }
}

function onDenied() {
  wanted = false;
  streaming = false;
  joinToken = "";
  flushAudio();
  releaseKeepAlive();
  show("denied");
}

function scheduleRetry() {
  show("reconnecting");
  const delay = BACKOFF_MS[Math.min(attempt, BACKOFF_MS.length - 1)];
  attempt++;
  retryTimer = setTimeout(connect, delay);
}

function start() {
  unlockAudio();
  if (wanted) {
    // Already running: this tap only resumed a paused context.
    return;
  }
  wanted = true;
  attempt = 0;
  connect();
}

function stop() {
  wanted = false;
  streaming = false;
  clearTimeout(retryTimer);
  clearTimeout(connectTimer);
  const sock = ws;
  ws = null;
  if (sock) sock.close();
  flushAudio();
  releaseKeepAlive();
  if (ctx && ctx.suspend) {
    const p = ctx.suspend();
    if (p && p.catch) p.catch(() => {});
  }
  lastLatencyMs = 0;
  show("idle");
}

// Reports how far behind the PC this device plays, once a second.
setInterval(() => {
  if (!ctx || !streaming || !ws || ws.readyState !== WebSocket.OPEN) return;
  const now = ctx.currentTime;
  if (scheduler.bufferedMs(now) <= 0) {
    lastLatencyMs = 0;
  } else {
    lastLatencyMs = scheduler.latencyMs(now, outputLatencySec());
    ws.send(JSON.stringify({ type: "stats", latency_ms: Math.round(lastLatencyMs * 10) / 10 }));
  }
  renderLatency();
}, 1000);

// ---------- wiring ----------

if (!supported) {
  show("unsupported");
} else {
  renderWeak();
  show("idle");
  el.play.addEventListener("click", start);
  el.stop.addEventListener("click", stop);
  el.weak.addEventListener("click", () => {
    weak = !weak;
    save(WEAK_STORE, weak ? "1" : "0");
    renderWeak();
    flushAudio();
  });
  document.addEventListener("visibilitychange", () => {
    if (document.visibilityState === "visible" && wanted && ctx && ctx.state !== "running") {
      const p = ctx.resume();
      if (p && p.catch) p.catch(() => {});
    }
  });
}

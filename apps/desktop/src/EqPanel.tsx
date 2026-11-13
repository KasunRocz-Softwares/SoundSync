import React, { useEffect, useMemo, useState } from 'react';
import { api } from './api';
import { ROOM_BANDS, type EngineSnapshot, type EqSettings, type OutputStatus } from './types';

const LOW_CUTS = [0, 40, 60, 80, 100, 120, 150];
const PRESETS: { label: string; eq: Omit<EqSettings, 'enabled' | 'room_correction' | 'correction_db'> }[] = [
  { label: 'Flat', eq: { low_cut_hz: 0, bass_db: 0, mid_db: 0, treble_db: 0 } },
  { label: 'Bass boost', eq: { low_cut_hz: 0, bass_db: 6, mid_db: 0, treble_db: 1 } },
  { label: 'Small speaker', eq: { low_cut_hz: 100, bass_db: -2, mid_db: 1, treble_db: 0 } },
  { label: 'Voice', eq: { low_cut_hz: 80, bass_db: -3, mid_db: 3, treble_db: 1 } },
  { label: 'Bright', eq: { low_cut_hz: 0, bass_db: 0, mid_db: -1, treble_db: 4 } },
];

// --- Biquad magnitude (mirrors the engine's RBJ design) for the response curve.
type Bq = [number, number, number, number, number];
const RATE = 48000;
function norm(b0: number, b1: number, b2: number, a0: number, a1: number, a2: number): Bq {
  return [b0 / a0, b1 / a0, b2 / a0, a1 / a0, a2 / a0];
}
function shelf(freq: number, db: number, high: boolean): Bq | null {
  if (Math.abs(db) < 0.01) return null;
  const A = 10 ** (db / 40), w = 2 * Math.PI * freq / RATE, s = Math.sin(w), c = Math.cos(w);
  const sq = 2 * Math.sqrt(A) * (s / 2 * Math.SQRT2);
  return high
    ? norm(A * ((A + 1) + (A - 1) * c + sq), -2 * A * ((A - 1) + (A + 1) * c), A * ((A + 1) + (A - 1) * c - sq),
      (A + 1) - (A - 1) * c + sq, 2 * ((A - 1) - (A + 1) * c), (A + 1) - (A - 1) * c - sq)
    : norm(A * ((A + 1) - (A - 1) * c + sq), 2 * A * ((A - 1) - (A + 1) * c), A * ((A + 1) - (A - 1) * c - sq),
      (A + 1) + (A - 1) * c + sq, -2 * ((A - 1) + (A + 1) * c), (A + 1) + (A - 1) * c - sq);
}
function peak(freq: number, db: number, q: number): Bq | null {
  if (Math.abs(db) < 0.01) return null;
  const A = 10 ** (db / 40), w = 2 * Math.PI * freq / RATE, al = Math.sin(w) / (2 * q), c = Math.cos(w);
  return norm(1 + al * A, -2 * c, 1 - al * A, 1 + al / A, -2 * c, 1 - al / A);
}
function highPass(freq: number): Bq {
  const w = 2 * Math.PI * freq / RATE, al = Math.sin(w) / (2 * Math.SQRT1_2), c = Math.cos(w);
  return norm((1 + c) / 2, -(1 + c), (1 + c) / 2, 1 + al, -2 * c, 1 - al);
}
function mag([b0, b1, b2, a1, a2]: Bq, f: number) {
  const w = 2 * Math.PI * f / RATE;
  const nr = b0 + b1 * Math.cos(w) + b2 * Math.cos(2 * w), ni = -(b1 * Math.sin(w) + b2 * Math.sin(2 * w));
  const dr = 1 + a1 * Math.cos(w) + a2 * Math.cos(2 * w), di = -(a1 * Math.sin(w) + a2 * Math.sin(2 * w));
  return Math.sqrt((nr * nr + ni * ni) / (dr * dr + di * di));
}
function responseDb(eq: EqSettings, f: number) {
  const manual = eq.enabled
    ? [eq.low_cut_hz ? highPass(eq.low_cut_hz) : null, shelf(100, eq.bass_db, false), peak(1000, eq.mid_db, 0.9), shelf(8000, eq.treble_db, true)]
    : [];
  const room = eq.room_correction ? ROOM_BANDS.map((b, i) => (b < RATE * 0.45 ? peak(b, eq.correction_db[i] ?? 0, 1.41) : null)) : [];
  const headroom = -(eq.enabled ? Math.max(0, eq.bass_db, eq.mid_db, eq.treble_db) : 0)
    - (eq.room_correction ? Math.max(0, ...eq.correction_db) : 0);
  return [...manual, ...room].reduce((db, s) => db + (s ? 20 * Math.log10(mag(s, f)) : 0), headroom);
}

function Curve({ eq }: { eq: EqSettings }) {
  const W = 360, H = 120, min = -18, max = 12;
  const xs = (f: number) => (Math.log10(f / 20) / Math.log10(20000 / 20)) * W;
  const ys = (db: number) => H - ((Math.max(min, Math.min(max, db)) - min) / (max - min)) * H;
  const path = useMemo(() => {
    const pts: string[] = [];
    for (let i = 0; i <= 120; i++) {
      const f = 20 * (1000 ** (i / 120));
      pts.push(`${i ? 'L' : 'M'}${xs(f).toFixed(1)},${ys(responseDb(eq, f)).toFixed(1)}`);
    }
    return pts.join(' ');
  }, [eq.enabled, eq.low_cut_hz, eq.bass_db, eq.mid_db, eq.treble_db, eq.room_correction, eq.correction_db.join(',')]);
  return <svg className="eq-curve" viewBox={`0 0 ${W} ${H + 16}`} role="img" aria-label="Frequency response">
    {[100, 1000, 10000].map(f => <g key={f}><line x1={xs(f)} x2={xs(f)} y1={0} y2={H} /><text x={xs(f)} y={H + 12}>{f >= 1000 ? `${f / 1000}k` : f}</text></g>)}
    <line className="zero" x1={0} x2={W} y1={ys(0)} y2={ys(0)} />
    <path d={path} className={eq.enabled || eq.room_correction ? 'on' : ''} />
  </svg>;
}

/** Slider with local state; commits when released. */
function BandSlider({ label, value, disabled, onCommit }: { label: string; value: number; disabled: boolean; onCommit: (v: number) => void }) {
  const [v, setV] = useState(value);
  const [dragging, setDragging] = useState(false);
  useEffect(() => { if (!dragging) setV(value); }, [value, dragging]);
  const commit = () => { setDragging(false); if (v !== value) onCommit(v); };
  return <label className="band">
    <span>{label}</span>
    <input type="range" min={-12} max={12} step={0.5} value={v} disabled={disabled} aria-label={`${label} in dB`}
      onPointerDown={() => setDragging(true)} onChange={e => setV(Number(e.target.value))} onPointerUp={commit} onKeyUp={commit} />
    <strong>{v > 0 ? '+' : ''}{v} dB</strong>
  </label>;
}

function OutputEq({ output, act }: { output: OutputStatus; act: (fn: () => Promise<unknown>) => void }) {
  const eq = output.config.eq;
  const set = (patch: Partial<EqSettings>) => act(() => api.eq(output.endpoint_id, { ...eq, ...patch }));
  return <article className={`device eq-card ${eq.enabled ? 'on' : ''}`}>
    <div className="device-top">
      <div className="device-info"><h3>{output.name}</h3><p>{eq.enabled ? 'Equalizer on' : 'Equalizer off'}{eq.room_correction ? ' · room correction on' : ''}</p></div>
      <label className="switch">
        <input type="checkbox" checked={eq.enabled} onChange={e => set({ enabled: e.target.checked })} />
        <span aria-hidden="true" /><span className="sr">Equalizer for {output.name}</span>
      </label>
    </div>
    <div className="eq-body">
      <div className="eq-controls">
        <label className="band">
          <span>Low cut</span>
          <select value={eq.low_cut_hz} disabled={!eq.enabled} onChange={e => set({ low_cut_hz: Number(e.target.value) })} aria-label="Low cut frequency">
            {LOW_CUTS.map(f => <option key={f} value={f}>{f ? `${f} Hz` : 'Off'}</option>)}
          </select>
          <strong className="muted">removes deep bass a small speaker can't play</strong>
        </label>
        <BandSlider label="Bass" value={eq.bass_db} disabled={!eq.enabled} onCommit={v => set({ bass_db: v })} />
        <BandSlider label="Mid" value={eq.mid_db} disabled={!eq.enabled} onCommit={v => set({ mid_db: v })} />
        <BandSlider label="Treble" value={eq.treble_db} disabled={!eq.enabled} onCommit={v => set({ treble_db: v })} />
        <div className="presets">
          {PRESETS.map(p => <button key={p.label} disabled={!eq.enabled} onClick={() => set(p.eq)}>{p.label}</button>)}
        </div>
        <div className="room-row">
          <label className="check">
            <input type="checkbox" checked={eq.room_correction} disabled={eq.correction_db.every(c => c === 0)}
              onChange={e => set({ room_correction: e.target.checked })} />
            Room correction
          </label>
          {eq.correction_db.some(c => c !== 0)
            ? <span className="bands">{ROOM_BANDS.map((b, i) => eq.correction_db[i] ? <span key={b} className={eq.correction_db[i] < 0 ? 'cut' : 'boost'}>{b >= 1000 ? `${b / 1000}k` : b} {eq.correction_db[i] > 0 ? '+' : ''}{eq.correction_db[i].toFixed(1)}</span> : null)}</span>
            : <span className="muted">Not measured yet — use Measure room on the Speaker setup tab.</span>}
        </div>
      </div>
      <Curve eq={eq} />
    </div>
  </article>;
}

export function EqPanel({ snapshot, act }: { snapshot: EngineSnapshot | null; act: (fn: () => Promise<unknown>) => void }) {
  const outputs = (snapshot?.outputs ?? []).filter(o => o.config.enabled);
  return <section>
    <p className="hint top">Shape each speaker separately. Bass at 100 Hz, mid at 1 kHz, treble at 8 kHz, ±12 dB. Boosts lower the overall level by the same amount so they never clip. Settings are saved in profiles.</p>
    <div className="devices">
      {outputs.map(o => <OutputEq key={o.endpoint_id} output={o} act={act} />)}
      {!outputs.length && <div className="empty">Turn on an output on the Routing tab first.</div>}
    </div>
  </section>;
}

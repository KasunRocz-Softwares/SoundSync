import React, { useEffect, useState } from 'react';
import { AudioLines, Speaker, Waves } from 'lucide-react';
import { api } from './api';
import { CROSSOVER_OPTIONS, type BassRole, type EngineSnapshot, type OutputStatus, type SoundSettings } from './types';

type Act = (fn: () => Promise<unknown>) => void;

const ROLES: { id: BassRole; label: string; detail: string }[] = [
  { id: 'full', label: 'Full range', detail: 'Plays everything, bass included.' },
  { id: 'satellite', label: 'Small speaker', detail: 'Deep bass removed so it plays cleaner and louder.' },
  { id: 'subwoofer', label: 'Subwoofer', detail: 'Plays only the bass, in mono.' },
];

/** Range slider with local state; commits when released. */
function Slider({ label, value, min, max, step, format, disabled, onCommit }: {
  label: string; value: number; min: number; max: number; step: number;
  format: (v: number) => string; disabled?: boolean; onCommit: (v: number) => void;
}) {
  const [v, setV] = useState(value);
  const [dragging, setDragging] = useState(false);
  useEffect(() => { if (!dragging) setV(value); }, [value, dragging]);
  const commit = () => { setDragging(false); if (v !== value) onCommit(v); };
  return <label className="band">
    <span>{label}</span>
    <input type="range" min={min} max={max} step={step} value={v} disabled={disabled} aria-label={label}
      onPointerDown={() => setDragging(true)} onChange={e => setV(Number(e.target.value))} onPointerUp={commit} onKeyUp={commit} />
    <strong>{format(v)}</strong>
  </label>;
}

const pct = (v: number) => `${Math.round(v * 100)}%`;
const tilt = (v: number) => (v === 0 ? 'Neutral' : v < 0 ? `Warm ${-v}` : `Bright +${v}`);

function BassRow({ output, act }: { output: OutputStatus; act: Act }) {
  const eq = output.config.eq;
  const set = (patch: Partial<typeof eq>) => act(() => api.eq(output.endpoint_id, { ...eq, ...patch }));
  return <article className="device bass-row">
    <div className="device-top">
      <div className="device-info"><h3>{output.name}</h3>
        <p>{ROLES.find(r => r.id === eq.bass_role)?.detail}</p></div>
      <div className="segmented" role="group" aria-label={`Bass role for ${output.name}`}>
        {ROLES.map(r => <button key={r.id} className={eq.bass_role === r.id ? 'selected' : ''}
          aria-pressed={eq.bass_role === r.id} onClick={() => set({ bass_role: r.id })}>{r.label}</button>)}
      </div>
    </div>
    {eq.bass_role !== 'subwoofer' && <div className="bass-vb">
      <Slider label="Virtual bass" value={eq.virtual_bass} min={0} max={1} step={0.05} format={pct}
        onCommit={v => set({ virtual_bass: v })} />
    </div>}
  </article>;
}

export function SoundPanel({ snapshot, act }: { snapshot: EngineSnapshot | null; act: Act }) {
  const sound = snapshot?.sound;
  const outputs = (snapshot?.outputs ?? []).filter(o => o.config.enabled);
  if (!sound) return <div className="empty">Loading…</div>;
  const set = (patch: Partial<SoundSettings>) => act(() => api.sound({ ...sound, ...patch }));
  const roles = outputs.map(o => o.config.eq.bass_role);
  const noBass = roles.length > 0 && roles.every(r => r === 'satellite');
  const subOnly = roles.length > 0 && roles.every(r => r === 'subwoofer');

  return <section>
    <p className="hint top">These work on every speaker at once and are saved in profiles. Each one is off or neutral until you change it.</p>

    <section className="panel">
      <div className="setup-head"><h2><AudioLines size={16} /> Tone</h2></div>
      <div className="eq-controls sound-controls">
        <Slider label="Tilt" value={sound.tilt_db} min={-6} max={6} step={0.5} format={tilt}
          onCommit={v => set({ tilt_db: v })} />
        <Slider label="Loudness" value={sound.loudness} min={0} max={1} step={0.05} format={pct}
          onCommit={v => set({ loudness: v })} />
      </div>
      <p className="hint">Tilt warms up harsh, bright speakers (left) or adds clarity to dull ones (right). Loudness adds back the bass and treble your ears lose at low volume: turn it up when you listen quietly, down when you play loud.</p>
    </section>

    <section className="panel">
      <div className="setup-head"><h2><Speaker size={16} /> Bass</h2>
        <label className="band crossover">
          <span>Crossover</span>
          <select value={sound.crossover_hz} onChange={e => set({ crossover_hz: Number(e.target.value) })} aria-label="Crossover frequency">
            {CROSSOVER_OPTIONS.map(f => <option key={f} value={f}>{f} Hz</option>)}
          </select>
        </label>
      </div>
      <p className="hint top">Small speakers can't play deep bass, and pushing it makes them distort. Mark them <strong>Small speaker</strong> and send the bass to a <strong>Subwoofer</strong> or a bigger speaker instead. <strong>Virtual bass</strong> adds harmonics so you still hear the low notes on a speaker that can't play them.</p>
      <div className="devices">
        {outputs.map(o => <BassRow key={o.endpoint_id} output={o} act={act} />)}
        {!outputs.length && <div className="empty">Turn on an output on the Routing tab first.</div>}
      </div>
      {noBass && <p className="field-error" role="alert">Every speaker is set to Small speaker, so nothing plays the deep bass. Set your biggest speaker to Full range or Subwoofer, or turn up Virtual bass.</p>}
      {subOnly && <p className="field-error" role="alert">Every speaker is set to Subwoofer, so you only hear bass.</p>}
    </section>

    <section className="panel">
      <div className="setup-head"><h2><Waves size={16} /> Everyday</h2></div>
      <label className="setting">
        <input type="checkbox" checked={sound.normalize} onChange={e => set({ normalize: e.target.checked })} />
        <span>Even out volume<small>Slowly raises quiet videos and lowers loud ads and songs, up to 12 dB either way.</small></span>
      </label>
      <label className="setting">
        <input type="checkbox" checked={sound.night_mode} onChange={e => set({ night_mode: e.target.checked })} />
        <span>Night mode<small>Lifts quiet dialogue and holds back explosions, so you can listen late without waking anyone.</small></span>
      </label>
      <label className="setting">
        <input type="checkbox" checked={sound.match_levels} onChange={e => set({ match_levels: e.target.checked })} />
        <span>Match speaker levels when calibrating<small>Calibrate delays also measures how loud each speaker is at your seat and turns the louder ones down, so none of them dominates.</small></span>
      </label>
    </section>
  </section>;
}

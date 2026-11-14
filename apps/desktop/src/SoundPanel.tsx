import React, { useEffect, useState } from 'react';
import { Activity, AppWindow, AudioLines, Clapperboard, Gamepad2, Mic, Music, Plus, Speaker, Waves, X } from 'lucide-react';
import { api, errorMessage } from './api';
import {
  CROSSOVER_OPTIONS, ROOM_BANDS, type AppMode, type AudioApp, type BassRole, type EngineSnapshot,
  type ListeningMode, type OutputSpectrum, type OutputStatus, type SoundSettings,
} from './types';

type Act = (fn: () => Promise<unknown>) => void;

const ROLES: { id: BassRole; label: string; detail: string }[] = [
  { id: 'full', label: 'Full range', detail: 'Plays everything, bass included.' },
  { id: 'satellite', label: 'Small speaker', detail: 'Deep bass removed so it plays cleaner and louder.' },
  { id: 'subwoofer', label: 'Subwoofer', detail: 'Plays only the bass, in mono.' },
];

const MODES: { id: Exclude<ListeningMode, 'custom'>; label: string; detail: string; icon: React.ReactNode }[] = [
  { id: 'music', label: 'Music', detail: 'Fuller and wider', icon: <Music size={16} /> },
  { id: 'movie', label: 'Movie', detail: 'Wide, clear dialogue', icon: <Clapperboard size={16} /> },
  { id: 'game', label: 'Game', detail: 'Wide, crisp footsteps', icon: <Gamepad2 size={16} /> },
  { id: 'voice', label: 'Voice', detail: 'Calls and podcasts', icon: <Mic size={16} /> },
];
const modeLabel = (m: ListeningMode) => MODES.find(x => x.id === m)?.label ?? 'Custom';

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
const width = (v: number) => (v === 1 ? 'Normal' : v === 0 ? 'Mono' : pct(v));
const bandLabel = (f: number) => (f >= 1000 ? `${f / 1000}k` : `${f}`);

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

/** Live octave-band levels of what each speaker plays, after every effect. */
function SpectrumCard({ outputs, running }: { outputs: OutputStatus[]; running: boolean }) {
  const [spectra, setSpectra] = useState<OutputSpectrum[]>([]);
  useEffect(() => {
    if (!running) { setSpectra([]); return; }
    let alive = true;
    const tick = () => api.spectrum().then(s => { if (alive) setSpectra(s); }, () => {});
    tick();
    const timer = window.setInterval(tick, 100);
    return () => { alive = false; window.clearInterval(timer); };
  }, [running]);
  const height = (db: number) => Math.max(0, Math.min(1, (db + 66) / 60));
  return <section className="panel">
    <div className="setup-head"><h2><Activity size={16} /> What each speaker plays</h2></div>
    {!running && <p className="hint top">Start routing to see the live levels.</p>}
    {running && <div className="spectra">
      {outputs.map(o => {
        const levels = spectra.find(s => s.endpoint_id === o.endpoint_id)?.levels_db ?? Array(10).fill(-90);
        return <figure key={o.endpoint_id} className="spectrum" aria-label={`Live spectrum of ${o.name}`}>
          <div className="bars">
            {levels.map((db, i) => <div key={i} className="bar-slot" title={`${bandLabel(ROOM_BANDS[i])} Hz: ${db.toFixed(0)} dB`}>
              <div className={`bar ${i < 2 ? 'low' : ''}`} style={{ transform: `scaleY(${height(db)})` }} />
            </div>)}
          </div>
          <div className="bar-labels">{ROOM_BANDS.map(b => <span key={b}>{bandLabel(b)}</span>)}</div>
          <figcaption>{o.name}</figcaption>
        </figure>;
      })}
    </div>}
    <p className="hint">Measured after bass routing, virtual bass, tone and the Equalizer, so you can see each change. Turn on Virtual bass on a small speaker and watch its 63–250 Hz bars rise.</p>
  </section>;
}

/** Apps that switch the listening mode while they play. */
function AppModesCard({ snapshot, act }: { snapshot: EngineSnapshot; act: Act }) {
  const [apps, setApps] = useState<AudioApp[]>([]);
  const [exe, setExe] = useState('');
  const [mode, setMode] = useState<AppMode['mode']>('music');
  const [error, setError] = useState('');
  const load = () => api.apps().then(a => { setApps(a); setError(''); }, e => setError(errorMessage(e)));
  useEffect(() => { void load(); }, []);
  const modes = snapshot.app_modes;
  const choices = apps.filter(a => !modes.some(m => m.exe.toLowerCase() === a.exe.toLowerCase())
    && a.exe.toLowerCase() !== 'syncsound-desktop.exe');
  const save = (next: AppMode[]) => act(() => api.appModes(next));
  return <section className="panel">
    <div className="setup-head"><h2><AppWindow size={16} /> Switch mode by app</h2></div>
    <p className="hint top">While a listed app is playing sound, SyncSound uses its mode; when none is playing, your own settings come back. The first app in the list wins if several play at once.</p>
    {modes.length > 0 && <ul className="app-modes">
      {modes.map((m, i) => <li key={m.exe}>
        <span className="exe">{m.exe}</span>
        <select value={m.mode} aria-label={`Mode for ${m.exe}`}
          onChange={e => save(modes.map((x, k) => (k === i ? { ...x, mode: e.target.value as AppMode['mode'] } : x)))}>
          {MODES.map(x => <option key={x.id} value={x.id}>{x.label}</option>)}
        </select>
        <button className="icon-btn" aria-label={`Remove ${m.exe}`} onClick={() => save(modes.filter((_, k) => k !== i))}><X size={14} /></button>
      </li>)}
    </ul>}
    <div className="app-mode-add">
      <select value={exe} onChange={e => setExe(e.target.value)} aria-label="App">
        <option value="">{choices.length ? 'Choose an app that is playing…' : 'No other apps are playing sound'}</option>
        {choices.map(a => <option key={a.exe} value={a.exe}>{a.name} ({a.exe})</option>)}
      </select>
      <select value={mode} onChange={e => setMode(e.target.value as AppMode['mode'])} aria-label="Listening mode">
        {MODES.map(x => <option key={x.id} value={x.id}>{x.label}</option>)}
      </select>
      <button disabled={!exe} onClick={() => { save([...modes, { exe, mode }]); setExe(''); }}><Plus size={14} /> Add</button>
      <button onClick={() => void load()}>Refresh apps</button>
    </div>
    {error && <p className="field-error">{error}</p>}
  </section>;
}

export function SoundPanel({ snapshot, act }: { snapshot: EngineSnapshot | null; act: Act }) {
  const sound = snapshot?.sound;
  const outputs = (snapshot?.outputs ?? []).filter(o => o.config.enabled);
  if (!snapshot || !sound) return <div className="empty">Loading…</div>;
  const set = (patch: Partial<SoundSettings>) => act(() => api.sound({ ...sound, ...patch }));
  const roles = outputs.map(o => o.config.eq.bass_role);
  const noBass = roles.length > 0 && roles.every(r => r === 'satellite');
  const subOnly = roles.length > 0 && roles.every(r => r === 'subwoofer');
  const auto = snapshot.auto_mode;

  return <section>
    <section className="panel">
      <div className="setup-head"><h2><Waves size={16} /> Listening mode</h2>
        <span className="muted">{snapshot.listening_mode === 'custom' ? 'Custom settings' : `${modeLabel(snapshot.listening_mode)} settings`}</span></div>
      <div className="mode-grid" role="group" aria-label="Listening mode">
        {MODES.map(m => <button key={m.id} className={`mode ${snapshot.listening_mode === m.id ? 'selected' : ''}`}
          aria-pressed={snapshot.listening_mode === m.id} onClick={() => act(() => api.listeningMode(m.id))}>
          {m.icon}<strong>{m.label}</strong><small>{m.detail}</small>
        </button>)}
      </div>
      {auto
        ? <p className="capture ok"><span className="dot" />{auto.exe} is playing, so {modeLabel(auto.mode)} mode is on. The tone controls below come back when it stops.</p>
        : <p className="hint">A mode sets the tone controls below in one click. Change any of them afterwards to make it your own.</p>}
    </section>

    <SpectrumCard outputs={outputs} running={snapshot.running} />

    <section className="panel">
      <div className="setup-head"><h2><AudioLines size={16} /> Tone</h2></div>
      <div className="eq-controls sound-controls">
        <Slider label="Tilt" value={sound.tilt_db} min={-6} max={6} step={0.5} format={tilt}
          onCommit={v => set({ tilt_db: v })} />
        <Slider label="Loudness" value={sound.loudness} min={0} max={1} step={0.05} format={pct}
          onCommit={v => set({ loudness: v })} />
        <Slider label="Width" value={sound.width} min={0} max={2} step={0.05} format={width}
          onCommit={v => set({ width: v })} />
        <Slider label="De-harsh" value={sound.deharsh} min={0} max={1} step={0.05} format={pct}
          onCommit={v => set({ deharsh: v })} />
      </div>
      <ul className="hint tone-help">
        <li><strong>Tilt</strong> warms up harsh, bright speakers (left) or adds clarity to dull ones (right).</li>
        <li><strong>Loudness</strong> puts back the bass and treble your ears lose at low volume: more when you listen quietly.</li>
        <li><strong>Width</strong> spreads speakers that sit close together (over 100%) or narrows the sound toward mono.</li>
        <li><strong>De-harsh</strong> turns down the sharp 2–6 kHz range only while it is loud, so quiet detail stays.</li>
      </ul>
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

    <AppModesCard snapshot={snapshot} act={act} />

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

import React, { useCallback, useEffect, useMemo, useState } from 'react';
import { createRoot } from 'react-dom/client';
import {
  Activity, AlertTriangle, AppWindow, Settings, SlidersHorizontal, Speaker, Download, FolderOpen, Headphones, Play, Power, RefreshCw, Save, Volume2, VolumeX,
} from 'lucide-react';
import { api, errorMessage, type SourceKind } from './api';
import {
  MAX_DELAY_MS, MAX_DISTANCE_M, MAX_ENABLED_OUTPUTS, MIN_DISTANCE_M, MIN_TRIM_DB,
  type AudioEndpoint, type AudioMode, type CaptureSource, type CaptureState, type ChannelRole, type EngineSnapshot,
  type LifecycleState, type OutputStatus, type ProfileLoadReport, type SoundPreset, type SpeakerPosition,
} from './types';
import { RoomMap } from './RoomMap';
import { AppsPanel } from './AppsPanel';
import { EqPanel } from './EqPanel';
import { CalibrationCard } from './CalibrationCard';
import { Updater } from './Updater';
import { SettingsPanel } from './SettingsPanel';
import { VirtualSpeakerCard } from './VirtualSpeakerCard';
import './style.css';

const MODES: { id: AudioMode; label: string; detail: string }[] = [
  { id: 'gaming', label: 'Gaming', detail: '20 ms buffer. Lowest delay; warns when an output exceeds 60 ms estimated latency.' },
  { id: 'balanced', label: 'Balanced', detail: '40 ms buffer. Conservative shared buffering for steady playback.' },
  { id: 'synchronized', label: 'Synchronized', detail: '60 ms buffer. Delays faster outputs to the slowest Windows-reported latency.' },
];

const PRESETS: { id: SoundPreset; label: string; detail: string }[] = [
  { id: 'stereo', label: 'Stereo', detail: 'Every output plays full stereo.' },
  { id: 'front_rear', label: 'Front + Rear', detail: 'Front outputs at full level; outputs placed at the rear play 3 dB quieter.' },
  { id: 'left_right_split', label: 'Left / Right pair', detail: 'Two outputs act as one stereo pair: one plays left, the other right.' },
  { id: 'party_mono', label: 'Party mono', detail: 'Every output plays the same mono mix, so nobody misses a channel.' },
];

const POSITIONS: { id: SpeakerPosition; label: string }[] = [
  { id: 'unassigned', label: 'Not set' },
  { id: 'front_left', label: 'Front left' }, { id: 'front', label: 'Front' }, { id: 'front_right', label: 'Front right' },
  { id: 'left', label: 'Left side' }, { id: 'right', label: 'Right side' },
  { id: 'rear_left', label: 'Rear left' }, { id: 'rear', label: 'Rear' }, { id: 'rear_right', label: 'Rear right' },
];

const ROLES: { id: ChannelRole; label: string }[] = [
  { id: 'stereo', label: 'Stereo' }, { id: 'swapped', label: 'Stereo (L/R swapped)' },
  { id: 'left_only', label: 'Left channel only' }, { id: 'right_only', label: 'Right channel only' },
  { id: 'mono', label: 'Mono mix' },
];

const STATE_LABEL: Record<LifecycleState, string> = {
  disabled: 'Off', starting: 'Starting', running: 'Playing', disconnected: 'Disconnected', retrying: 'Retrying', failed: 'Failed',
};

function sourceValue(source: CaptureSource): string {
  if (source === 'DefaultRender') return 'default';
  if (source === 'SystemExcludingSelf') return 'system_excluding_self';
  if ('Endpoint' in source) return `endpoint:${source.Endpoint}`;
  return 'apps';
}

type Tab = 'routing' | 'speakers' | 'apps' | 'eq' | 'settings';
const TABS: { id: Tab; label: string; title: string; subtitle: string; icon: React.ReactNode }[] = [
  { id: 'routing', label: 'Routing', title: 'Audio routing', subtitle: 'Capture Windows audio once and play it on several outputs at the same time.', icon: <Activity size={17} /> },
  { id: 'speakers', label: 'Speaker setup', title: 'Speaker setup', subtitle: 'Place your speakers, choose a sound system, and calibrate with a microphone.', icon: <Speaker size={17} /> },
  { id: 'apps', label: 'Apps', title: 'App routing', subtitle: 'Send only the apps you choose to your outputs.', icon: <AppWindow size={17} /> },
  { id: 'eq', label: 'Equalizer', title: 'Equalizer', subtitle: 'Bass, mid, treble, low cut and room correction for each speaker.', icon: <SlidersHorizontal size={17} /> },
  { id: 'settings', label: 'Settings', title: 'Settings', subtitle: 'Start with Windows, restore your setup, and the tray.', icon: <Settings size={17} /> },
];
function savedTab(): Tab {
  try { const t = localStorage.getItem('syncsound.tab'); if (TABS.some(x => x.id === t)) return t as Tab; } catch { /* storage unavailable */ }
  return 'routing';
}

function captureText(state: CaptureState, endpoints: AudioEndpoint[]): { text: string; tone: 'ok' | 'warn' | 'bad' | 'idle' } {
  const name = (id: string) => endpoints.find(e => e.id === id)?.name ?? 'selected output';
  if (state === 'Stopped') return { text: 'Capture stopped', tone: 'idle' };
  if ('Capturing' in state) {
    const t = state.Capturing;
    if (t === 'SystemExcludingSelf') return { text: 'Capturing all apps except SyncSound', tone: 'ok' };
    if ('App' in t) return { text: 'Capturing the selected apps (see the Apps tab)', tone: 'ok' };
    return { text: `Capturing ${name(t.Endpoint)}`, tone: 'ok' };
  }
  if ('Starting' in state) return { text: 'Starting capture…', tone: 'idle' };
  if ('Blocked' in state) return { text: `Capture blocked — ${state.Blocked.reason}. Use “All apps except SyncSound” or pick a source that is not an enabled output.`, tone: 'bad' };
  if ('Unavailable' in state) return { text: 'Capture source unavailable — it is unplugged or disabled. SyncSound will not switch to another device on its own.', tone: 'warn' };
  return { text: `Capture interrupted, retrying — ${state.Recoverable.reason}`, tone: 'warn' };
}

/** Slider that keeps local state while dragging so polling never fights the pointer. */
function GainSlider({ value, disabled, onCommit }: { value: number; disabled: boolean; onCommit: (v: number) => void }) {
  const [local, setLocal] = useState(value);
  const [dragging, setDragging] = useState(false);
  useEffect(() => { if (!dragging) setLocal(value); }, [value, dragging]);
  return <input type="range" min={0} max={100} value={Math.round(local * 100)} disabled={disabled}
    aria-label="Output gain"
    onPointerDown={() => setDragging(true)}
    onPointerUp={() => setDragging(false)}
    onChange={e => { const v = Number(e.target.value) / 100; setLocal(v); onCommit(v); }} />;
}

function DelayInput({ value, onCommit }: { value: number; onCommit: (v: number) => void }) {
  const [text, setText] = useState(String(value));
  const [focused, setFocused] = useState(false);
  useEffect(() => { if (!focused) setText(String(value)); }, [value, focused]);
  const commit = () => {
    const n = Math.round(Number(text));
    if (Number.isFinite(n) && n >= 0 && n <= MAX_DELAY_MS && n !== value) onCommit(n);
    else setText(String(value));
  };
  return <input className="delay" type="number" min={0} max={MAX_DELAY_MS} step={1} value={text} aria-label="Delay in milliseconds"
    onFocus={() => setFocused(true)} onBlur={() => { setFocused(false); commit(); }}
    onChange={e => setText(e.target.value)} onKeyDown={e => { if (e.key === 'Enter') (e.target as HTMLInputElement).blur(); }} />;
}

// Mirrors syncsound-engine profile::validate_name.
const PROFILE_NAME = /^[\p{L}\p{N} _\-+&(),]{1,64}$/u;
function profileNameProblem(name: string): string | null {
  if (!name) return null;
  if (name.trim() !== name) return 'Remove spaces at the start or end.';
  if ([...name].length > 64) return 'Use at most 64 characters.';
  if (!PROFILE_NAME.test(name)) return 'Use letters, digits, spaces and - _ + & ( ) , only.';
  return null;
}

/** Number input that commits on blur/Enter, keeps its text while focused, and can be blank. */
function NumberField({ value, min, max, step, allowEmpty, label, width, onCommit }: {
  value: number | null; min: number; max: number; step: number; allowEmpty?: boolean; label: string; width?: number;
  onCommit: (v: number | null) => void;
}) {
  const show = (v: number | null) => (v == null ? '' : String(v));
  const [text, setText] = useState(show(value));
  const [focused, setFocused] = useState(false);
  useEffect(() => { if (!focused) setText(show(value)); }, [value, focused]);
  const commit = () => {
    if (text.trim() === '' && allowEmpty) { if (value != null) onCommit(null); return; }
    const n = Number(text);
    if (Number.isFinite(n) && n >= min && n <= max) { if (n !== value) onCommit(n); }
    else setText(show(value));
  };
  return <input className="num-field" style={width ? { width } : undefined} type="number" min={min} max={max} step={step}
    value={text} aria-label={label} placeholder={allowEmpty ? '-' : undefined}
    onFocus={() => setFocused(true)} onBlur={() => { setFocused(false); commit(); }}
    onChange={e => setText(e.target.value)} onKeyDown={e => { if (e.key === 'Enter') (e.target as HTMLInputElement).blur(); }} />;
}

function fmt(n: number, digits = 0) { return Number.isFinite(n) ? n.toFixed(digits) : '—'; }

function OutputCard({ endpoint, status, running, enabledCount, onAction }: {
  endpoint: AudioEndpoint; status?: OutputStatus; running: boolean; enabledCount: number;
  onAction: (fn: () => Promise<unknown>) => void;
}) {
  const config = status?.config;
  const enabled = config?.enabled ?? false;
  const state: LifecycleState = status?.state ?? 'disabled';
  const unavailable = endpoint.state !== 'Active';
  const atLimit = !enabled && enabledCount >= MAX_ENABLED_OUTPUTS;
  const showMetrics = running && enabled && state === 'running';
  const bluetooth = endpoint.bus === 'BTHENUM' || endpoint.bus === 'BTHLEDEVICE';
  return <article className={`device ${enabled ? 'on' : ''} ${unavailable ? 'unavailable' : ''}`}>
    <div className="device-top">
      <div className="device-icon"><Volume2 size={21} /></div>
      <div className="device-info">
        <h3>{endpoint.name}{endpoint.is_default && <span className="tag">Windows default</span>}{bluetooth && <span className="tag bt">Bluetooth</span>}</h3>
        <p>
          {[endpoint.connection_type, endpoint.mix_format && `${endpoint.mix_format.channels} ch · ${endpoint.mix_format.sample_rate.toLocaleString()} Hz`,
            endpoint.bits_per_sample && `${endpoint.bits_per_sample}-bit mix`, unavailable && 'Unplugged or disabled'].filter(Boolean).join(' · ') || 'Format unknown'}
        </p>
      </div>
      {enabled && running && <span className={`state ${state}`}>{STATE_LABEL[state]}</span>}
      <label className="switch" title={atLimit ? `At most ${MAX_ENABLED_OUTPUTS} outputs` : undefined}>
        <input type="checkbox" checked={enabled} disabled={atLimit || (unavailable && !enabled)}
          onChange={() => onAction(() => enabled ? api.disable(endpoint.id) : api.enable(endpoint.id))} />
        <span aria-hidden="true" />
        <span className="sr">Route to this output</span>
      </label>
    </div>
    {enabled && config && <div className="controls">
      <button className={`icon ${config.muted ? 'muted' : ''}`} aria-pressed={config.muted} title={config.muted ? 'Unmute' : 'Mute'}
        onClick={() => onAction(() => api.mute(endpoint.id, !config.muted))}>{config.muted ? <VolumeX size={16} /> : <Volume2 size={16} />}</button>
      <label className="gain">Gain <GainSlider value={config.gain} disabled={config.muted}
        onCommit={v => onAction(() => api.volume(endpoint.id, v))} /><span className="num">{Math.round(config.gain * 100)}%</span></label>
      <label className="delay-label">Delay <DelayInput value={config.delay_ms} onCommit={v => onAction(() => api.delay(endpoint.id, v))} /> ms</label>
    </div>}
    {enabled && config && <div className="placement">
      <label>Position
        <select value={config.position} aria-label="Speaker position"
          onChange={e => onAction(() => api.placement(endpoint.id, e.target.value as SpeakerPosition, config.distance_m))}>
          {POSITIONS.map(p => <option key={p.id} value={p.id}>{p.label}</option>)}
        </select>
      </label>
      <label>Distance
        <NumberField value={config.distance_m} min={MIN_DISTANCE_M} max={MAX_DISTANCE_M} step={0.1} allowEmpty width={70}
          label="Distance from your seat in metres" onCommit={v => onAction(() => api.placement(endpoint.id, config.position, v))} /> m
      </label>
      <label>Plays
        <select value={config.role} aria-label="Channels this output plays"
          onChange={e => onAction(() => api.role(endpoint.id, e.target.value as ChannelRole))}>
          {ROLES.map(r => <option key={r.id} value={r.id}>{r.label}</option>)}
        </select>
      </label>
      <label>Level
        <NumberField value={config.trim_db} min={MIN_TRIM_DB} max={0} step={0.5} width={64}
          label="Level trim in dB" onCommit={v => onAction(() => api.trim(endpoint.id, v ?? 0))} /> dB
      </label>
      {status && (status.placement_delay_ms > 0.05 || status.placement_level < 0.999) && <span className="placement-note">
        Placement: +{fmt(status.placement_delay_ms, 1)} ms, {Math.round(status.placement_level * 100)}% level
      </span>}
    </div>}
    {showMetrics && status && <dl className="metrics">
      <div><dt>Buffered</dt><dd>{fmt(status.buffered_ms)} ms</dd></div>
      <div><dt>Est. latency</dt><dd>{fmt(status.estimated_latency_ms)} ms</dd></div>
      <div><dt>Auto-align</dt><dd>{fmt(status.align_ms)} ms</dd></div>
      <div><dt>Drift correction</dt><dd>{fmt(status.metrics.correction_ppm, 1)} ppm</dd></div>
      <div><dt>Underruns</dt><dd>{status.metrics.underruns}</dd></div>
      <div><dt>Overruns</dt><dd>{status.metrics.overruns}</dd></div>
    </dl>}
    {enabled && bluetooth && <p className="warn-line"><AlertTriangle size={14} /> Windows does not report Bluetooth codec and radio delay, so this output usually plays 100–250 ms later than shown. Add delay to your other outputs until they match.</p>}
    {status?.warning && <p className="warn-line"><AlertTriangle size={14} /> {status.warning}</p>}
    {enabled && running && status?.last_error && state !== 'running' && <p className="err-line">
      {status.last_error}{status.retry_in_ms != null && ` · retrying in ${Math.ceil(status.retry_in_ms / 1000)} s`}
      {state === 'failed' && ' · toggle the output off and on to retry'}
    </p>}
  </article>;
}

function App() {
  const [snapshot, setSnapshot] = useState<EngineSnapshot | null>(null);
  const [error, setError] = useState('');
  const [busy, setBusy] = useState(false);
  const [showUnavailable, setShowUnavailable] = useState(false);
  const [profiles, setProfiles] = useState<string[]>([]);
  const [profileName, setProfileName] = useState('');
  const [selectedProfile, setSelectedProfile] = useState('');
  const [report, setReport] = useState<ProfileLoadReport | null>(null);
  const [rebindTargets, setRebindTargets] = useState<Record<string, string>>({});
  const [info, setInfo] = useState('');
  const [profileMessage, setProfileMessage] = useState<{ tone: 'ok' | 'bad'; text: string } | null>(null);
  const [tab, setTabState] = useState<Tab>(savedTab);
  const setTab = (t: Tab) => { setTabState(t); try { localStorage.setItem('syncsound.tab', t); } catch { /* ignore */ } };
  const current = TABS.find(t => t.id === tab)!;

  const poll = useCallback(async () => {
    try { setSnapshot(await api.status()); } catch (e) { setError(errorMessage(e)); }
  }, []);
  const act = useCallback((fn: () => Promise<unknown>) => {
    setError('');
    fn().then(poll, e => { setError(errorMessage(e)); void poll(); });
  }, [poll]);
  const refreshProfiles = useCallback(() => { api.listProfiles().then(setProfiles, e => setError(errorMessage(e))); }, []);

  useEffect(() => {
    void (async () => { try { await api.devices(); } catch (e) { setError(errorMessage(e)); } await poll(); })();
    refreshProfiles();
    const timer = window.setInterval(poll, 500);
    return () => window.clearInterval(timer);
  }, [poll, refreshProfiles]);

  const endpoints = snapshot?.endpoints ?? [];
  const statusById = useMemo(() => new Map((snapshot?.outputs ?? []).map(o => [o.endpoint_id, o])), [snapshot]);
  const active = endpoints.filter(e => e.state === 'Active');
  const unavailable = endpoints.filter(e => e.state !== 'Active');
  const configuredMissing = (snapshot?.outputs ?? []).filter(o => !endpoints.some(e => e.id === o.endpoint_id));
  const enabledCount = (snapshot?.outputs ?? []).filter(o => o.config.enabled).length;
  const running = snapshot?.running ?? false;
  const capture = snapshot ? captureText(snapshot.capture_state, endpoints) : null;

  async function refresh() {
    setBusy(true); setError('');
    try { await api.devices(); await poll(); } catch (e) { setError(errorMessage(e)); } finally { setBusy(false); }
  }
  /** Profile actions report success and failure next to the controls, not at the page top. */
  function profileAct(fn: () => Promise<string>) {
    setProfileMessage(null);
    fn().then(
      text => { setProfileMessage({ tone: 'ok', text }); void poll(); },
      e => { setProfileMessage({ tone: 'bad', text: errorMessage(e) }); void poll(); },
    );
  }
  const nameProblem = profileNameProblem(profileName);

  function changeSource(value: string) {
    const [kind, ...rest] = value.split(':');
    if (kind === 'apps') { setTab('apps'); return; }
    act(() => api.selectSource(kind as SourceKind, kind === 'endpoint' ? rest.join(':') : undefined));
  }

  return <div className="shell">
    <aside>
      <div className="brand"><Headphones size={26} /> <span>SyncSound</span></div>
      <p className="version">Developer Preview</p>
      <nav aria-label="Sections">
        {TABS.map(t => <button key={t.id} className={`nav ${tab === t.id ? 'active' : ''}`} aria-current={tab === t.id ? 'page' : undefined}
          onClick={() => setTab(t.id)}>{t.icon} {t.label}</button>)}
      </nav>
      <div className="sidebar-bottom"><Updater /></div>
    </aside>
    <main>
      <header>
        <div><h1>{current.title}</h1><p>{current.subtitle}</p></div>
        <div className="header-actions">
          <button onClick={refresh} disabled={busy}><RefreshCw size={16} /> Refresh devices</button>
          <button className={running ? 'stop' : 'start'} onClick={() => act(running ? api.stop : api.start)}>
            {running ? <><Power size={16} /> Stop routing</> : <><Play size={16} /> Start routing</>}
          </button>
        </div>
      </header>

      {tab === 'routing' && <section className="notice"><AlertTriangle size={19} /><div>
        <strong>Developer preview.</strong>
        <p>Latency figures on the cards are estimates from Windows-reported buffer sizes and do not include Bluetooth or HDMI transport delay or speaker distance. For real alignment, use Calibrate on the Speaker setup tab with a microphone at your seat.</p>
      </div></section>}

      {error && <div className="error" role="alert">{error}</div>}
      {info && <div className="info" role="status" onClick={() => setInfo('')}>{info}</div>}

      {tab === 'routing' && <>
      <VirtualSpeakerCard onChange={() => void poll()} running={running} />
      <section className="panel grid2">
        <div>
          <h2>Source</h2>
          <select value={snapshot ? sourceValue(snapshot.source) : 'default'} onChange={e => changeSource(e.target.value)} aria-label="Capture source">
            <option value="default">Windows default output (follows changes)</option>
            <option value="system_excluding_self">All apps except SyncSound (system-wide)</option>
            {active.map(e => <option key={e.id} value={`endpoint:${e.id}`}>Pinned: {e.name}</option>)}
            <option value="apps">Selected apps only (choose on the Apps tab)</option>
          </select>
          <p className="hint">A device used as the source cannot also be an output — that would feed SyncSound back into itself. To include your default speakers, choose “All apps except SyncSound”; note those speakers already play the original audio directly.</p>
          {capture && <p className={`capture ${capture.tone}`}><span className="dot" />{capture.text}</p>}
        </div>
        <div>
          <h2>Mode</h2>
          <div className="segmented" role="radiogroup" aria-label="Audio mode">
            {MODES.map(m => <button key={m.id} role="radio" aria-checked={snapshot?.mode === m.id}
              className={snapshot?.mode === m.id ? 'selected' : ''} onClick={() => act(() => api.mode(m.id))}>{m.label}</button>)}
          </div>
          <p className="hint">{MODES.find(m => m.id === snapshot?.mode)?.detail}</p>
        </div>
      </section>

      </>}
      {tab === 'speakers' && <>
      <section className="panel">
        <div className="setup-head">
          <h2>Speakers and sound system</h2>
          <label className="check">
            <input type="checkbox" checked={snapshot?.distance_compensation ?? true}
              onChange={e => act(() => api.distanceCompensation(e.target.checked))} />
            Compensate for distance
          </label>
        </div>
        <div className="presets" role="group" aria-label="Sound system preset">
          {PRESETS.map(p => <button key={p.id} title={p.detail} disabled={enabledCount === 0}
            onClick={() => act(() => api.preset(p.id))}>{p.label}</button>)}
        </div>
        <p className="hint">Presets set what each output plays and its level, and keep your gain, delay and positions. Set each output's position and distance from your seat on its card below. With distance compensation on, nearer speakers are delayed (2.9 ms per metre) and turned down so sound from every speaker reaches you together and evenly. If you already tuned delays with a microphone at your seat, the distance is already in that measurement, so turn compensation off.</p>
        {(snapshot?.outputs ?? []).some(o => o.config.enabled) && <ul className="layout">
          {(snapshot?.outputs ?? []).filter(o => o.config.enabled).map(o => <li key={o.endpoint_id}>
            <strong>{POSITIONS.find(p => p.id === o.config.position)?.label}</strong> {o.name}
            <span>{ROLES.find(r => r.id === o.config.role)?.label}{o.config.distance_m != null && `, ${o.config.distance_m} m`}{o.config.trim_db !== 0 && `, ${o.config.trim_db} dB`}</span>
          </li>)}
        </ul>}
        <RoomMap snapshot={snapshot} act={act} />
      </section>
      <CalibrationCard snapshot={snapshot} onDone={() => void poll()} />
      </>}
      {tab === 'routing' && <>

      <div className="section-head"><h2>Outputs</h2><span>{enabledCount} / {MAX_ENABLED_OUTPUTS} enabled · {active.length} available</span></div>
      <div className="devices">
        {active.map(e => <OutputCard key={e.id} endpoint={e} status={statusById.get(e.id)} running={running} enabledCount={enabledCount} onAction={act} />)}
        {configuredMissing.map(o => <article key={o.endpoint_id} className="device unavailable"><div className="device-top"><div className="device-info"><h3>{o.name}</h3><p>Device not found. It will not be replaced automatically.</p></div></div></article>)}
      </div>
      {!active.length && <div className="empty">No active output devices. Connect an audio device and select Refresh devices.</div>}
      {unavailable.length > 0 && <>
        <button className="link" onClick={() => setShowUnavailable(v => !v)}>{showUnavailable ? 'Hide' : 'Show'} {unavailable.length} unplugged or disabled devices</button>
        {showUnavailable && <div className="devices">
          {unavailable.map(e => <OutputCard key={e.id} endpoint={e} status={statusById.get(e.id)} running={running} enabledCount={enabledCount} onAction={act} />)}
        </div>}
      </>}

      <section className="panel grid2">
        <div>
          <h2>Profiles</h2>
          <div className="row">
            <input value={profileName} onChange={e => setProfileName(e.target.value)} placeholder="Profile name" maxLength={64}
              aria-label="Profile name" aria-invalid={nameProblem != null} aria-describedby="profile-name-hint" />
            <button disabled={!profileName || nameProblem != null} onClick={() => profileAct(async () => {
              await api.saveProfile(profileName); refreshProfiles(); setSelectedProfile(profileName);
              return `Saved profile “${profileName}”.`;
            })}><Save size={15} /> Save</button>
          </div>
          {nameProblem && <p id="profile-name-hint" className="field-error">{nameProblem}</p>}
          <div className="row">
            <select value={selectedProfile} onChange={e => setSelectedProfile(e.target.value)} aria-label="Saved profiles">
              <option value="">{profiles.length ? 'Choose a profile…' : 'No saved profiles'}</option>
              {profiles.map(p => <option key={p} value={p}>{p}</option>)}
            </select>
            <button disabled={!selectedProfile} onClick={() => profileAct(async () => {
              const r = await api.loadProfile(selectedProfile); setReport(r); setRebindTargets({});
              return `Loaded “${r.name}”: ${r.applied_outputs} output(s) applied${r.missing.length ? `, ${r.missing.length} missing` : ''}.`;
            })}><FolderOpen size={15} /> Load</button>
          </div>
          {profileMessage && <p className={profileMessage.tone === 'ok' ? 'field-ok' : 'field-error'} role="status">{profileMessage.text}</p>}
          {report && (report.missing.length > 0 || report.missing_source) && <div className="missing">
            <strong>Missing devices in “{report.name}”</strong>
            <p className="hint">These saved devices are not connected. SyncSound never substitutes a device with a similar name — choose a replacement explicitly, or reconnect the device and load again.</p>
            {report.missing_source && <p className="hint">The pinned capture source is missing; the current source was kept.</p>}
            {report.missing.map(m => <div className="row" key={m.endpoint_id}>
              <span className="grow">{m.name_hint || 'Unnamed device'}</span>
              <select value={rebindTargets[m.endpoint_id] ?? ''} aria-label={`Replacement for ${m.name_hint}`}
                onChange={e => setRebindTargets(t => ({ ...t, [m.endpoint_id]: e.target.value }))}>
                <option value="">Replace with…</option>
                {active.map(e => <option key={e.id} value={e.id}>{e.name}</option>)}
              </select>
              <button disabled={!rebindTargets[m.endpoint_id]} onClick={() => profileAct(async () => {
                await api.rebind(report.name, m.endpoint_id, rebindTargets[m.endpoint_id]);
                const r = await api.loadProfile(report.name); setReport(r);
                return `Rebound and reloaded “${r.name}”.`;
              })}>Rebind</button>
            </div>)}
          </div>}
        </div>
        <div>
          <h2>Diagnostics</h2>
          <p className="hint">Exports engine state, queue metrics and recent errors as JSON. Device IDs are replaced with anonymous hashes.</p>
          <button onClick={() => act(async () => { const path = await api.exportDiagnostics(); setInfo(`Diagnostics saved to ${path}`); })}><Download size={15} /> Export diagnostics</button>
          {(snapshot?.recent_errors.length ?? 0) > 0 && <details className="errors">
            <summary>Recent engine messages ({snapshot!.recent_errors.length})</summary>
            <ul>{snapshot!.recent_errors.slice(-10).reverse().map((e, i) => <li key={i}><code>{e.scope}</code> {e.message}</li>)}</ul>
          </details>}
        </div>
      </section>

      </>}
      {tab === 'apps' && <AppsPanel snapshot={snapshot} act={act} />}
      {tab === 'eq' && <EqPanel snapshot={snapshot} act={act} />}
      {tab === 'settings' && <SettingsPanel />}

      <footer>
        <span>Capture format {snapshot ? `${snapshot.capture_format.sample_rate.toLocaleString()} Hz · ${snapshot.capture_format.channels} ch float` : '—'}</span>
        <span>Card latencies are estimates · use Calibrate for measured alignment</span>
      </footer>
    </main>
  </div>;
}

createRoot(document.getElementById('root')!).render(<React.StrictMode><App /></React.StrictMode>);

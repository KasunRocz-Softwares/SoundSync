import React, { useCallback, useEffect, useMemo, useState } from 'react';
import { createRoot } from 'react-dom/client';
import {
  Activity, AlertTriangle, Download, FolderOpen, Headphones, Play, Power, RefreshCw, Save, Volume2, VolumeX,
} from 'lucide-react';
import { api, errorMessage, type SourceKind } from './api';
import {
  MAX_DELAY_MS, MAX_ENABLED_OUTPUTS,
  type AudioEndpoint, type AudioMode, type CaptureSource, type CaptureState, type EngineSnapshot,
  type LifecycleState, type OutputStatus, type ProfileLoadReport,
} from './types';
import './style.css';

const MODES: { id: AudioMode; label: string; detail: string }[] = [
  { id: 'gaming', label: 'Gaming', detail: '20 ms buffer. Lowest delay; warns when an output exceeds 60 ms estimated latency.' },
  { id: 'balanced', label: 'Balanced', detail: '40 ms buffer. Conservative shared buffering for steady playback.' },
  { id: 'synchronized', label: 'Synchronized', detail: '60 ms buffer. Delays faster outputs to the slowest Windows-reported latency.' },
];

const STATE_LABEL: Record<LifecycleState, string> = {
  disabled: 'Off', starting: 'Starting', running: 'Playing', disconnected: 'Disconnected', retrying: 'Retrying', failed: 'Failed',
};

function sourceValue(source: CaptureSource): string {
  if (source === 'DefaultRender') return 'default';
  if (source === 'SystemExcludingSelf') return 'system_excluding_self';
  return `endpoint:${source.Endpoint}`;
}

function captureText(state: CaptureState, endpoints: AudioEndpoint[]): { text: string; tone: 'ok' | 'warn' | 'bad' | 'idle' } {
  const name = (id: string) => endpoints.find(e => e.id === id)?.name ?? 'selected output';
  if (state === 'Stopped') return { text: 'Capture stopped', tone: 'idle' };
  if ('Capturing' in state) {
    const t = state.Capturing;
    return { text: t === 'SystemExcludingSelf' ? 'Capturing all apps except SyncSound' : `Capturing ${name(t.Endpoint)}`, tone: 'ok' };
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
  function changeSource(value: string) {
    const [kind, ...rest] = value.split(':');
    act(() => api.selectSource(kind as SourceKind, kind === 'endpoint' ? rest.join(':') : undefined));
  }

  return <div className="shell">
    <aside>
      <div className="brand"><Headphones size={26} /> <span>SyncSound</span></div>
      <p className="version">v0.1.0 · Developer Preview</p>
      <div className="nav active"><Activity size={17} /> Audio routing</div>
      <div className="sidebar-bottom">Windows 11 · WASAPI shared mode<br />Profiles stored in your app data</div>
    </aside>
    <main>
      <header>
        <div><h1>Audio routing</h1><p>Capture Windows audio once and play it on several outputs at the same time.</p></div>
        <div className="header-actions">
          <button onClick={refresh} disabled={busy}><RefreshCw size={16} /> Refresh devices</button>
          <button className={running ? 'stop' : 'start'} onClick={() => act(running ? api.stop : api.start)}>
            {running ? <><Power size={16} /> Stop routing</> : <><Play size={16} /> Start routing</>}
          </button>
        </div>
      </header>

      <section className="notice"><AlertTriangle size={19} /><div>
        <strong>Developer preview.</strong>
        <p>Latency and alignment figures are estimates from Windows-reported buffer sizes. They do not include Bluetooth or HDMI transport delay or speaker distance, and acoustic (microphone) calibration is not part of v0.1. Use the per-output delay to fine-tune by ear.</p>
      </div></section>

      {error && <div className="error" role="alert">{error}</div>}
      {info && <div className="info" role="status" onClick={() => setInfo('')}>{info}</div>}

      <section className="panel grid2">
        <div>
          <h2>Source</h2>
          <select value={snapshot ? sourceValue(snapshot.source) : 'default'} onChange={e => changeSource(e.target.value)} aria-label="Capture source">
            <option value="default">Windows default output (follows changes)</option>
            <option value="system_excluding_self">All apps except SyncSound (system-wide)</option>
            {active.map(e => <option key={e.id} value={`endpoint:${e.id}`}>Pinned: {e.name}</option>)}
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
            <input value={profileName} onChange={e => setProfileName(e.target.value)} placeholder="Profile name" maxLength={64} aria-label="Profile name" />
            <button disabled={!profileName.trim()} onClick={() => act(async () => {
              await api.saveProfile(profileName.trim()); setInfo(`Saved profile “${profileName.trim()}”.`); refreshProfiles();
            })}><Save size={15} /> Save</button>
          </div>
          <div className="row">
            <select value={selectedProfile} onChange={e => setSelectedProfile(e.target.value)} aria-label="Saved profiles">
              <option value="">{profiles.length ? 'Choose a profile…' : 'No saved profiles'}</option>
              {profiles.map(p => <option key={p} value={p}>{p}</option>)}
            </select>
            <button disabled={!selectedProfile} onClick={() => act(async () => {
              const r = await api.loadProfile(selectedProfile); setReport(r); setRebindTargets({});
              setInfo(`Loaded “${r.name}”: ${r.applied_outputs} output(s) applied${r.missing.length ? `, ${r.missing.length} missing` : ''}.`);
            })}><FolderOpen size={15} /> Load</button>
          </div>
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
              <button disabled={!rebindTargets[m.endpoint_id]} onClick={() => act(async () => {
                await api.rebind(report.name, m.endpoint_id, rebindTargets[m.endpoint_id]);
                const r = await api.loadProfile(report.name); setReport(r);
                setInfo(`Rebound and reloaded “${r.name}”.`);
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

      <footer>
        <span>Capture format {snapshot ? `${snapshot.capture_format.sample_rate.toLocaleString()} Hz · ${snapshot.capture_format.channels} ch float` : '—'}</span>
        <span>Estimated values only · no acoustic calibration in v0.1</span>
      </footer>
    </main>
  </div>;
}

createRoot(document.getElementById('root')!).render(<React.StrictMode><App /></React.StrictMode>);

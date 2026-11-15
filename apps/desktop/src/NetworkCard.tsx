import React, { useEffect, useRef, useState } from 'react';
import { AlertTriangle, Check, Copy, MonitorSmartphone, Pencil, Trash2, Wifi, X } from 'lucide-react';
import { api, errorMessage } from './api';
import { NET_PREFIX, type AudioEndpoint, type EngineSnapshot, type OutputStatus } from './types';

const FIREWALL_SEEN = 'syncsound.netFirewallNote';
const MAX_NAME = 40;

function firewallSeen(): boolean {
  try { return localStorage.getItem(FIREWALL_SEEN) === '1'; } catch { return true; }
}
function markFirewallSeen() {
  try { localStorage.setItem(FIREWALL_SEEN, '1'); } catch { /* storage unavailable */ }
}

/** What a joined device is doing right now, for its row. */
function deviceState(endpoint: AudioEndpoint, status: OutputStatus | undefined, running: boolean, serverOn: boolean):
  { text: string; tone: 'ok' | 'idle' | 'warn' } {
  if (!serverOn) return { text: 'Network speakers off', tone: 'idle' };
  if (endpoint.state !== 'Active') return { text: 'Not connected', tone: 'idle' };
  if (!status?.config.enabled) return { text: 'Connected, output off', tone: 'idle' };
  if (!running) return { text: 'Connected, routing stopped', tone: 'idle' };
  if (status.state === 'running') return { text: 'Playing', tone: 'ok' };
  if (status.state === 'failed') return { text: 'Failed', tone: 'warn' };
  return { text: 'Starting', tone: 'warn' };
}

function JoinedRow({ endpoint, status, running, serverOn, onDone }: {
  endpoint: AudioEndpoint; status?: OutputStatus; running: boolean; serverOn: boolean;
  onDone: (fn: () => Promise<unknown>) => Promise<boolean>;
}) {
  const key = endpoint.id.slice(NET_PREFIX.length);
  const [editing, setEditing] = useState(false);
  const [name, setName] = useState(endpoint.name);
  const [confirmForget, setConfirmForget] = useState(false);
  const input = useRef<HTMLInputElement>(null);
  useEffect(() => { if (editing) input.current?.select(); }, [editing]);
  useEffect(() => {
    if (!confirmForget) return;
    const t = window.setTimeout(() => setConfirmForget(false), 4000);
    return () => window.clearTimeout(t);
  }, [confirmForget]);

  const state = deviceState(endpoint, status, running, serverOn);
  const latency = state.text === 'Playing' && status && status.device_latency_ms > 0
    ? `${Math.round(status.device_latency_ms)} ms` : null;
  const trimmed = name.trim();
  const save = async () => {
    if (!trimmed || trimmed === endpoint.name) { setEditing(false); setName(endpoint.name); return; }
    if (await onDone(() => api.renameNetworkDevice(key, trimmed))) setEditing(false);
  };

  return <li className="net-row">
    <span className={`net-dot ${state.tone}`} aria-hidden="true" />
    {editing
      ? <form className="net-rename" onSubmit={e => { e.preventDefault(); void save(); }}>
        <input ref={input} value={name} maxLength={MAX_NAME} aria-label="Device name"
          onChange={e => setName(e.target.value)}
          onKeyDown={e => { if (e.key === 'Escape') { setEditing(false); setName(endpoint.name); } }} />
        <button type="submit" className="icon-btn" title="Save name" aria-label="Save name" disabled={!trimmed}><Check size={15} /></button>
        <button type="button" className="icon-btn" title="Cancel" aria-label="Cancel rename"
          onClick={() => { setEditing(false); setName(endpoint.name); }}><X size={15} /></button>
      </form>
      : <span className="net-name">{endpoint.name}</span>}
    {!editing && <span className="net-meta">
      {latency && <><span className="net-latency">{latency}</span><span className="net-sep" aria-hidden="true">·</span></>}
      <span className={`net-state ${state.tone}`}>{state.text}</span>
    </span>}
    {!editing && <span className="net-actions">
      <button className="ghost" onClick={() => { setName(endpoint.name); setEditing(true); }}><Pencil size={14} /> Rename</button>
      <button className={`ghost ${confirmForget ? 'danger' : ''}`}
        onClick={() => confirmForget ? void onDone(() => api.forgetNetworkDevice(key)) : setConfirmForget(true)}>
        <Trash2 size={14} /> {confirmForget ? 'Forget it?' : 'Forget'}
      </button>
    </span>}
  </li>;
}

/**
 * Network speakers: phones, TVs and PCs on the home Wi-Fi open a page served
 * by SyncSound and become normal outputs. Joining by QR code is immediate;
 * typing the address waits for Allow here.
 */
export function NetworkCard({ snapshot, onChange }: { snapshot: EngineSnapshot | null; onChange: () => void }) {
  const network = snapshot?.network;
  const on = network?.enabled ?? false;
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState('');
  const [copied, setCopied] = useState(false);
  const [firewallNote, setFirewallNote] = useState(false);

  useEffect(() => {
    if (!copied) return;
    const t = window.setTimeout(() => setCopied(false), 1600);
    return () => window.clearTimeout(t);
  }, [copied]);

  /** Runs one network action and refreshes. Not added to undo history. */
  const run = async (fn: () => Promise<unknown>): Promise<boolean> => {
    setError('');
    try { await fn(); return true; } catch (e) { setError(errorMessage(e)); return false; } finally { onChange(); }
  };

  const toggle = async (enable: boolean) => {
    setBusy(true);
    const ok = await run(async () => {
      const status = await api.setNetwork(enable);
      if (status.error) throw new Error(status.error);
    });
    if (ok && enable && !firewallSeen()) { setFirewallNote(true); markFirewallSeen(); }
    if (!enable) setFirewallNote(false);
    setBusy(false);
  };

  const copy = async () => {
    if (!network?.url) return;
    try { await navigator.clipboard.writeText(network.url); setCopied(true); } catch { setError('Could not copy the address. Select it and press Ctrl+C.'); }
  };

  const joined = (snapshot?.endpoints ?? []).filter(e => e.id.startsWith(NET_PREFIX));
  const statusById = new Map((snapshot?.outputs ?? []).map(o => [o.endpoint_id, o]));
  const pending = network?.pending ?? [];
  const running = snapshot?.running ?? false;

  return <section className="panel net">
    <div className="setup-head">
      <h2><Wifi size={16} /> Network speakers</h2>
      <label className="switch">
        <input type="checkbox" checked={on} disabled={busy || !snapshot} onChange={e => void toggle(e.target.checked)} />
        <span aria-hidden="true" /><span className="sr">Play on network speakers</span>
      </label>
    </div>
    <p className="net-lede">Play on phones, TVs and other PCs on your Wi-Fi.</p>

    {!on && <p className="hint">Turn this on and SyncSound shows a QR code. Any phone, tablet, smart TV or PC browser on the same Wi-Fi can open it and play in sync with your other speakers. Nothing outside your home network can connect.</p>}

    {firewallNote && <div className="net-note" role="status">
      <AlertTriangle size={15} />
      <span>Windows may ask about the firewall. Choose <strong>Allow</strong> on Private networks.</span>
      <button className="icon-btn" title="Dismiss" aria-label="Dismiss" onClick={() => setFirewallNote(false)}><X size={14} /></button>
    </div>}

    {on && network?.url && <div className="net-join">
      {/* The SVG is generated by our own engine (qrcode crate) from the server's
          own address and token; it never contains outside text. */}
      {network.qr_svg && <div className="net-qr" role="img" aria-label="QR code with the address of this PC"
        dangerouslySetInnerHTML={{ __html: network.qr_svg }} />}
      <div className="net-address">
        <p>Scan with your phone, or open</p>
        <div className="net-url-row">
          <code className="net-url">{network.url}</code>
          <button className="ghost" onClick={() => void copy()} aria-live="polite">
            {copied ? <><Check size={14} /> Copied</> : <><Copy size={14} /> Copy</>}
          </button>
        </div>
        <p>on a TV or PC on the same Wi-Fi.</p>
        <p className="hint">A scanned code joins right away. A typed address waits for you to allow it here. Keep the page open with the screen on.</p>
      </div>
    </div>}

    {on && pending.length > 0 && <div className="net-group">
      <h3>Waiting to join</h3>
      <ul className="net-list">
        {pending.map(p => <li key={p.key} className="net-row waiting">
          <span className="net-dot warn" aria-hidden="true" />
          <span className="net-name">{p.name}</span>
          <span className="net-meta"><span className="net-address-ip">{p.address}</span></span>
          <span className="net-actions">
            <button className="allow" onClick={() => void run(() => api.allowNetworkDevice(p.key))}>Allow</button>
            <button className="ghost" onClick={() => void run(() => api.denyNetworkDevice(p.key))}>Deny</button>
          </span>
        </li>)}
      </ul>
    </div>}

    {joined.length > 0 && <div className="net-group">
      <h3>Joined</h3>
      <ul className="net-list">
        {joined.map(e => <JoinedRow key={e.id} endpoint={e} status={statusById.get(e.id)} running={running} serverOn={on} onDone={run} />)}
      </ul>
    </div>}

    {on && joined.length === 0 && pending.length === 0 && <p className="net-empty"><MonitorSmartphone size={15} /> No devices yet. Scan the code to add the first one.</p>}

    {(error || network?.error) && <p className="field-error net-error" role="alert">{error || network?.error}</p>}
  </section>;
}

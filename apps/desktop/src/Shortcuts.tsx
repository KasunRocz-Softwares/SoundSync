import React, { useEffect, useState } from 'react';
import { Keyboard, RotateCcw, X } from 'lucide-react';
import { api, errorMessage } from './api';
import type { HotkeyStatus, OutputStatus } from './types';

const GLOBAL: { action: string; label: string }[] = [
  { action: 'toggle_routing', label: 'Start or stop routing' },
  { action: 'recall_a', label: 'Switch to A' },
  { action: 'recall_b', label: 'Switch to B' },
  { action: 'next_mode', label: 'Next listening mode' },
  { action: 'undo', label: 'Undo' },
  { action: 'redo', label: 'Redo' },
];

const PER_OUTPUT: { prefix: string; label: string }[] = [
  { prefix: 'mute', label: 'Mute' },
  { prefix: 'louder', label: 'Louder' },
  { prefix: 'quieter', label: 'Quieter' },
];

/** Turns a key press into the backend's format ("Ctrl+Alt+S"), or null while
 *  only modifiers are held. */
function comboFrom(e: KeyboardEvent): string | null {
  const code = e.code;
  if (/^(Control|Alt|Shift|Meta|OS)(Left|Right)?$/.test(code) || !code) return null;
  const key = code.startsWith('Key') ? code.slice(3)
    : code.startsWith('Digit') ? code.slice(5)
    : code;
  const mods = [e.ctrlKey && 'Ctrl', e.altKey && 'Alt', e.shiftKey && 'Shift', e.metaKey && 'Super'].filter(Boolean);
  return [...mods, key].join('+');
}

/** Pretty form for display: Super → Win, ArrowUp → ↑. */
function pretty(keys: string): string {
  return keys.split('+').map(k => ({
    Super: 'Win', ArrowUp: '↑', ArrowDown: '↓', ArrowLeft: '←', ArrowRight: '→', Equal: '=', Minus: '−',
  } as Record<string, string>)[k] ?? k).join(' + ');
}

function KeyField({ keys, refused, onSet, label }: {
  keys: string; refused: boolean; label: string; onSet: (keys: string) => Promise<void>;
}) {
  const [recording, setRecording] = useState(false);
  useEffect(() => {
    if (!recording) return;
    const down = (e: KeyboardEvent) => {
      e.preventDefault(); e.stopPropagation();
      if (e.code === 'Escape' && !e.ctrlKey && !e.altKey) { setRecording(false); return; }
      const combo = comboFrom(e);
      if (!combo) return;
      setRecording(false);
      void onSet(combo);
    };
    window.addEventListener('keydown', down, true);
    return () => window.removeEventListener('keydown', down, true);
  }, [recording, onSet]);
  return <span className="key-field">
    <button className={`key-record ${recording ? 'recording' : ''} ${refused ? 'taken' : ''}`}
      aria-label={`${label}: ${keys ? pretty(keys) : 'no shortcut'}. Press to record a new one`}
      onClick={() => setRecording(r => !r)} onBlur={() => setRecording(false)}>
      {recording ? 'Press keys… (Esc cancels)' : keys ? pretty(keys) : 'Not set'}
    </button>
    {keys && !recording && <button className="icon-btn" aria-label={`Clear the ${label} shortcut`} title="Clear"
      onClick={() => void onSet('')}><X size={14} /></button>}
  </span>;
}

/** Editable global shortcuts: app-wide actions plus mute / louder / quieter
 *  for each speaker. Shortcuts work anywhere in Windows, even from the tray. */
export function ShortcutsCard({ enabled, onToggle, outputs }: {
  enabled: boolean | null; onToggle: (on: boolean) => void; outputs: OutputStatus[];
}) {
  const [status, setStatus] = useState<HotkeyStatus | null>(null);
  const [message, setMessage] = useState<{ tone: 'ok' | 'bad'; text: string } | null>(null);
  useEffect(() => { api.hotkeyStatus().then(setStatus, () => setStatus(null)); }, [enabled]);

  const keysOf = (action: string) => status?.bindings.find(b => b.action === action)?.keys ?? '';
  const set = (action: string, label: string) => async (keys: string) => {
    try {
      const next = await api.setHotkey(action, keys);
      setStatus(next);
      const taken = keys && next.refused.includes(action);
      setMessage(taken
        ? { tone: 'bad', text: `${pretty(keys)} is held by another app, so it won't work for ${label}. Try another.` }
        : { tone: 'ok', text: keys ? `${label}: ${pretty(keys)}` : `${label}: shortcut cleared` });
    } catch (e) {
      setMessage({ tone: 'bad', text: errorMessage(e) });
    }
  };
  const reset = () => api.resetHotkeys().then(
    s => { setStatus(s); setMessage({ tone: 'ok', text: 'Shortcuts reset to the defaults.' }); },
    e => setMessage({ tone: 'bad', text: errorMessage(e) }),
  );
  const refused = (action: string) => status?.refused.includes(action) ?? false;
  const speakers = outputs.filter(o => o.config.enabled || PER_OUTPUT.some(p => keysOf(`${p.prefix}:${o.endpoint_id}`)));

  return <section className="panel settings">
    <div className="setup-head">
      <h2><Keyboard size={16} /> Shortcuts</h2>
      <button className="link small" onClick={reset} disabled={!status}><RotateCcw size={14} /> Reset</button>
    </div>
    <label className="setting">
      <input type="checkbox" checked={enabled ?? true} disabled={enabled === null}
        onChange={e => onToggle(e.target.checked)} />
      <span><strong>Global hotkeys</strong><small>Work anywhere in Windows, even when SyncSound is in the tray. Click a shortcut, then press the keys you want. Use Ctrl, Alt or Win with a key, or an F-key on its own.</small></span>
    </label>
    {status && enabled !== false && <>
      <table className="shortcut-table">
        <tbody>
          {GLOBAL.map(g => <tr key={g.action}>
            <th scope="row">{g.label}</th>
            <td><KeyField keys={keysOf(g.action)} refused={refused(g.action)} label={g.label} onSet={set(g.action, g.label)} /></td>
          </tr>)}
        </tbody>
      </table>
      <h3 className="shortcut-sub">Each speaker</h3>
      {speakers.length === 0 && <p className="hint top">Turn on an output on the Routing tab to give it mute and volume shortcuts.</p>}
      {speakers.length > 0 && <table className="shortcut-table per-output">
        <thead><tr><th scope="col">Speaker</th>{PER_OUTPUT.map(p => <th key={p.prefix} scope="col">{p.label}</th>)}</tr></thead>
        <tbody>
          {speakers.map(o => <tr key={o.endpoint_id}>
            <th scope="row">{o.name}</th>
            {PER_OUTPUT.map(p => {
              const action = `${p.prefix}:${o.endpoint_id}`;
              const label = `${p.label} ${o.name}`;
              return <td key={p.prefix} data-label={p.label}>
                <KeyField keys={keysOf(action)} refused={refused(action)} label={label} onSet={set(action, label)} />
              </td>;
            })}
          </tr>)}
        </tbody>
      </table>}
      <p className="hint">Louder and Quieter change that speaker's volume by 2 dB. Anything a shortcut changes can be undone like any other change. Inside SyncSound, Ctrl+Z and Ctrl+Y always undo and redo.</p>
    </>}
    {message && <p className={message.tone === 'ok' ? 'field-ok' : 'field-error'} role="status">{message.text}</p>}
  </section>;
}

import React, { useCallback, useEffect, useState } from 'react';
import { AppWindow, RefreshCw } from 'lucide-react';
import { api, errorMessage } from './api';
import type { AudioApp, EngineSnapshot } from './types';

const MAX_APPS = 8;

/** Pick which applications SyncSound routes, instead of all Windows audio. */
export function AppsPanel({ snapshot, act }: { snapshot: EngineSnapshot | null; act: (fn: () => Promise<unknown>) => void }) {
  const [apps, setApps] = useState<AudioApp[]>([]);
  const [error, setError] = useState('');
  const source = snapshot?.source;
  const routed = source && typeof source === 'object' && 'Apps' in source ? source.Apps : [];
  const [chosen, setChosen] = useState<string[]>(routed);
  useEffect(() => { setChosen(routed); }, [routed.join('|')]);

  const load = useCallback(() => {
    api.apps().then(list => { setApps(list); setError(''); }, e => setError(errorMessage(e)));
  }, []);
  useEffect(() => {
    load();
    const timer = window.setInterval(load, 3000);
    return () => window.clearInterval(timer);
  }, [load]);

  // Keep chosen apps visible even when they are not playing right now.
  const known = new Set(apps.map(a => a.exe.toLowerCase()));
  const offline = chosen.filter(exe => !known.has(exe.toLowerCase()));
  const toggle = (exe: string) => setChosen(c => c.some(x => x.toLowerCase() === exe.toLowerCase())
    ? c.filter(x => x.toLowerCase() !== exe.toLowerCase())
    : c.length >= MAX_APPS ? c : [...c, exe]);
  const isChosen = (exe: string) => chosen.some(x => x.toLowerCase() === exe.toLowerCase());
  const status = (exe: string) => snapshot?.app_sources.find(a => a.exe.toLowerCase() === exe.toLowerCase());
  const changed = chosen.slice().sort().join('|').toLowerCase() !== routed.slice().sort().join('|').toLowerCase();

  return <section className="panel">
    <div className="setup-head">
      <h2>Route only these apps</h2>
      <button onClick={load}><RefreshCw size={15} /> Refresh</button>
    </div>
    <p className="hint">Pick the apps whose sound SyncSound sends to your outputs. Other apps keep playing only on their normal device. Windows still plays a routed app on its own output too; to hear it only through SyncSound, set that app's output to a device you don't listen to in Windows Settings → System → Sound → Volume mixer.</p>
    {error && <p className="field-error">{error}</p>}
    <ul className="app-list">
      {apps.map(app => {
        const s = status(app.exe);
        return <li key={app.exe} className={isChosen(app.exe) ? 'on' : ''}>
          <label>
            <input type="checkbox" checked={isChosen(app.exe)} onChange={() => toggle(app.exe)}
              disabled={!isChosen(app.exe) && chosen.length >= MAX_APPS} />
            <AppWindow size={18} />
            <span className="app-name">{app.name}<small>{app.exe}</small></span>
          </label>
          <span className={`pill ${app.active ? 'live' : ''}`}>{app.active ? 'Playing' : 'Idle'}</span>
          {s && <span className={`pill ${s.capturing ? 'live' : 'wait'}`}>{s.capturing ? 'Routed' : 'Waiting'}</span>}
        </li>;
      })}
      {offline.map(exe => <li key={exe} className="on">
        <label><input type="checkbox" checked onChange={() => toggle(exe)} /><AppWindow size={18} />
          <span className="app-name">{exe.replace(/\.exe$/i, '')}<small>{exe}</small></span></label>
        <span className="pill wait">Not running — will connect when it plays</span>
      </li>)}
      {!apps.length && !offline.length && <li className="empty-row">No apps are playing sound right now. Start some audio and select Refresh.</li>}
    </ul>
    <div className="row actions-row">
      <button className="start" disabled={!chosen.length || !changed} onClick={() => act(() => api.selectApps(chosen))}>
        Route {chosen.length || ''} selected app{chosen.length === 1 ? '' : 's'}
      </button>
      <button disabled={!routed.length} onClick={() => act(() => api.selectSource('default'))}>Back to all Windows audio</button>
      <span className="hint inline">{routed.length ? `Now routing: ${routed.map(e => e.replace(/\.exe$/i, '')).join(', ')}` : 'Now routing: all audio from the source on the Routing tab'}</span>
    </div>
  </section>;
}

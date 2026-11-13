import React, { useCallback, useEffect, useState } from 'react';
import { Download, RefreshCw } from 'lucide-react';
import { getVersion } from '@tauri-apps/api/app';
import { check, type Update } from '@tauri-apps/plugin-updater';
import { relaunch } from '@tauri-apps/plugin-process';

const AUTO_KEY = 'syncsound.autoUpdate';
const readAuto = () => { try { return localStorage.getItem(AUTO_KEY) !== 'off'; } catch { return true; } };

type State =
  | { kind: 'idle' } | { kind: 'checking' } | { kind: 'current' }
  | { kind: 'available'; update: Update } | { kind: 'installing'; percent: number | null }
  | { kind: 'error'; message: string };

/** Checks GitHub Releases for a newer signed build and installs it on request. */
export function Updater() {
  const [version, setVersion] = useState('');
  const [auto, setAuto] = useState(readAuto);
  const [state, setState] = useState<State>({ kind: 'idle' });

  const runCheck = useCallback(async (quiet: boolean) => {
    setState({ kind: 'checking' });
    try {
      const update = await check();
      setState(update ? { kind: 'available', update } : quiet ? { kind: 'idle' } : { kind: 'current' });
    } catch (e) {
      setState(quiet ? { kind: 'idle' } : { kind: 'error', message: String(e) });
    }
  }, []);

  useEffect(() => {
    getVersion().then(setVersion, () => {});
    if (readAuto()) void runCheck(true);
  }, [runCheck]);

  const toggleAuto = (on: boolean) => {
    setAuto(on);
    try { localStorage.setItem(AUTO_KEY, on ? 'on' : 'off'); } catch { /* ignore */ }
  };

  const install = async (update: Update) => {
    let total = 0, done = 0;
    setState({ kind: 'installing', percent: null });
    try {
      await update.downloadAndInstall(event => {
        if (event.event === 'Started') total = event.data.contentLength ?? 0;
        if (event.event === 'Progress') {
          done += event.data.chunkLength;
          setState({ kind: 'installing', percent: total ? Math.round((done / total) * 100) : null });
        }
      });
      await relaunch();
    } catch (e) {
      setState({ kind: 'error', message: String(e) });
    }
  };

  return <div className="updater">
    {state.kind === 'available' && <div className="update-card" role="status">
      <strong>SyncSound {state.update.version} is available</strong>
      {state.update.body && <p>{state.update.body.split('\n')[0]}</p>}
      <button className="start" onClick={() => install(state.update)}><Download size={14} /> Install and restart</button>
    </div>}
    {state.kind === 'installing' && <p className="update-note">Installing update{state.percent != null ? ` ${state.percent}%` : '…'}</p>}
    {state.kind === 'current' && <p className="update-note">You have the latest version.</p>}
    {state.kind === 'error' && <p className="update-note bad">Update check failed. {state.message}</p>}
    <button className="link small" onClick={() => runCheck(false)} disabled={state.kind === 'checking' || state.kind === 'installing'}>
      <RefreshCw size={12} /> {state.kind === 'checking' ? 'Checking…' : 'Check for updates'}
    </button>
    <label className="check tiny">
      <input type="checkbox" checked={auto} onChange={e => toggleAuto(e.target.checked)} /> Check automatically
    </label>
    <span className="version-line">Version {version || '…'}</span>
  </div>;
}

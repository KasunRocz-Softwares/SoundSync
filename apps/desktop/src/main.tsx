import React, { useEffect, useState } from 'react';
import { createRoot } from 'react-dom/client';
import { invoke } from '@tauri-apps/api/core';
import { Headphones, RefreshCw, Square, Volume2, AlertTriangle, Activity } from 'lucide-react';
import './style.css';

type Device = { id: string; name: string; is_default: boolean; channels: number; sample_rate: number };
function App() {
  const [devices, setDevices] = useState<Device[]>([]);
  const [playing, setPlaying] = useState<string[]>([]);
  const [error, setError] = useState('');
  const [busy, setBusy] = useState(false);
  async function refresh() {
    setBusy(true); setError('');
    try { setDevices(await invoke<Device[]>('get_audio_devices')); }
    catch (e) { setError(String(e)); }
    finally { setBusy(false); }
  }
  useEffect(() => { void refresh(); }, []);
  async function toggle(id: string) {
    setError('');
    try {
      if (playing.includes(id)) { await invoke('stop_device', { id }); setPlaying(p => p.filter(x => x !== id)); }
      else { await invoke('start_test_tone', { id }); setPlaying(p => [...p, id]); }
    } catch (e) { setError(String(e)); }
  }
  async function volume(id: string, value: number) {
    try { await invoke('set_device_volume', { id, volume: value / 100 }); }
    catch (e) { setError(String(e)); }
  }
  return <div className="shell">
    <aside><div className="brand"><Headphones size={27}/> <span>SyncSound</span></div><p className="version">v0.1.0 · Developer Preview</p><div className="nav active"><Activity size={17}/> Audio Devices</div><div className="sidebar-bottom">Windows 11 · Local audio engine</div></aside>
    <main><header><div><h1>Audio Device Manager</h1><p>Discover outputs and test simultaneous playback.</p></div><button onClick={refresh} disabled={busy}><RefreshCw size={16}/> Refresh</button></header>
      <section className="notice"><AlertTriangle size={19}/><div><strong>Diagnostic prototype — not yet a system-audio mirroring app.</strong><p>Each enabled output plays a quiet 440 Hz test tone. WASAPI loopback, accurate cross-device synchronization, Bluetooth calibration and speaker placement are planned for subsequent milestones. Lower your speaker volume before testing.</p></div></section>
      <div className="section-head"><h2>Available outputs</h2><span>{devices.length} detected · {playing.length} active</span></div>
      {error && <div className="error" role="alert">{error}</div>}
      <div className="devices">{devices.map(d => <article className="device" key={d.id}><div className="device-top"><div className="device-icon"><Volume2 size={23}/></div><div className="device-info"><h3>{d.name} {d.is_default && <span className="tag">Default</span>}</h3><p>{d.channels} channels · {d.sample_rate.toLocaleString()} Hz</p></div><button className={playing.includes(d.id)?'stop':'start'} onClick={() => toggle(d.id)}>{playing.includes(d.id)?<><Square size={14}/> Stop tone</>:<>▶ Test tone</>}</button></div><label className="vol">Output gain <input type="range" min="0" max="50" defaultValue="12" disabled={!playing.includes(d.id)} onChange={e => void volume(d.id, Number(e.target.value))}/></label></article>)}</div>
      {!devices.length && <div className="empty">No output devices found. Connect an audio device and select Refresh.</div>}
      <footer><button className="stop-all" onClick={async()=>{await invoke('stop_all');setPlaying([])}} disabled={!playing.length}>Stop all test tones</button><span>System audio capture: Not implemented · Synchronization: Not implemented</span></footer>
    </main>
  </div>;
}

createRoot(document.getElementById('root')!).render(<React.StrictMode><App/></React.StrictMode>);

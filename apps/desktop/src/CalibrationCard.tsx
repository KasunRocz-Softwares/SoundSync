import React, { useEffect, useState } from 'react';
import { AudioWaveform, Mic, Wand2 } from 'lucide-react';
import { api, errorMessage } from './api';
import { ROOM_BANDS, type CalibrationReport, type EngineSnapshot, type Microphone, type RoomReport } from './types';

const bandLabel = (f: number) => (f >= 1000 ? `${f / 1000}k` : `${f}`);

/** Measures every playing output with a microphone at the seat and sets delays. */
export function CalibrationCard({ snapshot, onDone }: { snapshot: EngineSnapshot | null; onDone: () => void }) {
  const [mics, setMics] = useState<Microphone[]>([]);
  const [mic, setMic] = useState('');
  const [busy, setBusy] = useState(false);
  const [report, setReport] = useState<CalibrationReport | null>(null);
  const [room, setRoom] = useState<RoomReport | null>(null);
  const [error, setError] = useState('');

  useEffect(() => {
    api.mics().then(list => {
      setMics(list);
      setMic(current => current || (list.find(m => m.is_default) ?? list[0])?.id || '');
    }, e => setError(errorMessage(e)));
  }, []);

  const playing = (snapshot?.outputs ?? []).filter(o => o.config.enabled && !o.config.muted && o.state === 'running');
  const progress = snapshot?.calibration;
  const ready = snapshot?.running && playing.length >= 2 && mic && !busy;
  const seconds = playing.length * 6 + 3;

  const roomReady = snapshot?.running && playing.length >= 1 && mic && !busy;
  const measureRoom = () => {
    setBusy(true); setError(''); setRoom(null);
    api.measureRoom(mic).then(setRoom, e => setError(errorMessage(e)))
      .finally(() => { setBusy(false); onDone(); });
  };

  const run = () => {
    setBusy(true); setError(''); setReport(null);
    api.calibrate(mic).then(r => { setReport(r); }, e => setError(errorMessage(e)))
      .finally(() => { setBusy(false); onDone(); });
  };

  return <section className="panel">
    <div className="setup-head">
      <h2><Wand2 size={16} /> Calibrate with a microphone</h2>
    </div>
    <p className="hint">Put the microphone where you sit and keep the room quiet. SyncSound plays short clicks through each speaker in turn, at each speaker's own gain (about {seconds} s), listens, and sets every speaker's delay so they reach you at the same moment. Distance and Bluetooth delay are included automatically, so turn off "Compensate for distance" afterwards.</p>
    <div className="row">
      <Mic size={16} />
      <select value={mic} onChange={e => setMic(e.target.value)} aria-label="Microphone" disabled={busy}>
        {mics.map(m => <option key={m.id} value={m.id}>{m.name}{m.is_default ? ' (default)' : ''}</option>)}
        {!mics.length && <option value="">No microphone found</option>}
      </select>
      <button className="start" disabled={!ready} onClick={run}>{busy && !progress?.measuring_room ? 'Calibrating…' : 'Calibrate delays'}</button>
      <button disabled={!roomReady} onClick={measureRoom} title="Measures each speaker's frequency response and sets a room-correction EQ">
        <AudioWaveform size={15} /> {busy && progress?.measuring_room ? 'Measuring…' : 'Measure room'}
      </button>
    </div>
    <p className="hint">Measure room plays a short rising tone through each speaker (about {playing.length * 5.5 + 3} s), compares what the microphone hears with what was played, and stores a gentle 10-band correction in each speaker's Equalizer. It mostly cuts room booms, never boosts a speaker's missing deep bass, and includes the microphone's own colouration, so treat it as a starting point.</p>
    {!snapshot?.running && <p className="field-error">Start routing first.</p>}
    {snapshot?.running && playing.length < 2 && <p className="field-error">Needs at least two playing, unmuted outputs.</p>}
    {progress && <div className="calib-progress" role="status">
      <div className="bar"><span style={{ width: `${Math.min(100, (progress.elapsed_s / progress.total_s) * 100)}%` }} /></div>
      <p>{progress.playing ? `${progress.measuring_room ? 'Sweeping' : 'Clicking'}: ${progress.playing}` : 'Listening…'} · {Math.max(0, Math.ceil(progress.total_s - progress.elapsed_s))} s left</p>
    </div>}
    {error && <p className="field-error">{error}</p>}
    {room && <div className="calib-report">
      <p className={room.outputs.every(o => o.applied) ? 'field-ok' : 'field-error'}>{room.message}</p>
      <table className="room-table">
        <thead><tr><th>Output</th>{ROOM_BANDS.map(b => <th key={b}>{bandLabel(b)}</th>)}</tr></thead>
        <tbody>{room.outputs.map(o => <React.Fragment key={o.endpoint_id}>
          <tr><td>{o.name}<small>{o.result ? `measured · SNR ${o.result.snr_db.toFixed(0)} dB` : 'not heard'}</small></td>
            {ROOM_BANDS.map((b, i) => { const r = o.result?.response_db[i]; return <td key={b} className="resp">{r == null ? '—' : `${r > 0 ? '+' : ''}${r.toFixed(0)}`}</td>; })}</tr>
          <tr className="corr"><td>{o.applied ? 'correction applied' : 'not applied'}</td>
            {ROOM_BANDS.map((b, i) => { const c = o.result?.correction_db[i] ?? 0; return <td key={b} className={c < 0 ? 'cut' : c > 0 ? 'boost' : ''}>{c ? `${c > 0 ? '+' : ''}${c.toFixed(1)}` : '·'}</td>; })}</tr>
        </React.Fragment>)}</tbody>
      </table>
      <p className="hint">Top row: what the microphone heard (dB vs the mids). Bottom row: the correction now in each speaker's Equalizer — turn it off there anytime.</p>
    </div>}
    {report && <div className="calib-report">
      <p className={report.applied ? 'field-ok' : 'field-error'}>{report.message}</p>
      <table>
        <thead><tr><th>Output</th><th>Heard</th><th>Arrives</th><th>Delay</th></tr></thead>
        <tbody>{report.outputs.map(o => <tr key={o.endpoint_id}>
          <td>{o.name}</td>
          <td>{o.detected ? `Yes (${o.snr.toFixed(0)}×)` : `Too quiet (${o.snr.toFixed(1)}×)`}</td>
          <td>{o.relative_ms != null ? `${o.relative_ms >= 0 ? '+' : ''}${o.relative_ms.toFixed(0)} ms` : '—'}</td>
          <td>{o.old_delay_ms} → <strong>{o.new_delay_ms} ms</strong></td>
        </tr>)}</tbody>
      </table>
    </div>}
  </section>;
}

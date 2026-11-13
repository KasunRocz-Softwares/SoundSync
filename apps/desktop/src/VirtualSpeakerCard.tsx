import React, { useCallback, useEffect, useState } from 'react';
import { Cable } from 'lucide-react';
import { api, errorMessage } from './api';
import type { VirtualSpeakerStatus } from './types';

/**
 * Virtual speaker via VB-CABLE: Windows plays into the silent cable, SyncSound
 * captures it, so every real speaker — including the old default — can be
 * delayed and aligned.
 */
export function VirtualSpeakerCard({ onChange, running }: { onChange: () => void; running: boolean }) {
  const [status, setStatus] = useState<VirtualSpeakerStatus | null>(null);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState('');

  const load = useCallback(() => {
    api.virtualSpeaker().then(setStatus, e => setError(errorMessage(e)));
  }, []);
  useEffect(() => { load(); }, [load]);

  const toggle = (on: boolean) => {
    setBusy(true); setError('');
    (on ? api.enableVirtualSpeaker() : api.disableVirtualSpeaker())
      .then(setStatus, e => setError(errorMessage(e)))
      .finally(() => { setBusy(false); onChange(); });
  };

  return <section className="panel vspk">
    <div className="setup-head">
      <h2><Cable size={16} /> Virtual speaker</h2>
      {status?.installed && <label className="switch">
        <input type="checkbox" checked={status.active} disabled={busy} onChange={e => toggle(e.target.checked)} />
        <span aria-hidden="true" /><span className="sr">Use the SyncSound virtual speaker</span>
      </label>}
    </div>
    {!status && !error && <p className="hint">Checking…</p>}
    {status && !status.installed && <>
      <p className="hint">Windows plays its default device directly, so SyncSound can't delay it. With the free <strong>VB-CABLE</strong> virtual audio cable as the Windows default, every app plays into the cable, SyncSound captures it, and <em>all</em> your real speakers — including the one that used to be the default — are played and aligned by SyncSound.</p>
      <ol className="steps">
        <li>Download VB-CABLE from <code>vb-audio.com/Cable</code> and run its installer as administrator.</li>
        <li>Restart Windows if the installer asks.</li>
        <li>Come back here and turn this switch on. SyncSound makes the cable your default and restores your old default when you turn it off or quit.</li>
      </ol>
      <button onClick={load}>I've installed it — check again</button>
    </>}
    {status?.installed && !status.active && <p className="hint">{status.cable_name} is installed. Turn this on to send all Windows audio through SyncSound. Then turn on your real speakers below (for example your old default) and calibrate them.</p>}
    {status?.active && <p className="capture ok"><span className="dot" />
      Windows plays into {status.cable_name}; SyncSound captures it.{status.previous_default_name ? ` Your previous default (${status.previous_default_name}) comes back when you turn this off or quit.` : ''}
      {!status.default_is_cable && ' Windows default was changed elsewhere — toggle off and on to switch back.'}
    </p>}
    {status?.active && !running && <p className="field-error" role="alert">
      Routing is stopped, so you hear nothing: Windows is playing into the silent cable. Press Start routing, or turn the virtual speaker off to go back to {status.previous_default_name ?? 'your previous speakers'}.
    </p>}
    {error && <p className="field-error">{error}</p>}
  </section>;
}

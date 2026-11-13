import React, { useEffect, useState } from 'react';
import { disable as disableAutostart, enable as enableAutostart, isEnabled as autostartEnabled } from '@tauri-apps/plugin-autostart';
import { api, errorMessage } from './api';
import type { AppSettings } from './types';

/** Start with Windows, restore the last setup, tray behaviour. */
export function SettingsPanel() {
  const [settings, setSettings] = useState<AppSettings | null>(null);
  const [autostart, setAutostart] = useState(false);
  const [message, setMessage] = useState<{ tone: 'ok' | 'bad'; text: string } | null>(null);

  useEffect(() => {
    api.settings().then(s => setSettings(s), e => setMessage({ tone: 'bad', text: errorMessage(e) }));
    autostartEnabled().then(setAutostart, () => setAutostart(false));
  }, []);

  const update = (patch: Partial<AppSettings>) => {
    if (!settings) return;
    const next = { ...settings, ...patch };
    setSettings(next);
    api.saveSettings(next).then(
      s => { setSettings(s); setMessage({ tone: 'ok', text: 'Saved.' }); },
      e => setMessage({ tone: 'bad', text: errorMessage(e) }),
    );
  };
  const toggleAutostart = (on: boolean) => {
    (on ? enableAutostart() : disableAutostart()).then(
      () => { setAutostart(on); setMessage({ tone: 'ok', text: on ? 'SyncSound will start with Windows, in the tray.' : 'SyncSound will not start with Windows.' }); },
      e => setMessage({ tone: 'bad', text: errorMessage(e) }),
    );
  };

  return <section className="panel settings">
    <h2>Startup and tray</h2>
    <label className="setting">
      <input type="checkbox" checked={autostart} onChange={e => toggleAutostart(e.target.checked)} />
      <span><strong>Start with Windows</strong><small>Opens quietly in the system tray when you sign in.</small></span>
    </label>
    <label className="setting">
      <input type="checkbox" checked={settings?.restore_last_session ?? true} disabled={!settings}
        onChange={e => update({ restore_last_session: e.target.checked })} />
      <span><strong>Restore my last setup</strong><small>Outputs, delays, EQ, map and source come back exactly as you left them. Saved every 30 s and when you quit.</small></span>
    </label>
    <label className="setting">
      <input type="checkbox" checked={settings?.auto_start_routing ?? false} disabled={!settings}
        onChange={e => update({ auto_start_routing: e.target.checked })} />
      <span><strong>Start routing automatically</strong><small>Press Start for you after the setup is restored.</small></span>
    </label>
    <label className="setting">
      <input type="checkbox" checked={settings?.close_to_tray ?? true} disabled={!settings}
        onChange={e => update({ close_to_tray: e.target.checked })} />
      <span><strong>Keep running in the tray when I close the window</strong><small>Sound keeps playing. Right-click the tray icon to switch profiles, start/stop or quit.</small></span>
    </label>
    {message && <p className={message.tone === 'ok' ? 'field-ok' : 'field-error'} role="status">{message.text}</p>}
  </section>;
}

import React, { useEffect, useRef, useState } from 'react';
import { disable as disableAutostart, enable as enableAutostart, isEnabled as autostartEnabled } from '@tauri-apps/plugin-autostart';
import { FileDown, FileUp } from 'lucide-react';
import { api, errorMessage } from './api';
import type { AppSettings, OutputStatus } from './types';
import { ShortcutsCard } from './Shortcuts';

type Message = { tone: 'ok' | 'bad'; text: string } | null;

/** Start with Windows, restore the last setup, tray behaviour, hotkeys and settings files. */
export function SettingsPanel({ onChange, outputs }: { onChange: () => void; outputs: OutputStatus[] }) {
  const [settings, setSettings] = useState<AppSettings | null>(null);
  const [autostart, setAutostart] = useState(false);
  const [message, setMessage] = useState<Message>(null);
  const [fileMessage, setFileMessage] = useState<Message>(null);
  const fileInput = useRef<HTMLInputElement>(null);

  useEffect(() => {
    api.settings().then(s => setSettings(s), e => setMessage({ tone: 'bad', text: errorMessage(e) }));
    autostartEnabled().then(setAutostart, () => setAutostart(false));
  }, []);

  const update = (patch: Partial<AppSettings>) => {
    if (!settings) return;
    const next = { ...settings, ...patch };
    setSettings(next);
    api.saveSettings(next).then(
      s => {
        setSettings(s); setMessage({ tone: 'ok', text: 'Saved.' });
      },
      e => setMessage({ tone: 'bad', text: errorMessage(e) }),
    );
  };
  const toggleAutostart = (on: boolean) => {
    (on ? enableAutostart() : disableAutostart()).then(
      () => { setAutostart(on); setMessage({ tone: 'ok', text: on ? 'SyncSound will start with Windows, in the tray.' : 'SyncSound will not start with Windows.' }); },
      e => setMessage({ tone: 'bad', text: errorMessage(e) }),
    );
  };

  const exportSettings = () => api.exportSettings().then(
    path => setFileMessage({ tone: 'ok', text: `Saved to ${path}` }),
    e => setFileMessage({ tone: 'bad', text: errorMessage(e) }),
  );
  const importFile = (file: File | undefined) => {
    if (!file) return;
    file.text()
      .then(text => api.importSettings(text))
      .then(r => {
        const missing = r.missing.length ? ` ${r.missing.length} device(s) from the file aren't connected here.` : '';
        setFileMessage({ tone: 'ok', text: `Loaded ${file.name}: ${r.applied_outputs} output(s).${missing} Press Ctrl+Z to go back.` });
        onChange();
      }, e => setFileMessage({ tone: 'bad', text: errorMessage(e) }))
      .finally(() => { if (fileInput.current) fileInput.current.value = ''; });
  };

  return <>
    <section className="panel settings">
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
    </section>

    <ShortcutsCard enabled={settings ? settings.hotkeys : null} onToggle={on => update({ hotkeys: on })} outputs={outputs} />

    <section className="panel settings">
      <h2>Settings file</h2>
      <p className="hint top">Export everything (outputs, delays, EQ, room map, sound settings, app modes) to a file you can back up or load on another PC. Devices are matched by their exact Windows ID; anything not connected is listed.</p>
      <div className="file-actions">
        <button onClick={exportSettings}><FileDown size={15} /> Export settings</button>
        <button onClick={() => fileInput.current?.click()}><FileUp size={15} /> Import settings…</button>
        <input ref={fileInput} type="file" accept=".json,application/json" hidden onChange={e => importFile(e.target.files?.[0])} />
      </div>
      {fileMessage && <p className={fileMessage.tone === 'ok' ? 'field-ok' : 'field-error'} role="status">{fileMessage.text}</p>}
    </section>
  </>;
}

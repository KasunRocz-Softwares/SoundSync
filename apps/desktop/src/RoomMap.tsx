import React, { useEffect, useMemo, useRef, useState } from 'react';
import { RotateCcw, RotateCw, Sparkles } from 'lucide-react';
import { api } from './api';
import type { EngineSnapshot, OutputStatus, SourceLayout, SpatialConfig, StereoSpread } from './types';

/** Top-down room: screen at the top, you in the middle. 0° = front, + = clockwise (right). */
const SIZE = 640;
const CX = 320;
const CY = 330;
const PX_PER_M = 52;
const MAX_MAP_M = 5;
const TRAY_Y = 606;

const GUIDES: { label: string; name: string; az: number }[] = [
  { label: 'C', name: 'Center', az: 0 },
  { label: 'FL', name: 'Front left', az: -30 },
  { label: 'FR', name: 'Front right', az: 30 },
  { label: 'SL', name: 'Side left', az: -90 },
  { label: 'SR', name: 'Side right', az: 90 },
  { label: 'BL', name: 'Back left', az: -150 },
  { label: 'BR', name: 'Back right', az: 150 },
];
const SNAP_ANGLES = [...GUIDES.map(g => g.az), 180];

const VIRTUAL: Record<SourceLayout, { label: string; az: number }[]> = {
  stereo: [{ label: 'L', az: -30 }, { label: 'R', az: 30 }],
  upmix71: [
    { label: 'C', az: 0 }, { label: 'FL', az: -30 }, { label: 'FR', az: 30 }, { label: 'SL', az: -90 },
    { label: 'SR', az: 90 }, { label: 'BL', az: -150 }, { label: 'BR', az: 150 },
  ],
};

const SPREADS: { id: StereoSpread; label: string; detail: string }[] = [
  { id: 'point', label: 'Single box', detail: 'Both channels come from one spot' },
  { id: 'narrow', label: 'Small stereo', detail: 'Left/right ±15°' },
  { id: 'wide', label: 'Wide pair', detail: 'Left/right ±45°' },
  { id: 'headphones', label: 'Headphones', detail: 'Left/right ear at ±90°' },
];

const toXY = (az: number, m: number) => {
  const r = Math.min(m, MAX_MAP_M) * PX_PER_M;
  const rad = (az * Math.PI) / 180;
  return { x: CX + r * Math.sin(rad), y: CY - r * Math.cos(rad) };
};
const wrap = (a: number) => { const w = ((a + 180) % 360 + 360) % 360 - 180; return w === -180 ? 180 : w; };
const shortName = (name: string) => name.replace(/\s*\(.*\)\s*$/, '').slice(0, 18);

type Drag = { id: string; az: number; m: number };

export function RoomMap({ snapshot, act }: { snapshot: EngineSnapshot | null; act: (fn: () => Promise<unknown>) => void }) {
  const svg = useRef<SVGSVGElement>(null);
  const [drag, setDrag] = useState<Drag | null>(null);
  const [selected, setSelected] = useState<string | null>(null);
  const [snap, setSnap] = useState(true);
  const [now, setNow] = useState(() => performance.now());
  const spatial: SpatialConfig = snapshot?.spatial ?? { enabled: false, layout: 'stereo', rotation_s: null, clockwise: true };
  const [speed, setSpeed] = useState(spatial.rotation_s ?? 10);
  useEffect(() => { if (spatial.rotation_s != null) setSpeed(spatial.rotation_s); }, [spatial.rotation_s]);

  const outputs = useMemo(() => (snapshot?.outputs ?? []).filter(o => o.config.enabled), [snapshot]);
  const placed = outputs.filter(o => o.resolved_azimuth != null);
  const unplaced = outputs.filter(o => o.resolved_azimuth == null);
  const current = outputs.find(o => o.endpoint_id === selected) ?? null;

  // Animate the virtual field while 8D rotation is on.
  const rotating = spatial.enabled && spatial.rotation_s != null;
  useEffect(() => {
    if (!rotating) return;
    let frame = 0;
    const tick = () => { setNow(performance.now()); frame = requestAnimationFrame(tick); };
    frame = requestAnimationFrame(tick);
    return () => cancelAnimationFrame(frame);
  }, [rotating]);
  const rotation = rotating ? ((now / 1000) / (spatial.rotation_s ?? 10)) * 360 * (spatial.clockwise ? 1 : -1) : 0;

  const pointerToMap = (e: React.PointerEvent) => {
    const el = svg.current;
    if (!el) return null;
    const pt = el.createSVGPoint();
    pt.x = e.clientX; pt.y = e.clientY;
    const p = pt.matrixTransform(el.getScreenCTM()!.inverse());
    const dx = p.x - CX, dy = p.y - CY;
    let az = wrap((Math.atan2(dx, -dy) * 180) / Math.PI);
    if (snap) {
      const near = SNAP_ANGLES.find(a => Math.abs(wrap(az - a)) <= 10);
      if (near != null) az = near;
    }
    const m = Math.min(MAX_MAP_M, Math.max(0.2, Math.hypot(dx, dy) / PX_PER_M));
    return { az: Math.round(az), m: Math.round(m * 10) / 10 };
  };

  const startDrag = (o: OutputStatus, e: React.PointerEvent) => {
    e.preventDefault();
    svg.current?.setPointerCapture(e.pointerId);
    setSelected(o.endpoint_id);
    setDrag({ id: o.endpoint_id, az: o.resolved_azimuth ?? 0, m: o.config.distance_m ?? 2.5 });
  };
  const moveDrag = (e: React.PointerEvent) => {
    if (!drag) return;
    const p = pointerToMap(e);
    if (p) setDrag({ ...drag, ...p });
  };
  const endDrag = () => {
    if (!drag) return;
    const d = drag;
    setDrag(null);
    act(() => api.mapPlacement(d.id, d.az, d.m));
  };

  const setSpatial = (patch: Partial<SpatialConfig>) => act(() => api.spatial({ ...spatial, ...patch }));

  const puck = (o: OutputStatus, index: number) => {
    const dragging = drag?.id === o.endpoint_id;
    const az = dragging ? drag.az : o.resolved_azimuth;
    const m = dragging ? drag.m : o.config.distance_m ?? 2.5;
    const pos = az == null ? { x: 70 + index * 120, y: TRAY_Y } : toXY(az, m);
    const sel = selected === o.endpoint_id;
    const half = { point: 0, narrow: 15, wide: 45, headphones: 90 }[o.config.spread];
    return <g key={o.endpoint_id} className={`puck ${sel ? 'sel' : ''} ${o.config.subwoofer ? 'sub' : ''} ${o.bluetooth ? 'bt' : ''}`}
      transform={`translate(${pos.x} ${pos.y})`} onPointerDown={e => startDrag(o, e)} role="button" tabIndex={0}
      aria-label={`${o.name}${az != null ? `, ${az}° ${m} m` : ', not placed'}. Drag to move.`}
      onKeyDown={e => { if (e.key === 'Enter') setSelected(o.endpoint_id); }}>
      {half > 0 && az != null && <>
        <circle className="ear" r={4} cx={-Math.cos((az * Math.PI) / 180) * 22 * (half / 90 + 0.3)} cy={-Math.sin((az * Math.PI) / 180) * 22 * (half / 90 + 0.3)} />
        <circle className="ear" r={4} cx={Math.cos((az * Math.PI) / 180) * 22 * (half / 90 + 0.3)} cy={Math.sin((az * Math.PI) / 180) * 22 * (half / 90 + 0.3)} />
      </>}
      <circle className="body" r={19} />
      <text className="puck-icon" y={5}>{o.config.subwoofer ? 'SW' : o.bluetooth ? 'BT' : '♪'}</text>
      <text className="puck-label" y={36}>{shortName(o.name)}</text>
      {az != null && <text className="puck-meta" y={50}>{az}° · {m} m</text>}
    </g>;
  };

  return <div className="room">
    <div className="room-controls">
      <label className="check big">
        <input type="checkbox" checked={spatial.enabled} onChange={e => setSpatial({ enabled: e.target.checked })} />
        Spatial sound from this map
      </label>
      <p className="hint">When on, sound is panned to your speakers by where you place them. When off, each speaker plays what its card says.</p>

      <h3>Source layout</h3>
      <div className="segmented" role="radiogroup" aria-label="Source layout">
        {(['stereo', 'upmix71'] as SourceLayout[]).map(l => <button key={l} role="radio" aria-checked={spatial.layout === l}
          className={spatial.layout === l ? 'selected' : ''} disabled={!spatial.enabled}
          onClick={() => setSpatial({ layout: l })}>{l === 'stereo' ? 'Stereo' : '7.1 surround'}</button>)}
      </div>
      <p className="hint">7.1 builds center, side and back channels from the stereo audio (an upmix — Windows captures stereo). Outputs marked Subwoofer get the low bass.</p>

      <h3><Sparkles size={15} /> 8D audio</h3>
      <div className="row">
        <label className="check">
          <input type="checkbox" checked={spatial.rotation_s != null} disabled={!spatial.enabled}
            onChange={e => setSpatial({ rotation_s: e.target.checked ? speed : null })} />
          Rotate sound around me
        </label>
        <button className="icon" title="Direction" disabled={!rotating} aria-label="Change rotation direction"
          onClick={() => setSpatial({ clockwise: !spatial.clockwise })}>
          {spatial.clockwise ? <RotateCw size={15} /> : <RotateCcw size={15} />}
        </button>
      </div>
      <label className="speed">One turn every <strong>{speed} s</strong>
        <input type="range" min={2} max={60} step={1} value={speed} disabled={!rotating} aria-label="Seconds per turn"
          onChange={e => setSpeed(Number(e.target.value))}
          onPointerUp={() => setSpatial({ rotation_s: speed })} onKeyUp={() => setSpatial({ rotation_s: speed })} />
      </label>
      <p className="hint">Works best with headphones (set Width to Headphones) or speakers around you.</p>

      <label className="check"><input type="checkbox" checked={snap} onChange={e => setSnap(e.target.checked)} /> Snap to 7.1 positions</label>

      {current ? <div className="selected-box">
        <h3>{current.name}</h3>
        <p className="hint">{current.resolved_azimuth != null ? `${current.resolved_azimuth}° · ${current.config.distance_m ?? '—'} m` : 'Drag it onto the map to place it.'}</p>
        <label>Width
          <select value={current.config.spread} onChange={e => act(() => api.spread(current.endpoint_id, e.target.value as StereoSpread))}>
            {SPREADS.map(s => <option key={s.id} value={s.id} title={s.detail}>{s.label}</option>)}
          </select>
        </label>
        <label className="check">
          <input type="checkbox" checked={current.config.subwoofer}
            onChange={e => act(() => api.subwoofer(current.endpoint_id, e.target.checked))} /> Subwoofer (bass only)
        </label>
      </div> : <p className="hint">Click a speaker on the map to set its width or make it a subwoofer.</p>}
    </div>

    <svg ref={svg} className="room-svg" viewBox={`0 0 ${SIZE} ${SIZE + 20}`} onPointerMove={moveDrag} onPointerUp={endDrag}
      onPointerCancel={() => setDrag(null)} role="img" aria-label="Room map: you are in the centre, the screen is at the top">
      <defs>
        <radialGradient id="floor" cx="50%" cy="50%" r="60%">
          <stop offset="0%" stopColor="#1a2638" /><stop offset="100%" stopColor="#0f1621" />
        </radialGradient>
        <radialGradient id="glow"><stop offset="0%" stopColor="#5fe3cc" stopOpacity=".9" /><stop offset="100%" stopColor="#5fe3cc" stopOpacity="0" /></radialGradient>
      </defs>
      <rect x={14} y={14} width={SIZE - 28} height={SIZE - 48} rx={22} fill="url(#floor)" stroke="#2b394d" />
      <rect x={CX - 120} y={30} width={240} height={10} rx={5} className="screen" />
      <text x={CX} y={58} className="map-caption">SCREEN</text>
      {[1, 2, 3, 4, 5].map(m => <g key={m}>
        <circle cx={CX} cy={CY} r={m * PX_PER_M} className="ring" />
        <text x={CX + 4} y={CY - m * PX_PER_M + 12} className="ring-label">{m} m</text>
      </g>)}
      {GUIDES.map(g => { const p = toXY(g.az, 3.2); const line = toXY(g.az, 4.6); return <g key={g.label} className="guide">
        <line x1={CX} y1={CY} x2={line.x} y2={line.y} />
        <circle cx={p.x} cy={p.y} r={14} /><text x={p.x} y={p.y + 4}>{g.label}</text>
        <title>{g.name} ({g.az}°)</title>
      </g>; })}
      {spatial.enabled && <g className={`field ${rotating ? 'spin' : ''}`}>
        <circle cx={CX} cy={CY} r={1.6 * PX_PER_M} className="field-ring" />
        {VIRTUAL[spatial.layout].map(v => { const p = toXY(v.az + rotation, 1.6); return <g key={v.label}>
          <circle cx={p.x} cy={p.y} r={14} fill="url(#glow)" />
          <circle cx={p.x} cy={p.y} r={5} className="vdot" />
          <text x={p.x} y={p.y - 10} className="vlabel">{v.label}</text>
        </g>; })}
      </g>}
      <g className="listener" transform={`translate(${CX} ${CY})`}>
        <circle r={16} /><path d="M -9 -13 Q 0 -24 9 -13" /><text y={34}>You</text>
      </g>
      {unplaced.length > 0 && <text x={24} y={TRAY_Y - 26} className="map-caption left">Not placed yet — drag onto the map</text>}
      {placed.map((o, i) => puck(o, i))}
      {unplaced.map((o, i) => puck(o, i))}
    </svg>
  </div>;
}

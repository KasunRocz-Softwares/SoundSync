import { invoke } from '@tauri-apps/api/core';
import type {
  AudioEndpoint, AudioMode, ChannelRole, EngineSnapshot, ProfileLoadReport, SoundPreset, SpatialConfig,
  SpeakerPosition, StereoSpread,
} from './types';

export function errorMessage(error: unknown): string {
  if (error && typeof error === 'object' && 'message' in error) return String((error as { message: unknown }).message);
  return String(error);
}

export type SourceKind = 'default' | 'endpoint' | 'system_excluding_self';

export const api = {
  devices: () => invoke<AudioEndpoint[]>('get_audio_devices'),
  status: () => invoke<EngineSnapshot>('get_engine_status'),
  start: () => invoke<EngineSnapshot>('start_audio_engine'),
  stop: () => invoke<EngineSnapshot>('stop_audio_engine'),
  selectSource: (kind: SourceKind, endpointId?: string) =>
    invoke<void>('select_capture_source', { kind, endpointId: endpointId ?? null }),
  enable: (id: string) => invoke<void>('enable_device', { id }),
  disable: (id: string) => invoke<void>('disable_device', { id }),
  volume: (id: string, volume: number) => invoke<void>('set_device_volume', { id, volume }),
  mute: (id: string, muted: boolean) => invoke<void>('set_device_mute', { id, muted }),
  delay: (id: string, delayMs: number) => invoke<void>('set_device_delay', { id, delayMs }),
  mode: (mode: AudioMode) => invoke<void>('set_audio_mode', { mode }),
  saveProfile: (name: string) => invoke<string>('save_profile', { name }),
  listProfiles: () => invoke<string[]>('list_profiles'),
  loadProfile: (name: string) => invoke<ProfileLoadReport>('load_profile', { name }),
  rebind: (name: string, fromId: string, toId: string) =>
    invoke<void>('rebind_profile_device', { name, fromId, toId }),
  exportDiagnostics: () => invoke<string>('export_diagnostics'),
  placement: (id: string, position: SpeakerPosition, distanceM: number | null) =>
    invoke<void>('set_device_placement', { id, position, distanceM }),
  role: (id: string, role: ChannelRole) => invoke<void>('set_device_role', { id, role }),
  trim: (id: string, trimDb: number) => invoke<void>('set_device_trim', { id, trimDb }),
  distanceCompensation: (enabled: boolean) => invoke<void>('set_distance_compensation', { enabled }),
  preset: (preset: SoundPreset) => invoke<void>('apply_sound_preset', { preset }),
  mapPlacement: (id: string, azimuthDeg: number, distanceM: number | null) =>
    invoke<void>('set_device_map_placement', { id, azimuthDeg, distanceM }),
  spread: (id: string, spread: StereoSpread) => invoke<void>('set_device_spread', { id, spread }),
  subwoofer: (id: string, on: boolean) => invoke<void>('set_device_subwoofer', { id, on }),
  spatial: (c: SpatialConfig) =>
    invoke<void>('set_spatial', { enabled: c.enabled, layout: c.layout, rotationS: c.rotation_s, clockwise: c.clockwise }),
};

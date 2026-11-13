// Mirrors the serde shapes produced by syncsound-engine / the Tauri commands.
export type PcmFormat = { sample_rate: number; channels: number };

export type AudioEndpoint = {
  id: string;
  name: string;
  is_default: boolean;
  state: 'Active' | 'Unavailable';
  mix_format: PcmFormat | null;
  bits_per_sample: number | null;
  channel_mask: number | null;
  connection_type: string | null;
};

export type CaptureSource = 'DefaultRender' | 'SystemExcludingSelf' | { Endpoint: string };
export type CaptureTarget = 'SystemExcludingSelf' | { Endpoint: string };
export type CaptureState =
  | 'Stopped'
  | { Starting: CaptureTarget }
  | { Capturing: CaptureTarget }
  | { Unavailable: { endpoint_id: string | null } }
  | { Blocked: { reason: string } }
  | { Recoverable: { reason: string } };

export type AudioMode = 'gaming' | 'balanced' | 'synchronized';
export type LifecycleState = 'disabled' | 'starting' | 'running' | 'disconnected' | 'retrying' | 'failed';

export type OutputConfig = { endpoint_id: string; enabled: boolean; gain: number; muted: boolean; delay_ms: number };

export type OutputMetrics = {
  underruns: number;
  overruns: number;
  discontinuities: number;
  buffered_frames: number;
  rendered_frames: number;
  correction_ppm: number;
  device_buffer_frames: number;
  device_padding_frames: number;
  device_stream_latency_frames: number;
  primed: boolean;
};

export type OutputStatus = {
  endpoint_id: string;
  name: string;
  endpoint_available: boolean;
  config: OutputConfig;
  state: LifecycleState;
  last_error: string | null;
  retry_in_ms: number | null;
  align_ms: number;
  buffered_ms: number;
  device_latency_ms: number;
  estimated_latency_ms: number;
  metrics: OutputMetrics;
  warning: string | null;
};

export type ErrorRecord = { at_ms: number; scope: string; message: string };

export type EngineSnapshot = {
  running: boolean;
  mode: AudioMode;
  source: CaptureSource;
  capture_state: CaptureState;
  capture_format: PcmFormat;
  outputs: OutputStatus[];
  endpoints: AudioEndpoint[];
  recent_errors: ErrorRecord[];
  updated_at_ms: number;
};

export type MissingOutput = { endpoint_id: string; name_hint: string; enabled: boolean };
export type ProfileLoadReport = {
  name: string;
  mode: AudioMode;
  applied_outputs: number;
  missing: MissingOutput[];
  missing_source: string | null;
};

export type CommandError = { code: string; message: string };

export const MAX_ENABLED_OUTPUTS = 8;
export const MAX_DELAY_MS = 1000;

//! Event-driven WASAPI shared-mode render worker. One thread per output endpoint;
//! no mutable audio state is shared between outputs. The steady-state loop only
//! waits on the WASAPI event, calls `RenderPipeline::render`, encodes, and releases.
use super::windows::{Apartment, CaptureError, ComMemory};
use crate::render::{
    encode_samples, DeviceSampleFormat, OutputMetrics, RenderPipeline, WorkerPoll,
};
use crate::PcmFormat;
use std::sync::{
    atomic::{AtomicBool, AtomicU8, Ordering},
    Arc, Mutex,
};
use std::thread::JoinHandle;
use windows::{
    core::{w, PCWSTR},
    Win32::{
        Foundation::{CloseHandle, HANDLE, WAIT_OBJECT_0},
        Media::{Audio::*, KernelStreaming::WAVE_FORMAT_EXTENSIBLE},
        System::{
            Com::{CoCreateInstance, CLSCTX_ALL},
            Threading::{
                AvRevertMmThreadCharacteristics, AvSetMmThreadCharacteristicsW, CreateEventW,
                WaitForSingleObject,
            },
        },
    },
};

const STARTING: u8 = 0;
const RUNNING: u8 = 1;
const FAILED: u8 = 2;
const STOPPED: u8 = 3;

struct WorkerStatus {
    state: AtomicU8,
    // Written once on the failure path only, never in the steady render loop.
    error: Mutex<Option<String>>,
}

pub struct RenderWorker {
    stop: Arc<AtomicBool>,
    status: Arc<WorkerStatus>,
    thread: Option<JoinHandle<()>>,
}

struct EventHandle(HANDLE);
impl Drop for EventHandle {
    fn drop(&mut self) {
        unsafe {
            let _ = CloseHandle(self.0);
        }
    }
}

impl RenderWorker {
    /// `format` is the render format (always stereo). `exclusive` asks for
    /// WASAPI exclusive mode, falling back to shared if the device refuses.
    pub fn spawn(
        endpoint_id: String,
        pipeline: RenderPipeline,
        format: PcmFormat,
        buffer_hns: i64,
        metrics: Arc<OutputMetrics>,
        exclusive: bool,
    ) -> Self {
        let stop = Arc::new(AtomicBool::new(false));
        let status = Arc::new(WorkerStatus {
            state: AtomicU8::new(STARTING),
            error: Mutex::new(None),
        });
        let thread = {
            let stop = stop.clone();
            let status = status.clone();
            std::thread::Builder::new()
                .name("syncsound-render".into())
                .spawn(move || {
                    let result = run(
                        &endpoint_id,
                        pipeline,
                        format,
                        buffer_hns,
                        &metrics,
                        &stop,
                        &status,
                        exclusive,
                    );
                    match result {
                        Ok(()) => status.state.store(STOPPED, Ordering::Release),
                        Err(error) => {
                            if let Ok(mut slot) = status.error.lock() {
                                *slot = Some(describe(&error));
                            }
                            status.state.store(FAILED, Ordering::Release);
                        }
                    }
                })
                .ok()
        };
        if thread.is_none() {
            *status.error.lock().unwrap() = Some("Could not start render thread".into());
            status.state.store(FAILED, Ordering::Release);
        }
        Self {
            stop,
            status,
            thread,
        }
    }

    pub fn poll(&self) -> WorkerPoll {
        match self.status.state.load(Ordering::Acquire) {
            STARTING => WorkerPoll::Starting,
            RUNNING => WorkerPoll::Running,
            FAILED => WorkerPoll::Failed(
                self.status
                    .error
                    .lock()
                    .ok()
                    .and_then(|e| e.clone())
                    .unwrap_or_else(|| "Render worker failed".into()),
            ),
            _ => WorkerPoll::Stopped,
        }
    }
}

impl Drop for RenderWorker {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Release);
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

fn describe(error: &CaptureError) -> String {
    if let CaptureError::Windows(inner) = error {
        if inner.code() == AUDCLNT_E_DEVICE_INVALIDATED {
            return "Device was removed or disabled".into();
        }
        if inner.code() == AUDCLNT_E_DEVICE_IN_USE {
            return "Device is in exclusive use by another application".into();
        }
    }
    error.to_string()
}

fn float_wave(format: PcmFormat) -> WAVEFORMATEX {
    WAVEFORMATEX {
        wFormatTag: 3,
        nChannels: format.channels,
        nSamplesPerSec: format.sample_rate,
        nAvgBytesPerSec: format.sample_rate * u32::from(format.channels) * 4,
        nBlockAlign: format.channels * 4,
        wBitsPerSample: 32,
        cbSize: 0,
    }
}

/// Integer container of a shared-mode mix format, when it matches the source layout.
unsafe fn integer_mix_format(
    wave: *const WAVEFORMATEX,
    format: PcmFormat,
) -> Option<DeviceSampleFormat> {
    let wave_ref = unsafe { &*wave };
    if wave_ref.nSamplesPerSec != format.sample_rate || wave_ref.nChannels != format.channels {
        return None;
    }
    let is_pcm = match u32::from(wave_ref.wFormatTag) {
        1 => true,
        tag if tag == WAVE_FORMAT_EXTENSIBLE => {
            let ext = unsafe { &*wave.cast::<WAVEFORMATEXTENSIBLE>() };
            ext.SubFormat.data1 == 1
        }
        _ => false,
    };
    if !is_pcm {
        return None;
    }
    match wave_ref.wBitsPerSample {
        16 => Some(DeviceSampleFormat::I16),
        24 => Some(DeviceSampleFormat::I24),
        32 => Some(DeviceSampleFormat::I32),
        _ => None,
    }
}

const KSDATAFORMAT_SUBTYPE_PCM: windows::core::GUID =
    windows::core::GUID::from_u128(0x00000001_0000_0010_8000_00aa00389b71);
const KSDATAFORMAT_SUBTYPE_IEEE_FLOAT: windows::core::GUID =
    windows::core::GUID::from_u128(0x00000003_0000_0010_8000_00aa00389b71);

/// Stereo WAVEFORMATEXTENSIBLE for an exclusive-mode candidate format.
fn extensible(format: PcmFormat, sample: DeviceSampleFormat, valid_bits: u16) -> WAVEFORMATEXTENSIBLE {
    let bits = (sample.bytes_per_sample() * 8) as u16;
    let block = format.channels * bits / 8;
    WAVEFORMATEXTENSIBLE {
        Format: WAVEFORMATEX {
            wFormatTag: WAVE_FORMAT_EXTENSIBLE as u16,
            nChannels: format.channels,
            nSamplesPerSec: format.sample_rate,
            nAvgBytesPerSec: format.sample_rate * u32::from(block),
            nBlockAlign: block,
            wBitsPerSample: bits,
            cbSize: (std::mem::size_of::<WAVEFORMATEXTENSIBLE>() - std::mem::size_of::<WAVEFORMATEX>())
                as u16,
        },
        Samples: WAVEFORMATEXTENSIBLE_0 {
            wValidBitsPerSample: valid_bits,
        },
        dwChannelMask: 0x3, // front left + front right
        SubFormat: if sample == DeviceSampleFormat::F32 {
            KSDATAFORMAT_SUBTYPE_IEEE_FLOAT
        } else {
            KSDATAFORMAT_SUBTYPE_PCM
        },
    }
}

/// Opens the device in exclusive mode with the first format it accepts at the
/// engine's rate. Returns the client and the sample container to encode.
unsafe fn open_exclusive(
    device: &IMMDevice,
    format: PcmFormat,
) -> Result<(IAudioClient, DeviceSampleFormat), CaptureError> {
    let candidates = [
        (DeviceSampleFormat::F32, 32),
        (DeviceSampleFormat::I32, 32),
        (DeviceSampleFormat::I32, 24),
        (DeviceSampleFormat::I24, 24),
        (DeviceSampleFormat::I16, 16),
    ];
    let probe: IAudioClient = unsafe { device.Activate(CLSCTX_ALL, None)? };
    let Some((sample, wave)) = candidates.iter().find_map(|(sample, valid)| {
        let wave = extensible(format, *sample, *valid);
        let hr = unsafe {
            probe.IsFormatSupported(AUDCLNT_SHAREMODE_EXCLUSIVE, &wave.Format, None)
        };
        (hr == windows::Win32::Foundation::S_OK).then_some((*sample, wave))
    }) else {
        return Err(CaptureError::Windows(windows::core::Error::from_hresult(
            AUDCLNT_E_UNSUPPORTED_FORMAT,
        )));
    };
    let mut period = 0i64;
    unsafe { probe.GetDevicePeriod(Some(&mut period), None)? };
    drop(probe);
    let init = |period: i64| -> windows::core::Result<IAudioClient> {
        let client: IAudioClient = unsafe { device.Activate(CLSCTX_ALL, None)? };
        unsafe {
            client.Initialize(
                AUDCLNT_SHAREMODE_EXCLUSIVE,
                AUDCLNT_STREAMFLAGS_EVENTCALLBACK,
                period,
                period,
                &wave.Format,
                None,
            )?;
        }
        Ok(client)
    };
    match init(period) {
        Ok(client) => Ok((client, sample)),
        Err(error) if error.code() == AUDCLNT_E_BUFFER_SIZE_NOT_ALIGNED => {
            // Retry with the period the device rounds its buffer to.
            let client: IAudioClient = unsafe { device.Activate(CLSCTX_ALL, None)? };
            let _ = unsafe {
                client.Initialize(
                    AUDCLNT_SHAREMODE_EXCLUSIVE,
                    AUDCLNT_STREAMFLAGS_EVENTCALLBACK,
                    period,
                    period,
                    &wave.Format,
                    None,
                )
            };
            let frames = unsafe { client.GetBufferSize()? };
            let aligned = (10_000_000.0 * f64::from(frames) / f64::from(format.sample_rate)
                + 0.5) as i64;
            Ok((init(aligned)?, sample))
        }
        Err(error) => Err(CaptureError::Windows(error)),
    }
}

#[allow(clippy::too_many_arguments)]
fn run(
    endpoint_id: &str,
    mut pipeline: RenderPipeline,
    format: PcmFormat,
    buffer_hns: i64,
    metrics: &OutputMetrics,
    stop: &AtomicBool,
    status: &WorkerStatus,
    exclusive: bool,
) -> Result<(), CaptureError> {
    let _apartment = Apartment::new()?;
    unsafe {
        let enumerator: IMMDeviceEnumerator =
            CoCreateInstance(&MMDeviceEnumerator, None, CLSCTX_ALL)?;
        let wide: Vec<u16> = endpoint_id.encode_utf16().chain(Some(0)).collect();
        let device = enumerator.GetDevice(PCWSTR(wide.as_ptr()))?;

        // Exclusive mode first when asked; if the device refuses (in use, no
        // matching format, Bluetooth…), fall back to shared mode below.
        let exclusive_client = if exclusive {
            open_exclusive(&device, format).ok()
        } else {
            None
        };
        metrics
            .exclusive_active
            .store(exclusive_client.is_some(), Ordering::Relaxed);
        let is_exclusive = exclusive_client.is_some();
        let client: IAudioClient = device.Activate(CLSCTX_ALL, None)?;

        // Prefer float32 in the source layout and let the shared engine convert rate,
        // channels and container. If that is refused, accept an integer mix format
        // with an identical layout and encode it ourselves. Anything else fails.
        let event_flags = AUDCLNT_STREAMFLAGS_EVENTCALLBACK;
        let wave = float_wave(format);
        let mut sample_format = DeviceSampleFormat::F32;
        let first = if let Some((_, sample)) = &exclusive_client {
            sample_format = *sample;
            Ok(())
        } else {
            client.Initialize(
            AUDCLNT_SHAREMODE_SHARED,
            event_flags
                | AUDCLNT_STREAMFLAGS_AUTOCONVERTPCM
                | AUDCLNT_STREAMFLAGS_SRC_DEFAULT_QUALITY,
            buffer_hns,
            0,
            &wave,
            None,
        )
        };
        let client = if let Some((exclusive, _)) = exclusive_client {
            exclusive
        } else if let Err(error) = first {
            // A failed Initialize leaves the client unusable; activate a fresh one.
            let client: IAudioClient = device.Activate(CLSCTX_ALL, None)?;
            let mix = ComMemory(client.GetMixFormat()?);
            let Some(integer) = integer_mix_format(mix.0, format) else {
                return Err(CaptureError::Windows(error));
            };
            client.Initialize(
                AUDCLNT_SHAREMODE_SHARED,
                event_flags,
                buffer_hns,
                0,
                mix.0,
                None,
            )?;
            sample_format = integer;
            client
        } else {
            client
        };

        let event = EventHandle(CreateEventW(None, false, false, PCWSTR::null())?);
        client.SetEventHandle(event.0)?;
        let render: IAudioRenderClient = client.GetService()?;
        let buffer_frames = client.GetBufferSize()?;
        let latency_hns = client.GetStreamLatency().unwrap_or(0).max(0) as u64;
        metrics.set_device_timing(
            u64::from(buffer_frames),
            latency_hns * u64::from(format.sample_rate) / 10_000_000,
        );

        let channels = usize::from(format.channels);
        let bytes_per_frame = channels * sample_format.bytes_per_sample();
        let mut scratch = vec![0.0_f32; buffer_frames as usize * channels];

        // Pre-roll silence so the first event does not glitch.
        render.GetBuffer(buffer_frames)?;
        render.ReleaseBuffer(buffer_frames, AUDCLNT_BUFFERFLAGS_SILENT.0 as u32)?;

        let mut task_index = 0_u32;
        let mmcss = AvSetMmThreadCharacteristicsW(w!("Pro Audio"), &mut task_index).ok();
        client.Start()?;
        status.state.store(RUNNING, Ordering::Release);

        let mut timeouts = 0;
        let result = (|| -> Result<(), CaptureError> {
            while !stop.load(Ordering::Acquire) {
                if WaitForSingleObject(event.0, 200) != WAIT_OBJECT_0 {
                    timeouts += 1;
                    // ~2 s without a buffer event: the endpoint stopped servicing us.
                    if timeouts >= 10 {
                        return Err(CaptureError::Windows(windows::core::Error::from_hresult(
                            AUDCLNT_E_DEVICE_INVALIDATED,
                        )));
                    }
                    continue;
                }
                timeouts = 0;
                // Exclusive event mode hands over the whole buffer on every event.
                let padding = if is_exclusive {
                    0
                } else {
                    client.GetCurrentPadding()?
                };
                metrics.set_device_padding(u64::from(padding));
                let available = buffer_frames.saturating_sub(padding);
                if available == 0 {
                    continue;
                }
                let samples = &mut scratch[..available as usize * channels];
                pipeline.render(samples);
                let data = render.GetBuffer(available)?;
                let bytes =
                    std::slice::from_raw_parts_mut(data, available as usize * bytes_per_frame);
                let written = encode_samples(samples, sample_format, bytes);
                render.ReleaseBuffer(
                    available,
                    if written.is_ok() {
                        0
                    } else {
                        AUDCLNT_BUFFERFLAGS_SILENT.0 as u32
                    },
                )?;
            }
            Ok(())
        })();
        let _ = client.Stop();
        if let Some(handle) = mmcss {
            let _ = AvRevertMmThreadCharacteristics(handle);
        }
        result
    }
}

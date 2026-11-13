//! Microphone listing and recording for calibration. Recording runs on its own
//! MTA thread and returns mono float32 at the requested rate (Windows converts).
use super::windows::{device_id, Apartment, CaptureError, ComMemory};
use serde::{Deserialize, Serialize};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{mpsc, Arc};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};
use windows::core::PCWSTR;
use windows::Win32::Devices::FunctionDiscovery::PKEY_Device_FriendlyName;
use windows::Win32::Media::Audio::*;
use windows::Win32::System::Com::StructuredStorage::PropVariantToStringAlloc;
use windows::Win32::System::Com::{CoCreateInstance, CLSCTX_ALL, STGM_READ};

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Microphone {
    pub id: String,
    pub name: String,
    pub is_default: bool,
}

pub fn list_microphones() -> Result<Vec<Microphone>, CaptureError> {
    let _apartment = Apartment::new()?;
    unsafe {
        let enumerator: IMMDeviceEnumerator =
            CoCreateInstance(&MMDeviceEnumerator, None, CLSCTX_ALL)?;
        let default = enumerator
            .GetDefaultAudioEndpoint(eCapture, eConsole)
            .ok()
            .and_then(|d| device_id(&d).ok());
        let devices = enumerator.EnumAudioEndpoints(eCapture, DEVICE_STATE_ACTIVE)?;
        let mut mics = Vec::new();
        for i in 0..devices.GetCount()? {
            let Ok(device) = devices.Item(i) else {
                continue;
            };
            let Ok(id) = device_id(&device) else { continue };
            let name = device
                .OpenPropertyStore(STGM_READ)
                .ok()
                .and_then(|store| {
                    let value = store.GetValue(&PKEY_Device_FriendlyName).ok()?;
                    let text = PropVariantToStringAlloc(&value).ok()?;
                    let _memory = ComMemory(text.0);
                    text.to_string().ok()
                })
                .unwrap_or_else(|| id.clone());
            mics.push(Microphone {
                is_default: default.as_deref() == Some(id.as_str()),
                id,
                name,
            });
        }
        Ok(mics)
    }
}

/// Records until `finish` is called. `started_at` is when capture began, so the
/// caller can line up the recording with what it played.
pub struct MicRecorder {
    stop: Arc<AtomicBool>,
    thread: Option<JoinHandle<Result<Vec<f32>, String>>>,
    pub started_at: Instant,
}

impl MicRecorder {
    pub fn start(mic_id: String, rate: u32) -> Result<Self, String> {
        let stop = Arc::new(AtomicBool::new(false));
        let (ready_tx, ready_rx) = mpsc::sync_channel(1);
        let thread = {
            let stop = stop.clone();
            std::thread::Builder::new()
                .name("syncsound-mic".into())
                .spawn(move || record(&mic_id, rate, &stop, ready_tx))
                .map_err(|e| e.to_string())?
        };
        match ready_rx.recv_timeout(Duration::from_secs(5)) {
            Ok(Ok(started_at)) => Ok(Self {
                stop,
                thread: Some(thread),
                started_at,
            }),
            Ok(Err(error)) => {
                let _ = thread.join();
                Err(error)
            }
            Err(_) => {
                stop.store(true, Ordering::Release);
                Err("Microphone did not start".into())
            }
        }
    }

    pub fn finish(mut self) -> Result<Vec<f32>, String> {
        self.stop.store(true, Ordering::Release);
        self.thread
            .take()
            .map(|t| {
                t.join()
                    .unwrap_or_else(|_| Err("Microphone thread failed".into()))
            })
            .unwrap_or_else(|| Err("Microphone was not running".into()))
    }
}

impl Drop for MicRecorder {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Release);
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

fn record(
    mic_id: &str,
    rate: u32,
    stop: &AtomicBool,
    ready: mpsc::SyncSender<Result<Instant, String>>,
) -> Result<Vec<f32>, String> {
    let setup = || -> Result<(Apartment, IAudioClient, IAudioCaptureClient), CaptureError> {
        let apartment = Apartment::new()?;
        unsafe {
            let enumerator: IMMDeviceEnumerator =
                CoCreateInstance(&MMDeviceEnumerator, None, CLSCTX_ALL)?;
            let wide: Vec<u16> = mic_id.encode_utf16().chain(Some(0)).collect();
            let device = enumerator.GetDevice(PCWSTR(wide.as_ptr()))?;
            let client: IAudioClient = device.Activate(CLSCTX_ALL, None)?;
            let wave = WAVEFORMATEX {
                wFormatTag: 3,
                nChannels: 1,
                nSamplesPerSec: rate,
                nAvgBytesPerSec: rate * 4,
                nBlockAlign: 4,
                wBitsPerSample: 32,
                cbSize: 0,
            };
            client.Initialize(
                AUDCLNT_SHAREMODE_SHARED,
                AUDCLNT_STREAMFLAGS_AUTOCONVERTPCM | AUDCLNT_STREAMFLAGS_SRC_DEFAULT_QUALITY,
                1_000_000,
                0,
                &wave,
                None,
            )?;
            let capture: IAudioCaptureClient = client.GetService()?;
            client.Start()?;
            Ok((apartment, client, capture))
        }
    };
    let (_apartment, client, capture) = match setup() {
        Ok(parts) => parts,
        Err(error) => {
            let message = format!("Could not open the microphone: {error}");
            let _ = ready.send(Err(message.clone()));
            return Err(message);
        }
    };
    let _ = ready.send(Ok(Instant::now()));
    let mut samples: Vec<f32> = Vec::with_capacity(rate as usize * 60);
    let result = (|| -> windows::core::Result<()> {
        while !stop.load(Ordering::Acquire) {
            std::thread::sleep(Duration::from_millis(5));
            unsafe {
                while capture.GetNextPacketSize()? > 0 {
                    let mut data = std::ptr::null_mut();
                    let mut frames = 0;
                    let mut flags = 0;
                    capture.GetBuffer(&mut data, &mut frames, &mut flags, None, None)?;
                    if flags & AUDCLNT_BUFFERFLAGS_SILENT.0 as u32 != 0 || data.is_null() {
                        samples.extend(std::iter::repeat(0.0).take(frames as usize));
                    } else {
                        samples.extend_from_slice(std::slice::from_raw_parts(
                            data.cast::<f32>(),
                            frames as usize,
                        ));
                    }
                    capture.ReleaseBuffer(frames)?;
                }
            }
        }
        Ok(())
    })();
    unsafe {
        let _ = client.Stop();
    }
    result.map_err(|e| format!("Microphone stopped: {e}"))?;
    Ok(samples)
}

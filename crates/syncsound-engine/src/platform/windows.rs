//! Thread-affine WASAPI adapter. Call control methods and `poll` on one MTA worker.
//! Notification and activation callbacks only set atomics; they never touch PCM or COM ownership.
use crate::capture::{
    AudioEndpoint, CaptureConfig, CaptureController, CaptureState, CaptureTarget, EndpointState,
    Packet, PacketPublisher,
};
use crate::{PcmConsumer, PcmFormat};
use std::{
    marker::PhantomData,
    mem::{size_of, ManuallyDrop},
    rc::Rc,
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc,
    },
    time::{Duration, Instant},
};
use windows::{
    core::{implement, Interface, HRESULT, PCWSTR},
    Win32::{
        Devices::FunctionDiscovery::PKEY_Device_FriendlyName,
        Foundation::{E_FAIL, E_POINTER, PROPERTYKEY},
        Media::{Audio::*, KernelStreaming::WAVE_FORMAT_EXTENSIBLE},
        System::{
            Com::{StructuredStorage::*, *},
            Variant::VT_BLOB,
        },
    },
};

#[derive(Debug, thiserror::Error)]
pub enum CaptureError {
    #[error("Windows audio: {0}")]
    Windows(#[from] windows::core::Error),
    #[error("Invalid bounded capture configuration")]
    InvalidConfig,
    #[error("Process-exclusion activation timed out; no endpoint fallback was used")]
    ActivationTimeout,
    #[error("Capture is not available: {0:?}")]
    NotAvailable(CaptureState),
}

// Declared last in owners, so every interface is released before CoUninitialize.
struct Apartment(PhantomData<Rc<()>>);
impl Apartment {
    fn new() -> Result<Self, CaptureError> {
        unsafe {
            CoInitializeEx(None, COINIT_MULTITHREADED).ok()?;
        }
        Ok(Self(PhantomData))
    }
}
impl Drop for Apartment {
    fn drop(&mut self) {
        unsafe {
            CoUninitialize();
        }
    }
}

struct ComMemory<T>(*mut T);
impl<T> Drop for ComMemory<T> {
    fn drop(&mut self) {
        unsafe {
            CoTaskMemFree(Some(self.0.cast()));
        }
    }
}

fn device_id(device: &IMMDevice) -> windows::core::Result<String> {
    unsafe {
        let id = device.GetId()?;
        let _memory = ComMemory(id.0);
        Ok(id.to_string()?)
    }
}

#[implement(IMMNotificationClient)]
struct Notifications {
    changed: Arc<AtomicBool>,
}
impl IMMNotificationClient_Impl for Notifications_Impl {
    fn OnDeviceStateChanged(&self, _: &PCWSTR, _: DEVICE_STATE) -> windows::core::Result<()> {
        self.changed.store(true, Ordering::Release);
        Ok(())
    }
    fn OnDeviceAdded(&self, _: &PCWSTR) -> windows::core::Result<()> {
        self.changed.store(true, Ordering::Release);
        Ok(())
    }
    fn OnDeviceRemoved(&self, _: &PCWSTR) -> windows::core::Result<()> {
        self.changed.store(true, Ordering::Release);
        Ok(())
    }
    fn OnDefaultDeviceChanged(
        &self,
        flow: EDataFlow,
        role: ERole,
        _: &PCWSTR,
    ) -> windows::core::Result<()> {
        if flow == eRender && role == eConsole {
            self.changed.store(true, Ordering::Release);
        }
        Ok(())
    }
    fn OnPropertyValueChanged(&self, _: &PCWSTR, _: &PROPERTYKEY) -> windows::core::Result<()> {
        self.changed.store(true, Ordering::Release);
        Ok(())
    }
}

/// Enumerates all render endpoints, including disabled/unplugged devices.
/// The default role is eConsole, used consistently by discovery and notifications.
pub struct EndpointCatalog {
    enumerator: IMMDeviceEnumerator,
    notifications: IMMNotificationClient,
    changed: Arc<AtomicBool>,
    _apartment: Apartment,
}

impl EndpointCatalog {
    pub fn new() -> Result<Self, CaptureError> {
        let apartment = Apartment::new()?;
        let enumerator: IMMDeviceEnumerator =
            unsafe { CoCreateInstance(&MMDeviceEnumerator, None, CLSCTX_ALL)? };
        let changed = Arc::new(AtomicBool::new(true));
        let notifications: IMMNotificationClient = Notifications {
            changed: changed.clone(),
        }
        .into();
        unsafe {
            enumerator.RegisterEndpointNotificationCallback(&notifications)?;
        }
        Ok(Self {
            enumerator,
            notifications,
            changed,
            _apartment: apartment,
        })
    }

    pub fn enumerate(&self) -> Result<Vec<AudioEndpoint>, CaptureError> {
        unsafe {
            let default = self
                .enumerator
                .GetDefaultAudioEndpoint(eRender, eConsole)
                .ok()
                .and_then(|device| device_id(&device).ok());
            let devices = self
                .enumerator
                .EnumAudioEndpoints(eRender, DEVICE_STATE(DEVICE_STATEMASK_ALL))?;
            let mut endpoints = Vec::new();
            for index in 0..devices.GetCount()? {
                // A hot-unplug may race enumeration. Skip a vanished device; never invent an ID.
                let Ok(device) = devices.Item(index) else {
                    continue;
                };
                let Ok(id) = device_id(&device) else {
                    continue;
                };
                let state = if device.GetState().ok() == Some(DEVICE_STATE_ACTIVE) {
                    EndpointState::Active
                } else {
                    EndpointState::Unavailable
                };
                let properties = device.OpenPropertyStore(STGM_READ).ok();
                let name = properties
                    .as_ref()
                    .and_then(|store| {
                        let value = store.GetValue(&PKEY_Device_FriendlyName).ok()?;
                        let text = PropVariantToStringAlloc(&value).ok()?;
                        let _memory = ComMemory(text.0);
                        text.to_string().ok()
                    })
                    .unwrap_or_else(|| id.clone());
                let connection_type = properties.as_ref().and_then(|store| {
                    let value = store.GetValue(&PKEY_AudioEndpoint_FormFactor).ok()?;
                    let form = PropVariantToUInt32(&value).ok()?;
                    Some(
                        match form {
                            0 => "Remote network",
                            1 => "Speakers",
                            2 => "Line level",
                            3 => "Headphones",
                            5 => "Headset",
                            6 => "Handset",
                            8 => "S/PDIF",
                            9 => "HDMI / digital display",
                            _ => return None,
                        }
                        .to_string(),
                    )
                });
                let mut mix_format = None;
                let mut bits_per_sample = None;
                let mut channel_mask = None;
                if state == EndpointState::Active {
                    if let Ok(client) = device.Activate::<IAudioClient>(CLSCTX_ALL, None) {
                        if let Ok(pointer) = client.GetMixFormat() {
                            let memory = ComMemory(pointer);
                            if !memory.0.is_null() {
                                let wave = &*memory.0;
                                mix_format = Some(PcmFormat {
                                    sample_rate: wave.nSamplesPerSec,
                                    channels: wave.nChannels,
                                });
                                bits_per_sample = Some(wave.wBitsPerSample);
                                if wave.wFormatTag == WAVE_FORMAT_EXTENSIBLE as u16
                                    && wave.cbSize as usize
                                        >= size_of::<WAVEFORMATEXTENSIBLE>()
                                            - size_of::<WAVEFORMATEX>()
                                {
                                    channel_mask = Some(
                                        (*memory.0.cast::<WAVEFORMATEXTENSIBLE>()).dwChannelMask,
                                    );
                                }
                            }
                        }
                    }
                }
                endpoints.push(AudioEndpoint {
                    is_default: default.as_ref() == Some(&id),
                    id,
                    name,
                    state,
                    mix_format,
                    bits_per_sample,
                    channel_mask,
                    connection_type,
                });
            }
            Ok(endpoints)
        }
    }
}
impl Drop for EndpointCatalog {
    fn drop(&mut self) {
        unsafe {
            let _ = self
                .enumerator
                .UnregisterEndpointNotificationCallback(&self.notifications);
        }
    }
}

#[implement(IActivateAudioInterfaceCompletionHandler)]
struct ActivationCompletion {
    done: Arc<AtomicBool>,
    // Heap address stays stable even after moving this object into COM. Retained for
    // late completion, including timeout. No borrowed stack blob can outlive activation.
    _params: Box<AUDIOCLIENT_ACTIVATION_PARAMS>,
}
impl IActivateAudioInterfaceCompletionHandler_Impl for ActivationCompletion_Impl {
    fn ActivateCompleted(
        &self,
        _: windows::core::Ref<IActivateAudioInterfaceAsyncOperation>,
    ) -> windows::core::Result<()> {
        self.done.store(true, Ordering::Release);
        Ok(())
    }
}

fn process_exclusion_params() -> AUDIOCLIENT_ACTIVATION_PARAMS {
    AUDIOCLIENT_ACTIVATION_PARAMS {
        ActivationType: AUDIOCLIENT_ACTIVATION_TYPE_PROCESS_LOOPBACK,
        Anonymous: AUDIOCLIENT_ACTIVATION_PARAMS_0 {
            ProcessLoopbackParams: AUDIOCLIENT_PROCESS_LOOPBACK_PARAMS {
                TargetProcessId: std::process::id(),
                ProcessLoopbackMode: PROCESS_LOOPBACK_MODE_EXCLUDE_TARGET_PROCESS_TREE,
            },
        },
    }
}

fn activate_excluding_self() -> Result<IAudioClient, CaptureError> {
    let mut params = Box::new(process_exclusion_params());
    // PROPVARIANT normally frees VT_BLOB with CoTaskMemFree. This blob is borrowed
    // from a Box owned by the completion object; suppress that automatic free.
    let mut variant = ManuallyDrop::new(PROPVARIANT::default());
    unsafe {
        let inner = &mut *variant.Anonymous.Anonymous;
        inner.vt = VT_BLOB;
        inner.Anonymous.blob = BLOB {
            cbSize: size_of::<AUDIOCLIENT_ACTIVATION_PARAMS>() as u32,
            pBlobData: (&mut *params as *mut AUDIOCLIENT_ACTIVATION_PARAMS).cast(),
        };
    }
    let done = Arc::new(AtomicBool::new(false));
    let completion: IActivateAudioInterfaceCompletionHandler = ActivationCompletion {
        done: done.clone(),
        _params: params,
    }
    .into();
    let operation = unsafe {
        ActivateAudioInterfaceAsync(
            VIRTUAL_AUDIO_DEVICE_PROCESS_LOOPBACK,
            &IAudioClient::IID,
            Some(&*variant),
            &completion,
        )?
    };
    let deadline = Instant::now() + Duration::from_secs(3);
    // Control-thread wait only. Neither COM callback blocks or sends IPC.
    while !done.load(Ordering::Acquire) {
        if Instant::now() >= deadline {
            return Err(CaptureError::ActivationTimeout);
        }
        std::thread::sleep(Duration::from_millis(1));
    }
    let mut status = HRESULT(0);
    let mut unknown = None;
    unsafe {
        operation.GetActivateResult(&mut status, &mut unknown)?;
    }
    status.ok()?;
    Ok(unknown
        .ok_or_else(|| windows::core::Error::from_hresult(E_POINTER))?
        .cast()?)
}

struct Session {
    capture: IAudioCaptureClient,
    client: IAudioClient,
    channels: usize,
    buffer_frames: u32,
}
impl Session {
    fn open(
        catalog: &EndpointCatalog,
        target: &CaptureTarget,
        format: PcmFormat,
    ) -> Result<Self, CaptureError> {
        let client = match target {
            CaptureTarget::Endpoint(id) => {
                let wide: Vec<_> = id.encode_utf16().chain(Some(0)).collect();
                unsafe {
                    catalog
                        .enumerator
                        .GetDevice(PCWSTR(wide.as_ptr()))?
                        .Activate::<IAudioClient>(CLSCTX_ALL, None)?
                }
            }
            CaptureTarget::SystemExcludingSelf => activate_excluding_self()?,
        };
        // Ask the shared audio engine to convert native integer/multichannel mix PCM
        // into the configured float layout. A rejected format fails; never reinterpret it.
        let wave = WAVEFORMATEX {
            wFormatTag: 3,
            nChannels: format.channels,
            nSamplesPerSec: format.sample_rate,
            nAvgBytesPerSec: format.sample_rate * u32::from(format.channels) * 4,
            nBlockAlign: format.channels * 4,
            wBitsPerSample: 32,
            cbSize: 0,
        };
        unsafe {
            client.Initialize(
                AUDCLNT_SHAREMODE_SHARED,
                AUDCLNT_STREAMFLAGS_LOOPBACK
                    | AUDCLNT_STREAMFLAGS_AUTOCONVERTPCM
                    | AUDCLNT_STREAMFLAGS_SRC_DEFAULT_QUALITY,
                200_000,
                0,
                &wave,
                None,
            )?;
            let capture = client.GetService::<IAudioCaptureClient>()?;
            let buffer_frames = client.GetBufferSize()?;
            let session = Self {
                capture,
                client,
                channels: format.channels as usize,
                buffer_frames,
            };
            session.client.Start()?;
            Ok(session)
        }
    }

    fn drain(&self, publisher: &mut PacketPublisher) -> windows::core::Result<()> {
        // Bound each service call even if the producer keeps supplying packets.
        for _ in 0..64 {
            unsafe {
                if self.capture.GetNextPacketSize()? == 0 {
                    break;
                }
                let mut data = std::ptr::null_mut();
                let mut frames = 0;
                let mut flags = 0;
                let mut device_position = 0;
                let mut qpc_100ns = 0;
                self.capture.GetBuffer(
                    &mut data,
                    &mut frames,
                    &mut flags,
                    Some(&mut device_position),
                    Some(&mut qpc_100ns),
                )?;
                if frames == 0 {
                    break;
                } // AUDCLNT_S_BUFFER_EMPTY acquires no packet.
                let silent = flags & AUDCLNT_BUFFERFLAGS_SILENT.0 as u32 != 0;
                if frames > self.buffer_frames || (!silent && data.is_null()) {
                    self.capture.ReleaseBuffer(frames)?;
                    return Err(windows::core::Error::from_hresult(E_FAIL));
                }
                let bytes = if silent {
                    &[]
                } else {
                    std::slice::from_raw_parts(data, frames as usize * self.channels * 4)
                };
                let _status = publisher.publish(Packet {
                    data: bytes,
                    frames: frames as usize,
                    device_position,
                    qpc_100ns,
                    silent,
                    discontinuity: flags & AUDCLNT_BUFFERFLAGS_DATA_DISCONTINUITY.0 as u32 != 0,
                    timestamp_error: flags & AUDCLNT_BUFFERFLAGS_TIMESTAMP_ERROR.0 as u32 != 0,
                });
                // Full/invalid packets are already flushed by the publisher. Always release.
                self.capture.ReleaseBuffer(frames)?;
            }
        }
        Ok(())
    }
}
impl Drop for Session {
    fn drop(&mut self) {
        unsafe {
            let _ = self.client.Stop();
        }
    }
}

/// One MTA control/worker thread owns this object and services `poll` regularly
/// (e.g. every 5 ms). It is deliberately !Send / !Sync. PCM crosses threads only
/// through the returned Task 2 consumer. Control methods may allocate and activate
/// COM; do not call them from a render callback. Drop stops and flushes capture.
pub struct LoopbackCapture {
    session: Option<Session>,
    controller: CaptureController,
    publisher: PacketPublisher,
    enabled: bool,
    retry_at: Instant,
    catalog: EndpointCatalog,
}
impl LoopbackCapture {
    pub fn new(config: CaptureConfig) -> Result<(Self, PcmConsumer), CaptureError> {
        let (publisher, consumer) =
            PacketPublisher::new(&config).map_err(|_| CaptureError::InvalidConfig)?;
        let catalog = EndpointCatalog::new()?;
        Ok((
            Self {
                session: None,
                controller: CaptureController::new(config),
                publisher,
                enabled: false,
                retry_at: Instant::now(),
                catalog,
            },
            consumer,
        ))
    }
    pub fn state(&self) -> &CaptureState {
        self.controller.state()
    }
    pub fn config(&self) -> &CaptureConfig {
        self.controller.config()
    }

    pub fn start(&mut self) -> Result<(), CaptureError> {
        self.enabled = true;
        self.refresh()?;
        if matches!(self.state(), CaptureState::Capturing(_)) {
            Ok(())
        } else {
            Err(CaptureError::NotAvailable(self.state().clone()))
        }
    }

    fn release(&mut self) {
        self.session = None;
        self.publisher.reset();
    }
    fn fail(&mut self, error: &CaptureError) {
        self.release();
        self.controller.failed(error.to_string());
        self.retry_at = Instant::now() + Duration::from_secs(1);
    }
    fn refresh(&mut self) -> Result<(), CaptureError> {
        self.catalog.changed.swap(false, Ordering::AcqRel);
        // Stop first: no old endpoint PCM can escape while routing is revalidated.
        self.release();
        self.controller.stop();
        let result = (|| {
            let endpoints = self.catalog.enumerate()?;
            if let Some(target) = self.controller.reconcile(&endpoints) {
                self.session = Some(Session::open(&self.catalog, &target, self.config().format)?);
                self.controller.started();
            }
            Ok(())
        })();
        if let Err(error) = &result {
            self.fail(error);
        }
        self.retry_at = Instant::now() + Duration::from_secs(1);
        result
    }

    /// Update the complete routing set before enabling any new render client.
    /// A blocked result has already stopped capture and invalidated queued audio.
    pub fn set_output_endpoints(&mut self, ids: Vec<String>) -> Result<(), CaptureError> {
        self.controller.set_outputs(ids);
        if self.enabled {
            self.refresh()?;
            if !matches!(self.state(), CaptureState::Capturing(_)) {
                return Err(CaptureError::NotAvailable(self.state().clone()));
            }
        }
        Ok(())
    }

    pub fn poll(&mut self) -> Result<(), CaptureError> {
        if !self.enabled {
            return Ok(());
        }
        if self.catalog.changed.load(Ordering::Acquire)
            || (self.session.is_none() && Instant::now() >= self.retry_at)
        {
            self.refresh()?;
        }
        if let Some(session) = &self.session {
            if let Err(error) = session.drain(&mut self.publisher) {
                let error = CaptureError::Windows(error);
                self.fail(&error);
                return Err(error);
            }
        }
        Ok(())
    }

    pub fn stop(&mut self) {
        self.enabled = false;
        self.release();
        self.controller.stop();
    }
}
impl Drop for LoopbackCapture {
    fn drop(&mut self) {
        self.stop();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn capture_native_activation_excludes_current_process_tree() {
        let params = process_exclusion_params();
        assert_eq!(
            params.ActivationType,
            AUDIOCLIENT_ACTIVATION_TYPE_PROCESS_LOOPBACK
        );
        let process = unsafe { params.Anonymous.ProcessLoopbackParams };
        assert_eq!(process.TargetProcessId, std::process::id());
        assert_eq!(
            process.ProcessLoopbackMode,
            PROCESS_LOOPBACK_MODE_EXCLUDE_TARGET_PROCESS_TREE
        );
    }

    #[test]
    fn capture_notifications_filter_default_role_and_signal_removal() {
        let changed = Arc::new(AtomicBool::new(false));
        let callback: IMMNotificationClient = Notifications {
            changed: changed.clone(),
        }
        .into();
        unsafe {
            callback
                .OnDefaultDeviceChanged(eCapture, eConsole, PCWSTR::null())
                .unwrap();
            callback
                .OnDefaultDeviceChanged(eRender, eCommunications, PCWSTR::null())
                .unwrap();
            assert!(!changed.load(Ordering::Acquire));
            callback
                .OnDefaultDeviceChanged(eRender, eConsole, PCWSTR::null())
                .unwrap();
            assert!(changed.swap(false, Ordering::AcqRel));
            callback.OnDeviceRemoved(PCWSTR::null()).unwrap();
            assert!(changed.load(Ordering::Acquire));
        }
    }
}

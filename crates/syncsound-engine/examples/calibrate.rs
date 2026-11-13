//! Acoustic offset measurement between the Windows default output (played directly
//! by Windows) and one SyncSound output that mirrors it.
//!
//!   cargo run -p syncsound-engine --example calibrate -- <mic-substring> <output-substring> <mode> [wav-dir]
//!
//! 1. SyncSound captures the default output and renders it to the chosen output.
//! 2. A click track (8 clicks, 1 s apart) is played on the default output.
//! 3. A WASAPI capture of the chosen microphone records both arrivals.
//! 4. For each click the first peak is the direct (default) output and the next
//!    strong peak ≥ 40 ms later is the mirrored output; the median gap is reported.
#[cfg(windows)]
fn main() {
    use std::time::{Duration, Instant};
    use syncsound_engine::capture::EndpointState;
    use syncsound_engine::commands::parse_mode;
    use syncsound_engine::{AudioEngine, CaptureSource, OutputConfig};

    let args: Vec<String> = std::env::args().skip(1).collect();
    let mic_filter = args.first().expect("mic substring").to_lowercase();
    let out_filter = args.get(1).expect("output substring").to_lowercase();
    let mode = parse_mode(args.get(2).map(String::as_str).unwrap_or("gaming")).unwrap();
    let dir = std::path::PathBuf::from(args.get(3).cloned().unwrap_or_else(|| ".".into()));
    // Control run: keep the mirrored output muted so only the direct arrival remains.
    let control = args.get(4).map(String::as_str) == Some("muted");
    // Baseline: SyncSound routes nothing (output disabled, engine still running).
    let baseline = args.get(4).map(String::as_str) == Some("off");
    // Observe: route nothing here; measure whatever the running SyncSound app plays.
    let observe = args.get(4).map(String::as_str) == Some("observe");
    const RATE: u32 = 48_000;
    const CLICKS: usize = 8;

    // --- Route default output → chosen output.
    let engine = AudioEngine::spawn();
    let endpoints = engine.refresh_endpoints().unwrap();
    let default = endpoints
        .iter()
        .find(|e| e.is_default && e.state == EndpointState::Active)
        .expect("active default output");
    let output = endpoints
        .iter()
        .find(|e| e.state == EndpointState::Active && e.name.to_lowercase().contains(&out_filter))
        .expect("output not found");
    println!("direct (Windows default): {}", default.name);
    println!("mirrored by SyncSound:    {} [{:?}]", output.name, mode);
    engine.set_source(CaptureSource::DefaultRender).unwrap();
    engine.set_mode(mode).unwrap();
    engine
        .apply_outputs(vec![OutputConfig {
            endpoint_id: output.id.clone(),
            enabled: !baseline && !observe,
            gain: 1.0,
            muted: control,
            delay_ms: 0,
        }])
        .unwrap();
    engine.start().unwrap();
    if control {
        println!("CONTROL RUN: mirrored output muted");
    }
    if baseline {
        println!("BASELINE RUN: SyncSound output disabled");
    }
    if observe {
        println!("OBSERVE RUN: measuring the running SyncSound app only");
    }
    std::thread::sleep(Duration::from_secs(2));
    let snapshot = engine.snapshot();
    println!("capture: {:?}", snapshot.capture_state);
    let estimated = snapshot
        .outputs
        .first()
        .map(|o| o.estimated_latency_ms)
        .unwrap_or(0.0);

    // --- Click track: 5 ms decaying noise bursts, 1 s apart, 0.9 peak.
    let total = RATE as usize * (CLICKS + 1);
    let mut track = vec![0.0f32; total];
    let mut seed = 0x1234_5678u32;
    for k in 0..CLICKS {
        let start = RATE as usize / 2 + k * RATE as usize;
        for i in 0..(RATE as usize * 5 / 1000) {
            seed ^= seed << 13;
            seed ^= seed >> 17;
            seed ^= seed << 5;
            let noise = (seed as f32 / u32::MAX as f32) * 2.0 - 1.0;
            track[start + i] = noise * 0.9 * (1.0 - i as f32 / 240.0);
        }
    }
    let wav = dir.join("syncsound-clicks.wav");
    write_wav(&wav, &track, RATE);

    // --- Record the microphone while the track plays on the default output.
    let stop = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    let recorder = {
        let stop = stop.clone();
        std::thread::spawn(move || record(&mic_filter, RATE, &stop))
    };
    std::thread::sleep(Duration::from_millis(500));
    let played = Instant::now();
    let status = std::process::Command::new("powershell")
        .args([
            "-NoProfile",
            "-Command",
            &format!(
                "(New-Object Media.SoundPlayer '{}').PlaySync()",
                wav.display()
            ),
        ])
        .status()
        .unwrap();
    assert!(status.success(), "click playback failed");
    println!(
        "click track played in {:.1} s",
        played.elapsed().as_secs_f32()
    );
    std::thread::sleep(Duration::from_secs(1));
    stop.store(true, std::sync::atomic::Ordering::Release);
    let (mic_name, recording) = recorder.join().unwrap();
    engine.stop().unwrap();
    println!(
        "microphone: {mic_name}, {:.1} s recorded",
        recording.len() as f32 / RATE as f32
    );
    write_wav(&dir.join("syncsound-recording.wav"), &recording, RATE);

    // --- Analysis: fold the high-passed signal on the exact 1 s click period.
    // Averaging all periods keeps sound locked to the click train and averages
    // away anything else (music, speech). Peaks are located on the folded envelope.
    let period = RATE as usize;
    let periods = recording.len() / period;
    assert!(periods >= 4, "recording too short");
    let mut fold = vec![0.0f32; period];
    for p in 0..periods {
        for i in 1..period {
            let at = p * period + i;
            fold[i] += (recording[at] - recording[at - 1]).abs();
        }
    }
    let half_ms = (RATE / 2000) as usize; // 0.5 ms bins, 1 ms smoothing
    let bins = period / half_ms;
    let env: Vec<f32> = (0..bins)
        .map(|b| {
            (0..half_ms * 2)
                .map(|j| fold[(b * half_ms + j) % period])
                .sum::<f32>()
                / (half_ms * 2 * periods) as f32
        })
        .collect();
    let mut sorted = env.clone();
    sorted.sort_by(|a, b| a.partial_cmp(b).unwrap());
    let noise = sorted[sorted.len() / 2];
    // Pick the strongest peaks at least 30 ms apart (circular).
    let mut order: Vec<usize> = (0..bins).collect();
    order.sort_by(|a, b| env[*b].partial_cmp(&env[*a]).unwrap());
    let ms = |b: usize| b as f64 * 0.5;
    let mut peaks: Vec<usize> = Vec::new();
    for b in order {
        let far = peaks.iter().all(|p| {
            let d = (ms(b) - ms(*p)).abs();
            d.min(1000.0 - d) > 30.0
        });
        if far {
            peaks.push(b);
        }
        if peaks.len() == 4 {
            break;
        }
    }
    println!("folded {periods} periods; noise {noise:.4}");
    for p in &peaks {
        println!(
            "  peak at {:6.1} ms in period, {:.1}x noise",
            ms(*p),
            env[*p] / noise
        );
    }
    let strong: Vec<usize> = peaks
        .iter()
        .copied()
        .filter(|p| env[*p] / noise > 3.0)
        .collect();
    if strong.len() < 2 {
        println!(
            "RESULT: only {} clear arrival(s); need a quieter room or louder output",
            strong.len()
        );
    } else {
        // The mirrored copy is always later than the direct output; assume < 500 ms.
        let forward = (ms(strong[1]) - ms(strong[0])).rem_euclid(1000.0);
        let gap = forward.min(1000.0 - forward);
        println!(
            "RESULT: {} plays {gap:.0} ms after {} (SyncSound estimate for its own part: {estimated:.0} ms)",
            output.name, default.name
        );
    }
}

#[cfg(windows)]
fn write_wav(path: &std::path::Path, samples: &[f32], rate: u32) {
    let mut bytes = Vec::with_capacity(44 + samples.len() * 2);
    let data_len = (samples.len() * 2) as u32;
    bytes.extend_from_slice(b"RIFF");
    bytes.extend_from_slice(&(36 + data_len).to_le_bytes());
    bytes.extend_from_slice(b"WAVEfmt ");
    bytes.extend_from_slice(&16u32.to_le_bytes());
    bytes.extend_from_slice(&1u16.to_le_bytes()); // PCM
    bytes.extend_from_slice(&1u16.to_le_bytes()); // mono
    bytes.extend_from_slice(&rate.to_le_bytes());
    bytes.extend_from_slice(&(rate * 2).to_le_bytes());
    bytes.extend_from_slice(&2u16.to_le_bytes());
    bytes.extend_from_slice(&16u16.to_le_bytes());
    bytes.extend_from_slice(b"data");
    bytes.extend_from_slice(&data_len.to_le_bytes());
    for s in samples {
        bytes.extend_from_slice(&((s.clamp(-1.0, 1.0) * 32_767.0) as i16).to_le_bytes());
    }
    std::fs::write(path, bytes).unwrap();
}

/// Shared-mode WASAPI microphone capture, converted by Windows to mono float.
#[cfg(windows)]
fn record(filter: &str, rate: u32, stop: &std::sync::atomic::AtomicBool) -> (String, Vec<f32>) {
    use windows::Win32::Devices::FunctionDiscovery::PKEY_Device_FriendlyName;
    use windows::Win32::Media::Audio::*;
    use windows::Win32::System::Com::StructuredStorage::PropVariantToStringAlloc;
    use windows::Win32::System::Com::*;
    unsafe {
        CoInitializeEx(None, COINIT_MULTITHREADED).ok().unwrap();
        let enumerator: IMMDeviceEnumerator =
            CoCreateInstance(&MMDeviceEnumerator, None, CLSCTX_ALL).unwrap();
        let devices = enumerator
            .EnumAudioEndpoints(eCapture, DEVICE_STATE_ACTIVE)
            .unwrap();
        let mut chosen = None;
        for i in 0..devices.GetCount().unwrap() {
            let device = devices.Item(i).unwrap();
            let store = device.OpenPropertyStore(STGM_READ).unwrap();
            let value = store.GetValue(&PKEY_Device_FriendlyName).unwrap();
            let name = PropVariantToStringAlloc(&value)
                .unwrap()
                .to_string()
                .unwrap();
            if name.to_lowercase().contains(filter) {
                chosen = Some((name, device));
                break;
            }
        }
        let (name, device) = chosen.expect("microphone not found");
        let client: IAudioClient = device.Activate(CLSCTX_ALL, None).unwrap();
        let wave = WAVEFORMATEX {
            wFormatTag: 3,
            nChannels: 1,
            nSamplesPerSec: rate,
            nAvgBytesPerSec: rate * 4,
            nBlockAlign: 4,
            wBitsPerSample: 32,
            cbSize: 0,
        };
        client
            .Initialize(
                AUDCLNT_SHAREMODE_SHARED,
                AUDCLNT_STREAMFLAGS_AUTOCONVERTPCM | AUDCLNT_STREAMFLAGS_SRC_DEFAULT_QUALITY,
                1_000_000,
                0,
                &wave,
                None,
            )
            .unwrap();
        let capture: IAudioCaptureClient = client.GetService().unwrap();
        client.Start().unwrap();
        let mut samples = Vec::new();
        while !stop.load(std::sync::atomic::Ordering::Acquire) {
            std::thread::sleep(std::time::Duration::from_millis(5));
            while capture.GetNextPacketSize().unwrap() > 0 {
                let mut data = std::ptr::null_mut();
                let mut frames = 0;
                let mut flags = 0;
                capture
                    .GetBuffer(&mut data, &mut frames, &mut flags, None, None)
                    .unwrap();
                if flags & AUDCLNT_BUFFERFLAGS_SILENT.0 as u32 != 0 || data.is_null() {
                    samples.extend(std::iter::repeat(0.0).take(frames as usize));
                } else {
                    samples.extend_from_slice(std::slice::from_raw_parts(
                        data.cast::<f32>(),
                        frames as usize,
                    ));
                }
                capture.ReleaseBuffer(frames).unwrap();
            }
        }
        let _ = client.Stop();
        (name, samples)
    }
}

#[cfg(not(windows))]
fn main() {}

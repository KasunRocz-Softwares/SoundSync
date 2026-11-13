use syncsound_engine::capture::{
    AudioEndpoint, CaptureConfig, CaptureController, CaptureSource, CaptureState, CaptureTarget,
    EndpointState, Packet, PacketPublisher, EXCLUSION_SCOPE_REASON,
};
use syncsound_engine::{PcmFormat, QueueStatus};

fn endpoint(id: &str, default: bool) -> AudioEndpoint {
    AudioEndpoint {
        id: id.into(),
        name: "Identical display name".into(),
        is_default: default,
        state: EndpointState::Active,
        mix_format: Some(PcmFormat {
            sample_rate: 48_000,
            channels: 2,
        }),
        bits_per_sample: Some(32),
        channel_mask: None,
        connection_type: None,
    }
}

#[test]
fn capture_default_follows_but_explicit_source_stays_pinned() {
    let mut default = CaptureController::new(CaptureConfig::default());
    let mut pinned = CaptureController::new(CaptureConfig {
        source: CaptureSource::Endpoint("a".into()),
        ..CaptureConfig::default()
    });
    let initial = [endpoint("a", true), endpoint("b", false)];
    assert_eq!(
        default.reconcile(&initial),
        Some(CaptureTarget::Endpoint("a".into()))
    );
    assert_eq!(
        pinned.reconcile(&initial),
        Some(CaptureTarget::Endpoint("a".into()))
    );
    default.started();
    pinned.started();
    let changed = [endpoint("b", true), endpoint("a", false)];
    assert_eq!(
        default.reconcile(&changed),
        Some(CaptureTarget::Endpoint("b".into()))
    );
    assert_eq!(pinned.reconcile(&changed), None);
    assert_eq!(
        pinned.state(),
        &CaptureState::Capturing(CaptureTarget::Endpoint("a".into()))
    );
}

#[test]
fn capture_ids_survive_reordering_and_missing_pin_never_rebinds_by_name() {
    let mut control = CaptureController::new(CaptureConfig {
        source: CaptureSource::Endpoint("b".into()),
        ..CaptureConfig::default()
    });
    assert_eq!(
        control.reconcile(&[endpoint("a", true), endpoint("b", false)]),
        Some(CaptureTarget::Endpoint("b".into()))
    );
    control.started();
    assert_eq!(
        control.reconcile(&[endpoint("b", false), endpoint("a", true)]),
        None
    );
    assert_eq!(control.reconcile(&[endpoint("a", true)]), None);
    assert_eq!(
        control.state(),
        &CaptureState::Unavailable {
            endpoint_id: Some("b".into())
        }
    );
    assert_eq!(
        control.reconcile(&[endpoint("b", false)]),
        Some(CaptureTarget::Endpoint("b".into()))
    );
}

#[test]
fn capture_unavailable_default_and_inactive_pin_do_not_capture() {
    let mut control = CaptureController::new(CaptureConfig::default());
    assert_eq!(control.reconcile(&[]), None);
    assert_eq!(
        control.state(),
        &CaptureState::Unavailable { endpoint_id: None }
    );
    let mut inactive = endpoint("a", true);
    inactive.state = EndpointState::Unavailable;
    assert_eq!(control.reconcile(&[inactive]), None);
    assert_eq!(
        control.state(),
        &CaptureState::Unavailable { endpoint_id: None }
    );
}

#[test]
fn capture_feedback_guard_rechecks_on_default_transition() {
    let mut control = CaptureController::new(CaptureConfig {
        output_endpoint_ids: vec!["b".into()],
        ..CaptureConfig::default()
    });
    assert!(control.reconcile(&[endpoint("a", true)]).is_some());
    control.started();
    assert_eq!(control.reconcile(&[endpoint("b", true)]), None);
    assert_eq!(
        control.state(),
        &CaptureState::Blocked {
            reason: EXCLUSION_SCOPE_REASON.into()
        }
    );
    assert!(control.reconcile(&[endpoint("a", true)]).is_some());
}

#[test]
fn capture_process_exclusion_is_explicitly_system_wide_and_targets_self() {
    let config = CaptureConfig {
        source: CaptureSource::SystemExcludingSelf,
        output_endpoint_ids: vec!["a".into()],
        ..CaptureConfig::default()
    };
    assert_eq!(config.excluded_process_id(), Some(std::process::id()));
    let mut control = CaptureController::new(config);
    assert_eq!(
        control.reconcile(&[]),
        Some(CaptureTarget::SystemExcludingSelf)
    );
    control.started();
    control.failed("Process exclusion activation unavailable");
    assert!(matches!(control.state(), CaptureState::Recoverable { .. }));
    // A failed activation can only retry the same scope, never endpoint fallback.
    assert_eq!(
        control.reconcile(&[endpoint("a", true)]),
        Some(CaptureTarget::SystemExcludingSelf)
    );
}

#[test]
fn capture_invalidation_retries_same_pin_and_stop_is_explicit() {
    let mut control = CaptureController::new(CaptureConfig {
        source: CaptureSource::Endpoint("a".into()),
        ..CaptureConfig::default()
    });
    control.reconcile(&[endpoint("a", true)]);
    control.started();
    control.failed("AUDCLNT_E_DEVICE_INVALIDATED");
    assert!(matches!(control.state(), CaptureState::Recoverable { .. }));
    assert_eq!(control.reconcile(&[endpoint("b", true)]), None);
    assert_eq!(
        control.state(),
        &CaptureState::Unavailable {
            endpoint_id: Some("a".into())
        }
    );
    control.stop();
    assert_eq!(control.state(), &CaptureState::Stopped);
}

fn config() -> CaptureConfig {
    CaptureConfig {
        format: PcmFormat {
            sample_rate: 48_000,
            channels: 1,
        },
        max_block_frames: 2,
        queue_blocks: 2,
        ..CaptureConfig::default()
    }
}

#[test]
fn capture_packets_split_into_bounded_float_blocks_with_qpc_timestamps() {
    let (mut writer, mut reader) = PacketPublisher::new(&config()).unwrap();
    let data: Vec<_> = [0.25f32, -0.5, 1.0]
        .into_iter()
        .flat_map(f32::to_le_bytes)
        .collect();
    assert_eq!(
        writer.publish(Packet {
            data: &data,
            frames: 3,
            device_position: 100,
            qpc_100ns: 10_000_000,
            ..Packet::default()
        }),
        QueueStatus::Ok
    );
    let mut out = [0.0; 2];
    assert_eq!(reader.pop_into(&mut out), Ok((48_000, 2)));
    assert_eq!(out, [0.25, -0.5]);
    assert_eq!(reader.pop_into(&mut out), Ok((48_002, 1)));
    assert_eq!(out[0], 1.0);
    // Normal QPC jitter must not produce discontinuities between packets.
    assert_eq!(
        writer.publish(Packet {
            frames: 1,
            silent: true,
            device_position: 103,
            qpc_100ns: 10_000_900,
            ..Packet::default()
        }),
        QueueStatus::Ok
    );
    assert_eq!(reader.pop_into(&mut out), Ok((48_003, 1)));
    assert_eq!(out[0], 0.0);
}

#[test]
fn capture_discontinuity_bad_timestamp_and_full_queue_discard_stale_audio() {
    let (mut writer, mut reader) = PacketPublisher::new(&config()).unwrap();
    writer.publish(Packet {
        frames: 2,
        silent: true,
        ..Packet::default()
    });
    writer.publish(Packet {
        frames: 1,
        silent: true,
        discontinuity: true,
        qpc_100ns: 20_000_000,
        ..Packet::default()
    });
    let mut out = [1.0; 2];
    assert_eq!(reader.pop_into(&mut out), Ok((96_000, 1)));
    writer.publish(Packet {
        frames: 2,
        silent: true,
        timestamp_error: true,
        ..Packet::default()
    });
    assert_eq!(reader.pop_into(&mut out), Err(QueueStatus::Empty));
    assert_eq!(
        writer.publish(Packet {
            frames: 5,
            silent: true,
            ..Packet::default()
        }),
        QueueStatus::Full
    );
    assert_eq!(reader.pop_into(&mut out), Err(QueueStatus::Empty));
}

#[test]
fn capture_malformed_packets_and_invalid_configuration_are_rejected() {
    let (mut writer, mut reader) = PacketPublisher::new(&config()).unwrap();
    assert_eq!(
        writer.publish(Packet {
            data: &[0; 3],
            frames: 1,
            ..Packet::default()
        }),
        QueueStatus::InvalidFormat
    );
    assert_eq!(reader.pop_into(&mut [0.0; 2]), Err(QueueStatus::Empty));
    assert!(PacketPublisher::new(&CaptureConfig {
        max_block_frames: 0,
        ..config()
    })
    .is_err());
}

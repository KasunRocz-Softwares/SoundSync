//! Control-plane validation. Every untrusted UI value passes through here before
//! the engine is touched; a rejected command never mutates engine state.
use crate::capture::{AudioEndpoint, CaptureSource};
use crate::render::{AudioMode, MAX_ENABLED_OUTPUTS, MAX_OUTPUT_DELAY_MS};
use serde::Serialize;
use std::time::{Duration, Instant};

pub const MAX_ENDPOINT_ID_LEN: usize = 512;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, thiserror::Error)]
#[error("{message}")]
pub struct CommandError {
    pub code: String,
    pub message: String,
}

impl CommandError {
    pub fn new(code: &str, message: impl Into<String>) -> Self {
        Self {
            code: code.into(),
            message: message.into(),
        }
    }
}

pub fn validate_endpoint_id<'a>(
    id: &str,
    endpoints: &'a [AudioEndpoint],
) -> Result<&'a AudioEndpoint, CommandError> {
    if id.is_empty() || id.len() > MAX_ENDPOINT_ID_LEN || id.chars().any(char::is_control) {
        return Err(CommandError::new("invalid_id", "Endpoint ID is malformed"));
    }
    endpoints.iter().find(|e| e.id == id).ok_or_else(|| {
        CommandError::new(
            "unknown_device",
            "Device not found; refresh the device list",
        )
    })
}

pub fn validate_volume(volume: f64) -> Result<f32, CommandError> {
    if !volume.is_finite() || !(0.0..=1.0).contains(&volume) {
        return Err(CommandError::new(
            "invalid_volume",
            "Volume must be between 0 and 1",
        ));
    }
    Ok(volume as f32)
}

pub fn validate_delay(delay_ms: i64) -> Result<u32, CommandError> {
    if !(0..=i64::from(MAX_OUTPUT_DELAY_MS)).contains(&delay_ms) {
        return Err(CommandError::new(
            "invalid_delay",
            format!("Delay must be between 0 and {MAX_OUTPUT_DELAY_MS} ms"),
        ));
    }
    Ok(delay_ms as u32)
}

pub fn validate_enable(id: &str, enabled: &[String]) -> Result<(), CommandError> {
    if !enabled.iter().any(|e| e == id) && enabled.len() >= MAX_ENABLED_OUTPUTS {
        return Err(CommandError::new(
            "too_many_outputs",
            format!("At most {MAX_ENABLED_OUTPUTS} outputs can be enabled at once"),
        ));
    }
    Ok(())
}

pub fn parse_mode(mode: &str) -> Result<AudioMode, CommandError> {
    match mode {
        "gaming" => Ok(AudioMode::Gaming),
        "balanced" => Ok(AudioMode::Balanced),
        "synchronized" => Ok(AudioMode::Synchronized),
        _ => Err(CommandError::new("invalid_mode", "Unknown audio mode")),
    }
}

/// `kind` is `default`, `endpoint` (requires a known endpoint ID) or `system_excluding_self`.
pub fn parse_source(
    kind: &str,
    endpoint_id: Option<&str>,
    endpoints: &[AudioEndpoint],
) -> Result<CaptureSource, CommandError> {
    match (kind, endpoint_id) {
        ("default", None) => Ok(CaptureSource::DefaultRender),
        ("system_excluding_self", None) => Ok(CaptureSource::SystemExcludingSelf),
        ("endpoint", Some(id)) => {
            validate_endpoint_id(id, endpoints)?;
            Ok(CaptureSource::Endpoint(id.into()))
        }
        _ => Err(CommandError::new(
            "invalid_source",
            "Invalid capture source selection",
        )),
    }
}

/// Rate limiter for telemetry pulls/pushes.
#[derive(Clone, Copy, Debug)]
pub struct Throttle {
    interval: Duration,
    last: Option<Instant>,
}

impl Throttle {
    pub fn new(interval: Duration) -> Self {
        Self {
            interval,
            last: None,
        }
    }
    pub fn ready(&mut self, now: Instant) -> bool {
        if self
            .last
            .is_some_and(|last| now.saturating_duration_since(last) < self.interval)
        {
            return false;
        }
        self.last = Some(now);
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::capture::EndpointState;

    fn endpoints() -> Vec<AudioEndpoint> {
        ["{0.0.0.00000000}.{a}", "{0.0.0.00000000}.{b}"]
            .iter()
            .map(|id| AudioEndpoint {
                id: (*id).into(),
                name: "Speakers".into(),
                is_default: false,
                state: EndpointState::Active,
                mix_format: None,
                bits_per_sample: None,
                channel_mask: None,
                connection_type: None,
                bus: None,
            })
            .collect()
    }

    #[test]
    fn rejects_malformed_and_unknown_ids() {
        let list = endpoints();
        assert_eq!(
            validate_endpoint_id("", &list).unwrap_err().code,
            "invalid_id"
        );
        assert_eq!(
            validate_endpoint_id(&"x".repeat(513), &list)
                .unwrap_err()
                .code,
            "invalid_id"
        );
        assert_eq!(
            validate_endpoint_id("a\u{0}", &list).unwrap_err().code,
            "invalid_id"
        );
        // Friendly names are never accepted as identifiers.
        assert_eq!(
            validate_endpoint_id("Speakers", &list).unwrap_err().code,
            "unknown_device"
        );
        assert!(validate_endpoint_id("{0.0.0.00000000}.{a}", &list).is_ok());
    }

    #[test]
    fn rejects_volume_and_delay_outside_bounds() {
        for bad in [-0.01, 1.01, f64::NAN, f64::INFINITY] {
            assert!(validate_volume(bad).is_err(), "{bad}");
        }
        assert_eq!(validate_volume(0.5), Ok(0.5));
        assert!(validate_delay(-1).is_err());
        assert!(validate_delay(1_001).is_err());
        assert_eq!(validate_delay(1_000), Ok(1_000));
    }

    #[test]
    fn enforces_eight_output_limit_but_allows_reenable() {
        let enabled: Vec<String> = (0..8).map(|i| i.to_string()).collect();
        assert_eq!(
            validate_enable("8", &enabled).unwrap_err().code,
            "too_many_outputs"
        );
        assert!(validate_enable("3", &enabled).is_ok());
        assert!(validate_enable("x", &enabled[..7]).is_ok());
    }

    #[test]
    fn rejects_invalid_mode_and_source_transitions() {
        let list = endpoints();
        assert_eq!(parse_mode("synchronized"), Ok(AudioMode::Synchronized));
        assert!(parse_mode("Synchronized").is_err());
        assert!(parse_mode("zero-latency").is_err());
        assert_eq!(
            parse_source("default", None, &list),
            Ok(CaptureSource::DefaultRender)
        );
        assert!(parse_source("default", Some("{0.0.0.00000000}.{a}"), &list).is_err());
        assert!(parse_source("endpoint", None, &list).is_err());
        assert!(parse_source("endpoint", Some("missing"), &list).is_err());
        assert!(parse_source("system", None, &list).is_err());
        assert_eq!(
            parse_source("system_excluding_self", None, &list),
            Ok(CaptureSource::SystemExcludingSelf)
        );
    }

    #[test]
    fn throttle_limits_snapshot_rate() {
        let start = Instant::now();
        let mut throttle = Throttle::new(Duration::from_millis(100));
        assert!(throttle.ready(start));
        assert!(!throttle.ready(start + Duration::from_millis(50)));
        assert!(throttle.ready(start + Duration::from_millis(100)));
        assert!(!throttle.ready(start + Duration::from_millis(150)));
    }
}

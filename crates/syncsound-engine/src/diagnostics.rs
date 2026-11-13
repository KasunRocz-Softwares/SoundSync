//! Redacted diagnostic bundle. Opaque Windows endpoint IDs (which embed device
//! GUIDs) are replaced everywhere with short stable hashes.
use crate::routing::EngineSnapshot;
use serde_json::{json, Value};

pub fn redact_id(id: &str) -> String {
    // FNV-1a: stable across runs, not reversible to the original GUID string.
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for byte in id.bytes() {
        hash ^= u64::from(byte);
        hash = hash.wrapping_mul(0x0100_0000_01b3);
    }
    format!("ep-{:08x}", hash as u32)
}

fn redact(value: &mut Value, ids: &[String]) {
    match value {
        Value::String(text) => {
            for id in ids {
                if text.contains(id.as_str()) {
                    *text = text.replace(id.as_str(), &redact_id(id));
                }
            }
        }
        Value::Array(items) => items.iter_mut().for_each(|v| redact(v, ids)),
        Value::Object(map) => map.values_mut().for_each(|v| redact(v, ids)),
        _ => {}
    }
}

pub fn build_bundle(snapshot: &EngineSnapshot, app_version: &str, os: &str) -> Value {
    let mut ids: Vec<String> = snapshot.endpoints.iter().map(|e| e.id.clone()).collect();
    ids.extend(snapshot.outputs.iter().map(|o| o.endpoint_id.clone()));
    ids.sort_by_key(|id| std::cmp::Reverse(id.len()));
    ids.dedup();
    let mut bundle = json!({
        "kind": "syncsound-diagnostics",
        "format_version": 1,
        "app_version": app_version,
        "engine_version": env!("CARGO_PKG_VERSION"),
        "os": os,
        "note": "Latency values are estimates from Windows-reported buffers; not acoustic measurements.",
        "snapshot": snapshot,
    });
    redact(&mut bundle, &ids);
    bundle
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::capture::{AudioEndpoint, CaptureState, EndpointState};
    use crate::PcmFormat;

    #[test]
    fn removes_endpoint_ids_from_every_field() {
        let id = "{0.0.0.00000000}.{8f2a-secret-guid}";
        let mut snapshot = EngineSnapshot::idle(PcmFormat {
            sample_rate: 48_000,
            channels: 2,
        });
        snapshot.endpoints.push(AudioEndpoint {
            id: id.into(),
            name: "Speakers".into(),
            is_default: true,
            state: EndpointState::Active,
            mix_format: None,
            bits_per_sample: None,
            channel_mask: None,
            connection_type: None,
            bus: None,
        });
        snapshot.capture_state = CaptureState::Unavailable {
            endpoint_id: Some(id.into()),
        };
        let text = build_bundle(&snapshot, "0.1.0", "windows").to_string();
        assert!(!text.contains("secret-guid"), "{text}");
        assert!(text.contains(&redact_id(id)));
        assert!(text.contains("Speakers"));
        assert_eq!(redact_id(id), redact_id(id));
    }
}

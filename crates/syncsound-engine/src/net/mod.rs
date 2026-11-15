//! Network speakers: shared types and identifiers for devices that play audio
//! over the local network instead of a local WASAPI render endpoint.

pub mod access;
pub mod convert;
pub mod http;
pub mod link;
pub mod server;
pub mod worker;

use crate::capture::{AudioEndpoint, EndpointState};
use crate::PcmFormat;
use serde::{Deserialize, Serialize};

/// Prefix that distinguishes network endpoint ids from WASAPI endpoint ids.
pub const NET_PREFIX: &str = "net:";
/// Sample rate of the PCM stream sent to network speakers.
pub const STREAM_RATE: u32 = 48_000;
/// Frames per network audio chunk (20 ms at `STREAM_RATE`).
pub const CHUNK_FRAMES: usize = 960;
/// Default TCP port for the network speaker server.
pub const DEFAULT_PORT: u16 = 4877;

/// A network speaker that has been paired and is available for routing.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct NetworkDevice {
    pub key: String,
    pub name: String,
}

/// A network speaker that has announced itself and awaits user approval.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct PendingDevice {
    pub key: String,
    pub name: String,
    pub address: String,
}

/// Current state of the network speaker server, as shown to the user.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct NetworkStatus {
    pub enabled: bool,
    pub url: Option<String>,
    pub qr_svg: Option<String>,
    pub pending: Vec<PendingDevice>,
    pub error: Option<String>,
}

/// Builds the endpoint id used for a network speaker with the given key.
pub fn endpoint_id(key: &str) -> String {
    format!("{NET_PREFIX}{key}")
}

/// Returns the device key for a network endpoint id, or `None` when the id
/// does not belong to a network speaker.
pub fn key_of(id: &str) -> Option<&str> {
    id.strip_prefix(NET_PREFIX)
}

/// Makes the `net:` entries of `endpoints` match `devices`: one entry per
/// allowed device (Active while `connected(key)`), none for anything else.
/// WASAPI endpoints are left untouched and keep their order.
pub fn merge_network_endpoints(
    endpoints: &mut Vec<AudioEndpoint>,
    devices: &[NetworkDevice],
    connected: &dyn Fn(&str) -> bool,
) {
    endpoints.retain(|e| key_of(&e.id).is_none());
    endpoints.extend(devices.iter().map(|d| AudioEndpoint {
        id: endpoint_id(&d.key),
        name: d.name.clone(),
        is_default: false,
        state: if connected(&d.key) {
            EndpointState::Active
        } else {
            EndpointState::Unavailable
        },
        mix_format: Some(PcmFormat {
            sample_rate: STREAM_RATE,
            channels: 2,
        }),
        bits_per_sample: Some(16),
        channel_mask: None,
        connection_type: Some("Network".into()),
        bus: Some("NETWORK".into()),
    }));
}

/// The join QR code for `url` as an SVG document.
pub fn qr_svg(url: &str) -> Option<String> {
    use qrcode::render::svg::Color;
    let code = qrcode::QrCode::new(url.as_bytes()).ok()?;
    Some(
        code.render::<Color>()
            .min_dimensions(168, 168)
            .dark_color(Color("#0b0f16"))
            .light_color(Color("#ffffff"))
            .build(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn wasapi(id: &str) -> AudioEndpoint {
        AudioEndpoint {
            id: id.into(),
            name: "Speakers".into(),
            is_default: true,
            state: EndpointState::Active,
            mix_format: None,
            bits_per_sample: None,
            channel_mask: None,
            connection_type: None,
            bus: None,
        }
    }

    fn device(key: &str, name: &str) -> NetworkDevice {
        NetworkDevice { key: key.into(), name: name.into() }
    }

    #[test]
    fn merges_network_endpoints() {
        let mut endpoints = vec![wasapi("{0.0.0}.{a}")];
        let devices = [device("k1", "Kitchen phone"), device("k2", "TV")];
        let connected = |key: &str| key == "k1";
        merge_network_endpoints(&mut endpoints, &devices, &connected);
        // Merging twice (refresh, then a server event) never duplicates.
        merge_network_endpoints(&mut endpoints, &devices, &connected);
        assert_eq!(endpoints.len(), 3);
        assert_eq!(endpoints[0].id, "{0.0.0}.{a}");
        let k1: Vec<_> = endpoints.iter().filter(|e| e.id == "net:k1").collect();
        assert_eq!(k1.len(), 1);
        let k1 = k1[0];
        assert_eq!(k1.name, "Kitchen phone");
        assert_eq!(k1.connection_type.as_deref(), Some("Network"));
        assert_eq!(k1.bus.as_deref(), Some("NETWORK"));
        assert!(!k1.is_default);
        assert_eq!(k1.state, EndpointState::Active);
        assert_eq!(k1.mix_format, Some(PcmFormat { sample_rate: STREAM_RATE, channels: 2 }));
        assert_eq!(k1.bits_per_sample, Some(16));
        let k2 = endpoints.iter().find(|e| e.id == "net:k2").unwrap();
        assert_eq!(k2.state, EndpointState::Unavailable);

        // Disconnect: same entry, now unavailable; a rename shows up.
        let renamed = [device("k1", "Phone"), device("k2", "TV")];
        merge_network_endpoints(&mut endpoints, &renamed, &|_: &str| false);
        assert_eq!(endpoints.len(), 3);
        let k1 = endpoints.iter().find(|e| e.id == "net:k1").unwrap();
        assert_eq!(k1.state, EndpointState::Unavailable);
        assert_eq!(k1.name, "Phone");

        // A device no longer allowed disappears; WASAPI endpoints stay.
        merge_network_endpoints(&mut endpoints, &renamed[1..], &|_: &str| false);
        assert_eq!(endpoints.len(), 2);
        assert!(endpoints.iter().all(|e| e.id != "net:k1"));
    }

    #[test]
    fn renders_qr_svg() {
        let svg = qr_svg("http://192.168.1.20:4877/?join=00ff").unwrap();
        assert!(svg.contains("<svg"));
        assert!(svg.contains("#0b0f16"));
    }

    #[test]
    fn ids_round_trip() {
        assert_eq!(endpoint_id("ab12"), "net:ab12");
        assert_eq!(key_of("net:ab12"), Some("ab12"));
        assert_eq!(key_of("{0.0.0.00000000}.{guid}"), None);
    }
}

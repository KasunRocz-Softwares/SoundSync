//! Network speakers: shared types and identifiers for devices that play audio
//! over the local network instead of a local WASAPI render endpoint.

pub mod access;
pub mod convert;

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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ids_round_trip() {
        assert_eq!(endpoint_id("ab12"), "net:ab12");
        assert_eq!(key_of("net:ab12"), Some("ab12"));
        assert_eq!(key_of("{0.0.0.00000000}.{guid}"), None);
    }
}

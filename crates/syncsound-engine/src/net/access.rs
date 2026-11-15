//! Access rules for network speakers: which clients may connect, single-use
//! QR join tokens, and friendly device names guessed from User-Agent strings.

use std::net::IpAddr;
use std::time::{Duration, Instant};

use rand::rngs::OsRng;
use rand::RngCore;

/// How long a QR join token stays valid after it is first shown.
const TOKEN_TTL: Duration = Duration::from_secs(600);

/// True for private, loopback and link-local addresses only.
pub fn is_local(addr: IpAddr) -> bool {
    match addr {
        IpAddr::V4(v4) => v4.is_private() || v4.is_loopback() || v4.is_link_local(),
        IpAddr::V6(v6) => {
            if let Some(v4) = v6.to_ipv4_mapped() {
                return is_local(IpAddr::V4(v4));
            }
            let first = v6.segments()[0];
            v6.is_loopback() || (first & 0xfe00) == 0xfc00 || (first & 0xffc0) == 0xfe80
        }
    }
}

/// One-time QR join token, replaced after each use or after expiry.
#[derive(Default)]
pub struct JoinTokens {
    current: Option<(String, Instant)>,
}

impl JoinTokens {
    pub fn new() -> Self {
        Self::default()
    }

    /// Returns the live token, making a new one if there is none or it expired.
    pub fn current(&mut self, now: Instant) -> String {
        if let Some((token, made)) = &self.current {
            if now.saturating_duration_since(*made) <= TOKEN_TTL {
                return token.clone();
            }
        }
        let token = new_token();
        self.current = Some((token.clone(), now));
        token
    }

    /// True once for the live token; the token is then replaced.
    pub fn redeem(&mut self, token: &str, now: Instant) -> bool {
        let valid = match &self.current {
            Some((cur, made)) => cur == token && now.saturating_duration_since(*made) <= TOKEN_TTL,
            None => false,
        };
        if valid {
            self.current = None;
        }
        valid
    }
}

/// Friendly device name from a browser User-Agent, e.g. "iPhone (Safari)".
pub fn guess_name(user_agent: &str) -> String {
    let ua = user_agent;
    let has = |s: &str| ua.contains(s);
    // Order matters: TV user agents also contain "Android" or "Linux".
    if has("Tizen") || has("SMART-TV") {
        return "Samsung TV".into();
    }
    if has("Web0S") || has("webOS") {
        return "LG TV".into();
    }
    if has("Android") && (has("BRAVIA") || has("Android TV") || has("AFT") || has("; TV")) {
        return "Android TV".into();
    }
    let browser = if has("Edg/") || has("EdgA/") || has("EdgiOS/") {
        "Edge"
    } else if has("OPR/") {
        "Opera"
    } else if has("Firefox/") || has("FxiOS/") {
        "Firefox"
    } else if has("Chrome/") || has("CriOS/") {
        "Chrome"
    } else if has("Safari/") {
        "Safari"
    } else {
        ""
    };
    let device = if has("iPhone") {
        "iPhone"
    } else if has("iPad") {
        "iPad"
    } else if has("Android") {
        if has("Mobile") { "Android phone" } else { "Android tablet" }
    } else if has("Windows") {
        "Windows PC"
    } else if has("Macintosh") {
        "Mac"
    } else if has("CrOS") {
        "Chromebook"
    } else if has("Linux") {
        "Linux PC"
    } else {
        ""
    };
    match (device, browser) {
        ("", _) => "Browser".into(),
        (d, "") => d.into(),
        (d, b) => format!("{d} ({b})"),
    }
}

/// A device key is 16 to 64 characters of `[0-9a-f-]`.
pub fn valid_key(key: &str) -> bool {
    (16..=64).contains(&key.len())
        && key.bytes().all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b) || b == b'-')
}

fn new_token() -> String {
    let mut bytes = [0u8; 16];
    OsRng.fill_bytes(&mut bytes);
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_public_addresses() {
        for ok in ["192.168.1.5", "10.0.0.2", "172.20.1.1", "169.254.3.4", "127.0.0.1", "fe80::1", "fd00::1", "::ffff:192.168.1.5"] {
            assert!(is_local(ok.parse().unwrap()), "{ok} should be local");
        }
        for bad in ["8.8.8.8", "172.32.0.1", "2001:db8::1", "::ffff:8.8.8.8"] {
            assert!(!is_local(bad.parse().unwrap()), "{bad} should be refused");
        }
    }

    #[test]
    fn qr_token_is_single_use_and_expires() {
        let t0 = Instant::now();
        let mut jt = JoinTokens::new();
        let a = jt.current(t0);
        assert_eq!(a.len(), 32);
        assert_eq!(jt.current(t0), a);
        assert!(jt.redeem(&a, t0));
        assert!(!jt.redeem(&a, t0));
        let b = jt.current(t0);
        assert_ne!(a, b);
        assert!(!jt.redeem(&b, t0 + Duration::from_secs(601)));
        let c = jt.current(t0 + Duration::from_secs(601));
        assert_ne!(b, c);
        assert!(!jt.redeem("wrong", t0 + Duration::from_secs(601)));
        assert!(jt.redeem(&c, t0 + Duration::from_secs(700)));
    }

    #[test]
    fn guesses_common_device_names() {
        let cases = [
            ("Mozilla/5.0 (iPhone; CPU iPhone OS 17_0 like Mac OS X) AppleWebKit/605.1.15 (KHTML, like Gecko) Version/17.0 Mobile/15E148 Safari/604.1", "iPhone (Safari)"),
            ("Mozilla/5.0 (Linux; Android 14; Pixel 8) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/120.0.0.0 Mobile Safari/537.36", "Android phone (Chrome)"),
            ("Mozilla/5.0 (SMART-TV; Linux; Tizen 6.0) AppleWebKit/537.36 (KHTML, like Gecko) 76.0.3809.146/6.0 TV Safari/537.36", "Samsung TV"),
            ("Mozilla/5.0 (Web0S; Linux/SmartTV) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/79.0.3945.79 Safari/537.36 WebAppManager", "LG TV"),
            ("Mozilla/5.0 (Linux; Android 9; BRAVIA 4K GB Build/PTT1.190515.001) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/120.0.0.0 Safari/537.36", "Android TV"),
            ("Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/120.0.0.0 Safari/537.36 Edg/120.0.0.0", "Windows PC (Edge)"),
            ("Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/605.1.15 (KHTML, like Gecko) Version/17.0 Safari/605.1.15", "Mac (Safari)"),
            ("curl/8.0", "Browser"),
            ("", "Browser"),
        ];
        for (ua, want) in cases {
            assert_eq!(guess_name(ua), want, "ua: {ua}");
        }
    }

    #[test]
    fn validates_keys() {
        assert!(valid_key(&"a1".repeat(16)));
        assert!(valid_key("0123456789abcdef-0123"));
        assert!(!valid_key(""));
        assert!(!valid_key("../x"));
        assert!(!valid_key(&"a".repeat(100)));
        assert!(!valid_key(&"A".repeat(32)));
    }
}

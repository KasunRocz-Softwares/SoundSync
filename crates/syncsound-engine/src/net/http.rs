//! Minimal HTTP/1.1 request reading for the network speaker server: only the
//! request line and headers, enough to serve a few embedded files and accept
//! a WebSocket upgrade. Bodies are never read.

use std::io::{self, Read};
use std::net::TcpStream;
use std::time::Instant;

/// Largest request head the server accepts.
const MAX_HEAD: usize = 8 * 1024;

/// A parsed request head.
#[derive(Debug, Default, PartialEq)]
pub struct Request {
    pub method: String,
    /// Path without the query string, e.g. `/` for `/?join=ab`.
    pub path: String,
    /// Header names are lower-cased; values are trimmed.
    pub headers: Vec<(String, String)>,
}

impl Request {
    /// First header with this (lower-case) name.
    pub fn header(&self, name: &str) -> Option<&str> {
        self.headers.iter().find(|(n, _)| n == name).map(|(_, v)| v.as_str())
    }

    /// True when the request asks to upgrade to a WebSocket.
    pub fn is_websocket_upgrade(&self) -> bool {
        let upgrade = self.header("upgrade").is_some_and(|v| v.eq_ignore_ascii_case("websocket"));
        let connection = self
            .header("connection")
            .is_some_and(|v| v.split(',').any(|t| t.trim().eq_ignore_ascii_case("upgrade")));
        upgrade && connection && self.header("sec-websocket-key").is_some()
    }
}

/// Reads the request head, which must arrive completely before `deadline`
/// (a client that sends one byte at a time cannot hold the thread longer).
pub fn read_head_before(stream: &mut TcpStream, deadline: Instant) -> io::Result<Vec<u8>> {
    read_head_with(stream, |s| {
        let left = deadline.saturating_duration_since(Instant::now());
        if left.is_zero() {
            return Err(io::Error::new(io::ErrorKind::TimedOut, "request head too slow"));
        }
        s.set_read_timeout(Some(left))
    })
}

/// Reads bytes until the end of the request head (`\r\n\r\n`). Reads one
/// byte at a time so nothing past the head is consumed; `before_read` runs
/// before every read.
fn read_head_with<R: Read>(stream: &mut R, mut before_read: impl FnMut(&mut R) -> io::Result<()>) -> io::Result<Vec<u8>> {
    let mut head = Vec::with_capacity(512);
    let mut byte = [0u8; 1];
    while !head.ends_with(b"\r\n\r\n") {
        if head.len() >= MAX_HEAD {
            return Err(io::Error::new(io::ErrorKind::InvalidData, "request head too large"));
        }
        before_read(stream)?;
        match stream.read(&mut byte)? {
            0 => return Err(io::Error::new(io::ErrorKind::UnexpectedEof, "connection closed")),
            _ => head.push(byte[0]),
        }
    }
    Ok(head)
}

/// Parses a request head. Returns `None` for anything malformed.
pub fn parse(head: &[u8]) -> Option<Request> {
    let text = std::str::from_utf8(head).ok()?;
    let mut lines = text.split("\r\n");
    let mut parts = lines.next()?.split(' ');
    let method = parts.next()?.to_string();
    let target = parts.next()?;
    if !parts.next()?.starts_with("HTTP/1.") || method.is_empty() || !target.starts_with('/') {
        return None;
    }
    let path = target.split(['?', '#']).next().unwrap_or("/").to_string();
    let mut headers = Vec::new();
    for line in lines.take_while(|l| !l.is_empty()) {
        let (name, value) = line.split_once(':')?;
        headers.push((name.trim().to_ascii_lowercase(), value.trim().to_string()));
    }
    Some(Request { method, path, headers })
}

/// True when a `Host` header names this PC the way a LAN device reaches it:
/// an IP literal (IPv4 or bracketed IPv6), `localhost`, a single-label name
/// or a `.local` name, with an optional port. Anything else (such as
/// `evil.example.com` resolving to this PC) is DNS rebinding and is refused.
pub fn host_allowed(host: &str) -> bool {
    let host = host.trim();
    let name = if let Some(rest) = host.strip_prefix('[') {
        // [v6] or [v6]:port
        let Some((inner, after)) = rest.split_once(']') else { return false };
        if !(after.is_empty() || after.strip_prefix(':').is_some_and(is_port)) {
            return false;
        }
        return inner.parse::<std::net::Ipv6Addr>().is_ok();
    } else {
        match host.split_once(':') {
            Some((name, port)) if is_port(port) => name,
            Some(_) => return false,
            None => host,
        }
    };
    if name.is_empty() || !name.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'.') {
        return false;
    }
    let name = name.to_ascii_lowercase();
    name.parse::<std::net::Ipv4Addr>().is_ok()
        || name == "localhost"
        || !name.contains('.')
        || (name.ends_with(".local") && name.len() > ".local".len())
}

fn is_port(p: &str) -> bool {
    !p.is_empty() && p.len() <= 5 && p.bytes().all(|b| b.is_ascii_digit())
}

/// True when a WebSocket upgrade comes from this server's own page: no
/// `Origin` (non-browser clients), or exactly `http://` + the `Host` header.
/// A page from any other site open in a LAN browser is refused.
pub fn origin_allowed(origin: Option<&str>, host: &str) -> bool {
    match origin {
        None => true,
        Some(origin) => origin.trim().eq_ignore_ascii_case(&format!("http://{}", host.trim())),
    }
}

/// A complete response with a body and `Connection: close`.
pub fn response(status: &str, content_type: &str, body: &[u8]) -> Vec<u8> {
    let mut out = format!(
        "HTTP/1.1 {status}\r\nContent-Type: {content_type}\r\nContent-Length: {}\r\n\
         Cache-Control: no-store\r\nX-Content-Type-Options: nosniff\r\nConnection: close\r\n\r\n",
        body.len()
    )
    .into_bytes();
    out.extend_from_slice(body);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_request_line_and_headers() {
        let head = b"GET /?join=abc HTTP/1.1\r\nHost: x\r\nUpgrade: WebSocket\r\nConnection: keep-alive, Upgrade\r\nSec-WebSocket-Key: k==\r\n\r\n";
        let req = parse(head).unwrap();
        assert_eq!(req.method, "GET");
        assert_eq!(req.path, "/");
        assert_eq!(req.header("host"), Some("x"));
        assert!(req.is_websocket_upgrade());
        assert!(parse(b"GET nopath HTTP/1.1\r\n\r\n").is_none());
        assert!(parse(b"garbage\r\n\r\n").is_none());
    }

    #[test]
    fn host_must_be_a_lan_name() {
        for ok in ["192.168.1.20:4877", "192.168.1.20", "127.0.0.1:4877", "[::1]:4877", "[fe80::1]", "localhost:4877",
            "LOCALHOST", "kasun-pc:4877", "x", "kasun-pc.local:4877", "Kasun-PC.LOCAL"]
        {
            assert!(host_allowed(ok), "{ok}");
        }
        for bad in ["evil.example.com", "evil.example.com:4877", "192.168.1.20.nip.io:4877", ".local", "", ":4877",
            "[::1", "[::1]x", "[evil]:4877", "a:b", "kasun-pc:", "local.evil.com", "a b"]
        {
            assert!(!host_allowed(bad), "{bad}");
        }
    }

    #[test]
    fn origin_must_match_host() {
        assert!(origin_allowed(None, "192.168.1.20:4877"));
        assert!(origin_allowed(Some("http://192.168.1.20:4877"), "192.168.1.20:4877"));
        assert!(origin_allowed(Some("HTTP://Kasun-PC.local:4877"), "kasun-pc.local:4877"));
        assert!(!origin_allowed(Some("http://evil.example.com"), "192.168.1.20:4877"));
        assert!(!origin_allowed(Some("http://192.168.1.20"), "192.168.1.20:4877"));
        assert!(!origin_allowed(Some("https://192.168.1.20:4877"), "192.168.1.20:4877"));
        assert!(!origin_allowed(Some("null"), "192.168.1.20:4877"));
    }

    #[test]
    fn read_head_stops_at_blank_line() {
        let mut src: &[u8] = b"GET / HTTP/1.1\r\nA: b\r\n\r\nEXTRA";
        let head = read_head_with(&mut src, |_| Ok(())).unwrap();
        assert!(head.ends_with(b"\r\n\r\n"));
        assert_eq!(src, b"EXTRA");
    }
}

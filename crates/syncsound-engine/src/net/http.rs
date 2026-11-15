//! Minimal HTTP/1.1 request reading for the network speaker server: only the
//! request line and headers, enough to serve a few embedded files and accept
//! a WebSocket upgrade. Bodies are never read.

use std::io::{self, Read};

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

/// Reads bytes until the end of the request head (`\r\n\r\n`). Reads one
/// byte at a time so nothing past the head is consumed.
pub fn read_head(stream: &mut impl Read) -> io::Result<Vec<u8>> {
    let mut head = Vec::with_capacity(512);
    let mut byte = [0u8; 1];
    while !head.ends_with(b"\r\n\r\n") {
        if head.len() >= MAX_HEAD {
            return Err(io::Error::new(io::ErrorKind::InvalidData, "request head too large"));
        }
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
    fn read_head_stops_at_blank_line() {
        let mut src: &[u8] = b"GET / HTTP/1.1\r\nA: b\r\n\r\nEXTRA";
        let head = read_head(&mut src).unwrap();
        assert!(head.ends_with(b"\r\n\r\n"));
        assert_eq!(src, b"EXTRA");
    }
}

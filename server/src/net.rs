//! Minimal HTTP/1.1 static file serving and RFC 6455 WebSocket framing on std.

use std::io::{self, BufReader, Read, Write};
use std::net::TcpStream;
use std::path::Path;

pub struct Request {
    pub path: String,
    pub ws_key: Option<String>,
}

/// Reads the request head (up to 8 KiB) and returns the path and websocket key.
pub fn read_request(r: &mut BufReader<TcpStream>) -> io::Result<Request> {
    let mut head = Vec::with_capacity(1024);
    let mut byte = [0u8; 1];
    while !head.ends_with(b"\r\n\r\n") {
        if r.read(&mut byte)? == 0 || head.len() > 8192 {
            return Err(io::ErrorKind::InvalidData.into());
        }
        head.push(byte[0]);
    }
    let text = String::from_utf8_lossy(&head);
    let mut lines = text.split("\r\n");
    let mut first = lines.next().unwrap_or("").split(' ');
    let _method = first.next();
    let path = first.next().unwrap_or("/").split('?').next().unwrap_or("/").to_string();
    let mut ws_key = None;
    let mut upgrade = false;
    for line in lines {
        if let Some((k, v)) = line.split_once(':') {
            let k = k.trim().to_ascii_lowercase();
            let v = v.trim();
            if k == "sec-websocket-key" {
                ws_key = Some(v.to_string());
            } else if k == "upgrade" && v.eq_ignore_ascii_case("websocket") {
                upgrade = true;
            }
        }
    }
    Ok(Request { path, ws_key: if upgrade { ws_key } else { None } })
}

pub fn accept_websocket(s: &mut TcpStream, key: &str) -> io::Result<()> {
    let mut input = key.as_bytes().to_vec();
    input.extend_from_slice(b"258EAFA5-E914-47DA-95CA-C5AB0DC85B11");
    let resp = format!(
        "HTTP/1.1 101 Switching Protocols\r\nUpgrade: websocket\r\nConnection: Upgrade\r\nSec-WebSocket-Accept: {}\r\n\r\n",
        base64(&sha1(&input))
    );
    s.write_all(resp.as_bytes())
}

pub fn serve_file(s: &mut TcpStream, root: &Path, path: &str) -> io::Result<()> {
    let rel = if path == "/" { "index.html" } else { path.trim_start_matches('/') };
    let safe = !rel.is_empty() && !rel.contains("..") && !rel.contains('\\');
    let body = if safe { std::fs::read(root.join(rel)).ok() } else { None };
    let Some(body) = body else {
        return s.write_all(b"HTTP/1.1 404 Not Found\r\nContent-Length: 9\r\nConnection: close\r\n\r\nnot found");
    };
    let ctype = match rel.rsplit('.').next() {
        Some("html") => "text/html; charset=utf-8",
        Some("js") => "text/javascript",
        Some("wasm") => "application/wasm",
        Some("css") => "text/css",
        Some("svg") => "image/svg+xml",
        _ => "application/octet-stream",
    };
    let head = format!(
        "HTTP/1.1 200 OK\r\nContent-Type: {ctype}\r\nContent-Length: {}\r\nCache-Control: no-cache\r\nConnection: close\r\n\r\n",
        body.len()
    );
    s.write_all(head.as_bytes())?;
    s.write_all(&body)
}

pub enum Frame {
    Binary(Vec<u8>),
    Ping(Vec<u8>),
    Close,
    Other,
}

const MAX_MSG: usize = 4096;

/// Reads one client frame (always masked), reassembling fragments.
pub fn read_frame(r: &mut impl Read) -> io::Result<Frame> {
    let mut msg = Vec::new();
    let mut first_op = None;
    loop {
        let mut h = [0u8; 2];
        r.read_exact(&mut h)?;
        let fin = h[0] & 0x80 != 0;
        let op = h[0] & 0x0f;
        let masked = h[1] & 0x80 != 0;
        let mut len = (h[1] & 0x7f) as u64;
        if len == 126 {
            let mut b = [0u8; 2];
            r.read_exact(&mut b)?;
            len = u16::from_be_bytes(b) as u64;
        } else if len == 127 {
            let mut b = [0u8; 8];
            r.read_exact(&mut b)?;
            len = u64::from_be_bytes(b);
        }
        if len as usize + msg.len() > MAX_MSG || !masked {
            return Err(io::ErrorKind::InvalidData.into());
        }
        let mut mask = [0u8; 4];
        r.read_exact(&mut mask)?;
        let start = msg.len();
        msg.resize(start + len as usize, 0);
        r.read_exact(&mut msg[start..])?;
        for (i, b) in msg[start..].iter_mut().enumerate() {
            *b ^= mask[i & 3];
        }
        if op >= 8 {
            // Control frames are never fragmented and may interleave.
            let payload = msg.split_off(start);
            match op {
                8 => return Ok(Frame::Close),
                9 => return Ok(Frame::Ping(payload)),
                _ => {
                    if first_op.is_none() {
                        return Ok(Frame::Other);
                    }
                    continue;
                }
            }
        }
        if first_op.is_none() {
            first_op = Some(op);
        }
        if fin {
            return Ok(if first_op == Some(2) { Frame::Binary(msg) } else { Frame::Other });
        }
    }
}

/// Wraps a payload in an unmasked server frame.
pub fn frame(op: u8, payload: &[u8]) -> Vec<u8> {
    let n = payload.len();
    let mut out = Vec::with_capacity(n + 10);
    out.push(0x80 | op);
    if n < 126 {
        out.push(n as u8);
    } else if n < 65536 {
        out.push(126);
        out.extend_from_slice(&(n as u16).to_be_bytes());
    } else {
        out.push(127);
        out.extend_from_slice(&(n as u64).to_be_bytes());
    }
    out.extend_from_slice(payload);
    out
}

fn sha1(data: &[u8]) -> [u8; 20] {
    let mut h: [u32; 5] = [0x67452301, 0xEFCDAB89, 0x98BADCFE, 0x10325476, 0xC3D2E1F0];
    let mut m = data.to_vec();
    let bitlen = (data.len() as u64) * 8;
    m.push(0x80);
    while m.len() % 64 != 56 {
        m.push(0);
    }
    m.extend_from_slice(&bitlen.to_be_bytes());
    for chunk in m.chunks(64) {
        let mut w = [0u32; 80];
        for i in 0..16 {
            w[i] = u32::from_be_bytes([chunk[4 * i], chunk[4 * i + 1], chunk[4 * i + 2], chunk[4 * i + 3]]);
        }
        for i in 16..80 {
            w[i] = (w[i - 3] ^ w[i - 8] ^ w[i - 14] ^ w[i - 16]).rotate_left(1);
        }
        let [mut a, mut b, mut c, mut d, mut e] = h;
        for (i, wi) in w.iter().enumerate() {
            let (f, k) = match i {
                0..=19 => ((b & c) | (!b & d), 0x5A827999),
                20..=39 => (b ^ c ^ d, 0x6ED9EBA1),
                40..=59 => ((b & c) | (b & d) | (c & d), 0x8F1BBCDC),
                _ => (b ^ c ^ d, 0xCA62C1D6),
            };
            let t = a.rotate_left(5).wrapping_add(f).wrapping_add(e).wrapping_add(k).wrapping_add(*wi);
            e = d;
            d = c;
            c = b.rotate_left(30);
            b = a;
            a = t;
        }
        for (hv, v) in h.iter_mut().zip([a, b, c, d, e]) {
            *hv = hv.wrapping_add(v);
        }
    }
    let mut out = [0u8; 20];
    for (i, v) in h.iter().enumerate() {
        out[4 * i..4 * i + 4].copy_from_slice(&v.to_be_bytes());
    }
    out
}

fn base64(data: &[u8]) -> String {
    const T: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut s = String::new();
    for c in data.chunks(3) {
        let n = (c[0] as u32) << 16 | (*c.get(1).unwrap_or(&0) as u32) << 8 | *c.get(2).unwrap_or(&0) as u32;
        s.push(T[(n >> 18) as usize & 63] as char);
        s.push(T[(n >> 12) as usize & 63] as char);
        s.push(if c.len() > 1 { T[(n >> 6) as usize & 63] as char } else { '=' });
        s.push(if c.len() > 2 { T[n as usize & 63] as char } else { '=' });
    }
    s
}

#[cfg(test)]
mod tests {
    #[test]
    fn handshake_accept() {
        // Cross-checked against `openssl sha1 -binary | base64`.
        let mut k = b"dGhlIHNhbXBsZSBub25jZQ==".to_vec();
        k.extend_from_slice(b"258EAFA5-E914-47DA-95CA-C5AB0DC85B11");
        assert_eq!(super::base64(&super::sha1(&k)), "s3pPLMBiTxaQ9kYGzzhZRbK+xOo=");
    }
}

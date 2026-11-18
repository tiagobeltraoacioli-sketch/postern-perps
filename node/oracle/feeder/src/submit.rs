//! Submission of signed oracle rounds to the node's ingest (`docs/API.md`, agreed with 431 in the cross-area log,
//! 2026-11-18): `POST /v1/oracle/updates` (a whole `OracleUpdate`, README section 4) and
//! `POST /v1/oracle/observations` (`{"round", "observation"}`: one signer), `Authorization: Bearer <token>` with the
//! token read from a file. UNAUDITED PROTOTYPE.
//!
//! This client speaks plain HTTP/1.1 over a LOOPBACK socket only: the node URL is refused unless it is
//! `http://127.x.y.z:port`, `http://[::1]:port` or `http://localhost:port`. Reason: the bearer token and the dev
//! stack never leave the host; a feeder on another machine than its sequencer needs an authenticated, encrypted
//! channel that this prototype does not have (README section 9).

use std::fs;
use std::io::{Read, Write};
use std::net::{IpAddr, SocketAddr, TcpStream, ToSocketAddrs};
use std::path::Path;
use std::time::Duration;

pub const UPDATES_PATH: &str = "/v1/oracle/updates";
pub const OBSERVATIONS_PATH: &str = "/v1/oracle/observations";
pub const IO_TIMEOUT_MS: u64 = 2000;
const MAX_REPLY: usize = 64 * 1024;

#[derive(Debug, Clone)]
pub struct NodeClient {
    addr: SocketAddr,
    host_header: String,
    token: Option<String>,
}

/// `http://<loopback>:<port>` (an optional trailing `/`), nothing else.
pub fn parse_loopback_url(url: &str) -> Result<(SocketAddr, String), String> {
    let rest = url
        .strip_prefix("http://")
        .ok_or("node URL: only http:// on loopback is supported")?;
    let authority = rest.strip_suffix('/').unwrap_or(rest);
    if authority.is_empty() || authority.contains(['/', '?', '#', '@']) {
        return Err("node URL: expected http://HOST:PORT without a path".into());
    }
    let addrs: Vec<SocketAddr> = authority
        .to_socket_addrs()
        .map_err(|e| format!("node URL {authority}: {e}"))?
        .collect();
    if addrs.is_empty() {
        return Err(format!("node URL {authority}: no address"));
    }
    // Every address the name resolves to must be loopback (a name that also resolves elsewhere is refused).
    if !addrs.iter().all(|a| is_loopback(a.ip())) {
        return Err(format!(
            "node URL {authority}: not a loopback address (refused: the token would leave this host in clear text)"
        ));
    }
    Ok((addrs[0], authority.to_string()))
}

fn is_loopback(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(v4) => v4.is_loopback(),
        IpAddr::V6(v6) => {
            v6.is_loopback() || v6.to_ipv4_mapped().is_some_and(|v4| v4.is_loopback())
        }
    }
}

/// The ingest token file: one line of 16 to 256 visible ASCII characters without spaces; refused when group or
/// world can read it, or when it is a link (same rules as the key file).
pub fn read_token(path: &Path) -> Result<String, String> {
    let meta =
        fs::symlink_metadata(path).map_err(|e| format!("token file {}: {e}", path.display()))?;
    if meta.file_type().is_symlink() || !meta.is_file() {
        return Err("token file: must be a regular file, not a link".into());
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = meta.permissions().mode() & 0o777;
        if mode & 0o077 != 0 {
            return Err(format!(
                "token file: mode {mode:o} lets others read or write it; use chmod 600"
            ));
        }
    }
    let text = fs::read_to_string(path).map_err(|e| format!("token file: {e}"))?;
    let t = text.strip_suffix('\n').unwrap_or(&text);
    if !(16..=256).contains(&t.len()) || !t.bytes().all(|b| b.is_ascii_graphic()) {
        return Err("token file: expected one line of 16 to 256 visible ASCII characters".into());
    }
    Ok(t.to_string())
}

impl NodeClient {
    pub fn new(url: &str, token: Option<String>) -> Result<Self, String> {
        let (addr, host_header) = parse_loopback_url(url)?;
        Ok(NodeClient {
            addr,
            host_header,
            token,
        })
    }

    /// One POST of a JSON body. Returns the HTTP status and the reply body. Never retries: the next round is the
    /// retry (an observation is useless once its round has passed).
    pub fn post_json(&self, path: &str, body: &str) -> Result<(u16, String), String> {
        let to = Duration::from_millis(IO_TIMEOUT_MS);
        let mut s =
            TcpStream::connect_timeout(&self.addr, to).map_err(|e| format!("connect: {e}"))?;
        s.set_read_timeout(Some(to)).map_err(|e| e.to_string())?;
        s.set_write_timeout(Some(to)).map_err(|e| e.to_string())?;
        let mut req = format!(
            "POST {path} HTTP/1.1\r\nHost: {}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n",
            self.host_header,
            body.len()
        );
        if let Some(t) = &self.token {
            req.push_str(&format!("Authorization: Bearer {t}\r\n"));
        }
        req.push_str("\r\n");
        s.write_all(req.as_bytes())
            .and_then(|_| s.write_all(body.as_bytes()))
            .map_err(|e| format!("send: {e}"))?;
        let mut raw = Vec::new();
        let mut buf = [0u8; 4096];
        loop {
            match s.read(&mut buf) {
                Ok(0) => break,
                Ok(n) => {
                    raw.extend_from_slice(&buf[..n]);
                    if raw.len() > MAX_REPLY {
                        return Err("reply too large".into());
                    }
                    if reply_complete(&raw) {
                        break;
                    }
                }
                Err(e) => return Err(format!("receive: {e}")),
            }
        }
        parse_reply(&raw)
    }
}

fn split_head(raw: &[u8]) -> Option<(&str, &[u8])> {
    let i = raw.windows(4).position(|w| w == b"\r\n\r\n")?;
    Some((std::str::from_utf8(&raw[..i]).ok()?, &raw[i + 4..]))
}

fn header<'a>(head: &'a str, name: &str) -> Option<&'a str> {
    head.lines().skip(1).find_map(|l| {
        let (k, v) = l.split_once(':')?;
        k.trim().eq_ignore_ascii_case(name).then_some(v.trim())
    })
}

fn reply_complete(raw: &[u8]) -> bool {
    let Some((head, body)) = split_head(raw) else {
        return false;
    };
    if let Some(n) = header(head, "content-length").and_then(|v| v.parse::<usize>().ok()) {
        return body.len() >= n;
    }
    if header(head, "transfer-encoding").is_some_and(|v| v.eq_ignore_ascii_case("chunked")) {
        return body.ends_with(b"0\r\n\r\n");
    }
    false
}

fn parse_reply(raw: &[u8]) -> Result<(u16, String), String> {
    let (head, body) = split_head(raw).ok_or("reply: no HTTP header")?;
    let status_line = head.lines().next().unwrap_or_default();
    let mut parts = status_line.split(' ');
    let (proto, code) = (parts.next().unwrap_or(""), parts.next().unwrap_or(""));
    if !proto.starts_with("HTTP/1.") {
        return Err(format!("reply: not HTTP/1.x: {status_line:?}"));
    }
    let status: u16 = code
        .parse()
        .ok()
        .filter(|c| (100..600).contains(c))
        .ok_or(format!("reply: bad status in {status_line:?}"))?;
    let body = if header(head, "transfer-encoding")
        .is_some_and(|v| v.eq_ignore_ascii_case("chunked"))
    {
        dechunk(body)?
    } else if let Some(n) = header(head, "content-length").and_then(|v| v.parse::<usize>().ok()) {
        body.get(..n)
            .ok_or("reply: body shorter than content-length")?
            .to_vec()
    } else {
        body.to_vec()
    };
    Ok((status, String::from_utf8_lossy(&body).into_owned()))
}

fn dechunk(mut b: &[u8]) -> Result<Vec<u8>, String> {
    let mut out = Vec::new();
    loop {
        let i = b
            .windows(2)
            .position(|w| w == b"\r\n")
            .ok_or("reply: bad chunk")?;
        let size_text = std::str::from_utf8(&b[..i]).map_err(|_| "reply: bad chunk size")?;
        let size = usize::from_str_radix(size_text.split(';').next().unwrap_or("").trim(), 16)
            .map_err(|_| "reply: bad chunk size")?;
        b = &b[i + 2..];
        if size == 0 {
            return Ok(out);
        }
        if b.len() < size + 2 {
            return Err("reply: truncated chunk".into());
        }
        out.extend_from_slice(&b[..size]);
        b = &b[size + 2..];
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_loopback_plain_http() {
        for ok in [
            "http://127.0.0.1:53500",
            "http://127.0.0.1:53500/",
            "http://127.8.9.10:1",
            "http://[::1]:53500",
            "http://localhost:53500",
        ] {
            assert!(parse_loopback_url(ok).is_ok(), "{ok}");
        }
        for bad in [
            "https://127.0.0.1:53500",
            "http://10.0.0.1:53500",
            "http://192.168.1.1:80",
            "http://45.77.142.123:53500",
            "http://0.0.0.0:53500",
            "http://127.0.0.1",
            "http://127.0.0.1:53500/v1",
            "http://user@127.0.0.1:53500",
            "http://[::ffff:10.0.0.1]:80",
            "127.0.0.1:53500",
            "http://",
        ] {
            assert!(parse_loopback_url(bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn replies() {
        let r = b"HTTP/1.1 200 OK\r\nContent-Length: 11\r\nX: y\r\n\r\n{\"ok\":true}";
        assert!(reply_complete(r));
        assert_eq!(parse_reply(r), Ok((200, "{\"ok\":true}".into())));
        assert!(!reply_complete(&r[..r.len() - 1]));
        let c = b"HTTP/1.1 409 Conflict\r\ntransfer-encoding: chunked\r\n\r\n4\r\n{\"a\"\r\n3\r\n:1}\r\n0\r\n\r\n";
        assert!(reply_complete(c));
        assert_eq!(parse_reply(c), Ok((409, "{\"a\":1}".into())));
        assert!(parse_reply(b"SSH-2.0-x\r\n\r\n").is_err());
        assert!(parse_reply(b"HTTP/1.1 abc\r\n\r\n").is_err());
        assert!(parse_reply(b"garbage").is_err());
    }
}

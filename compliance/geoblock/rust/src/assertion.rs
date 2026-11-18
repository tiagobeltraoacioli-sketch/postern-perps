//! Edge-to-node geo assertion verification (port of verifyAssertion in ../src/geo-assertion.mjs).
//! Header: v1;c=<CC|->;s=<CC-SUB|->;tor=<0|1|->;vpn=<0|1|->;host=<0|1|->;sur=<api|ui>;ts=<ms>;rid=<32 hex>
//! MAC: hex(HMAC-SHA256(secret, "postern-perps/geo-assertion/v1\n" + header)).

use hmac::{Hmac, Mac};
use sha2::Sha256;
use std::collections::{HashMap, VecDeque};

pub const MAC_DOMAIN: &str = "postern-perps/geo-assertion/v1\n";
pub const MAX_AGE_MS: i64 = 30_000;
pub const MAX_FUTURE_MS: i64 = 5_000;
pub const MIN_SECRET_BYTES: usize = 32;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Verified {
    pub country: Option<String>,
    pub subdivision: Option<String>,
    pub tor: Option<bool>,
    pub vpn_proxy: Option<bool>,
    pub hosting: Option<bool>,
    pub surface: String,
    pub ts_ms: i64,
    pub request_id: String,
}

impl Verified {
    /// The `geo`, `signals` and `surface` fields of a decision request (geo.trusted = true).
    pub fn request_fields(&self) -> serde_json::Value {
        serde_json::json!({
            "geo": {"country": self.country, "subdivision": self.subdivision, "trusted": true},
            "signals": {"tor": self.tor, "vpnProxy": self.vpn_proxy, "hosting": self.hosting},
            "surface": self.surface,
        })
    }
}

pub fn mac_hex(secret: &[u8], header: &str) -> Option<String> {
    if secret.len() < MIN_SECRET_BYTES {
        return None;
    }
    let mut m = <Hmac<Sha256> as Mac>::new_from_slice(secret).ok()?;
    m.update(MAC_DOMAIN.as_bytes());
    m.update(header.as_bytes());
    Some(crate::sanctions::hex(&m.finalize().into_bytes()))
}

fn ct_eq(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    a.iter().zip(b).fold(0u8, |acc, (x, y)| acc | (x ^ y)) == 0
}

fn tri(s: &str) -> Option<Option<bool>> {
    match s {
        "1" => Some(Some(true)),
        "0" => Some(Some(false)),
        "-" => Some(None),
        _ => None,
    }
}

/// Request ids seen inside the window; refuses a repeat, and refuses when full rather than forget ids.
pub struct ReplayCache {
    window_ms: i64,
    max_entries: usize,
    order: VecDeque<(String, i64)>,
    seen: HashMap<String, i64>,
}

impl ReplayCache {
    pub fn new(max_entries: usize) -> Self {
        ReplayCache { window_ms: MAX_AGE_MS + MAX_FUTURE_MS, max_entries, order: VecDeque::new(), seen: HashMap::new() }
    }
    pub fn add(&mut self, rid: &str, ts_ms: i64, now_ms: i64) -> bool {
        while let Some((k, t)) = self.order.front() {
            if *t < now_ms - self.window_ms {
                self.seen.remove(k);
                self.order.pop_front();
            } else {
                break;
            }
        }
        if self.seen.contains_key(rid) || self.seen.len() >= self.max_entries {
            return false;
        }
        let t = ts_ms.max(now_ms);
        self.seen.insert(rid.to_string(), t);
        self.order.push_back((rid.to_string(), t));
        true
    }
}

/// Ok(Verified) or Err(reason) with the JS reason words: missing, shape, mac, format, stale, future, inconsistent, replay.
pub fn verify(header: Option<&str>, mac: Option<&str>, secret: &[u8], now_ms: i64, replay: Option<&mut ReplayCache>) -> Result<Verified, &'static str> {
    let (Some(h), Some(m)) = (header, mac) else { return Err("missing") };
    if h.len() > 200 || m.len() != 64 || !m.bytes().all(|c| c.is_ascii_digit() || (b'a'..=b'f').contains(&c)) {
        return Err("shape");
    }
    let Some(expect) = mac_hex(secret, h) else { return Err("mac") };
    if !ct_eq(expect.as_bytes(), m.as_bytes()) {
        return Err("mac");
    }
    let parts: Vec<&str> = h.split(';').collect();
    let keys = ["v1", "c=", "s=", "tor=", "vpn=", "host=", "sur=", "ts=", "rid="];
    if parts.len() != 9 || parts[0] != "v1" || !parts[1..].iter().zip(&keys[1..]).all(|(p, k)| p.starts_with(k)) {
        return Err("format");
    }
    let val = |i: usize| &parts[i][keys[i].len()..];
    let c = val(1);
    let country = if c == "-" {
        None
    } else if c.len() == 2 && c.as_bytes()[0].is_ascii_uppercase() && (c.as_bytes()[1].is_ascii_uppercase() || c.as_bytes()[1].is_ascii_digit()) {
        Some(c.to_string())
    } else {
        return Err("format");
    };
    let s = val(2);
    let subdivision = if s == "-" {
        None
    } else if crate::list::is_subdivision(s) {
        Some(s.to_string())
    } else {
        return Err("format");
    };
    let (Some(tor), Some(vpn), Some(host)) = (tri(val(3)), tri(val(4)), tri(val(5))) else { return Err("format") };
    let sur = val(6);
    if sur != "api" && sur != "ui" {
        return Err("format");
    }
    let ts = val(7);
    if ts.is_empty() || ts.len() > 16 || !ts.bytes().all(|b| b.is_ascii_digit()) || (ts.len() > 1 && ts.starts_with('0')) {
        return Err("format");
    }
    let ts_ms: i64 = ts.parse().map_err(|_| "format")?;
    let rid = val(8);
    if rid.len() != 32 || !rid.bytes().all(|c| c.is_ascii_digit() || (b'a'..=b'f').contains(&c)) {
        return Err("format");
    }
    if ts_ms < now_ms - MAX_AGE_MS {
        return Err("stale");
    }
    if ts_ms > now_ms + MAX_FUTURE_MS {
        return Err("future");
    }
    if let Some(sd) = &subdivision {
        if country.as_deref() != Some(&sd[..2]) {
            return Err("inconsistent");
        }
    }
    if let Some(r) = replay {
        if !r.add(rid, ts_ms, now_ms) {
            return Err("replay");
        }
    }
    Ok(Verified { country, subdivision, tor, vpn_proxy: vpn, hosting: host, surface: sur.to_string(), ts_ms, request_id: rid.to_string() })
}

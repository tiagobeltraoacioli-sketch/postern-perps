//! Run-time sanctions matcher (port of loadSanctions in ../src/sanctions.mjs). The builder stays in JS
//! (an operator tool); the node only loads its output, with the same integrity refusals.

use serde_json::Value;
use sha2::{Digest, Sha256};
use std::collections::HashSet;

pub const SCHEMA: &str = "postern-perps/sanctions-evm";
pub const DEFAULT_MAX_AGE_MS: i64 = 48 * 3600 * 1000;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SanctionsError(pub String);
impl std::fmt::Display for SanctionsError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "sanctions: {}", self.0)
    }
}
impl std::error::Error for SanctionsError {}

#[derive(Debug, Clone)]
pub struct Sanctions {
    pub version: String,
    pub sha256: String,
    pub built_ms: i64,
    pub max_age_ms: i64,
    set: HashSet<String>,
}

impl Sanctions {
    pub fn has(&self, lower: &str) -> bool {
        self.set.contains(&lower.to_ascii_lowercase())
    }
    pub fn fresh(&self, now_ms: f64) -> bool {
        now_ms >= self.built_ms as f64 && now_ms - self.built_ms as f64 <= self.max_age_ms as f64
    }
    pub fn len(&self) -> usize {
        self.set.len()
    }
    pub fn is_empty(&self) -> bool {
        self.set.is_empty()
    }
}

/// Days from 1970-01-01 of a proleptic Gregorian date (H. Hinnant's algorithm).
fn days_from_civil(y: i64, m: i64, d: i64) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = if y >= 0 { y } else { y - 399 } / 400;
    let yoe = y - era * 400;
    let mp = (m + 9) % 12;
    let doy = (153 * mp + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146097 + doe - 719468
}

/// Parse `YYYY-MM-DDTHH:MM:SS[.fff]Z` (the form the builder writes) to ms. Anything else: None.
pub fn parse_utc_ms(s: &str) -> Option<i64> {
    let b = s.as_bytes();
    if b.len() < 20 || *b.last()? != b'Z' {
        return None;
    }
    let num = |r: std::ops::Range<usize>| -> Option<i64> {
        let t = s.get(r)?;
        if t.bytes().all(|c| c.is_ascii_digit()) { t.parse().ok() } else { None }
    };
    if b[4] != b'-' || b[7] != b'-' || b[10] != b'T' || b[13] != b':' || b[16] != b':' {
        return None;
    }
    let (y, mo, d, h, mi, se) = (num(0..4)?, num(5..7)?, num(8..10)?, num(11..13)?, num(14..16)?, num(17..19)?);
    let mut ms = 0;
    let rest = &s[19..s.len() - 1];
    if !rest.is_empty() {
        let f = rest.strip_prefix('.')?;
        if f.len() != 3 || !f.bytes().all(|c| c.is_ascii_digit()) {
            return None;
        }
        ms = f.parse().ok()?;
    }
    let mdays = [31, if (y % 4 == 0 && y % 100 != 0) || y % 400 == 0 { 29 } else { 28 }, 31, 30, 31, 30, 31, 31, 30, 31, 30, 31];
    if !(1..=12).contains(&mo) || d < 1 || d > mdays[(mo - 1) as usize] || h > 23 || mi > 59 || se > 59 {
        return None;
    }
    Some(((days_from_civil(y, mo, d) * 24 + h) * 60 + mi) * 60_000 + se * 1000 + ms)
}

pub fn evm_addresses_sha256(addrs: &[&str]) -> String {
    hex(&Sha256::digest(addrs.join("\n").as_bytes()))
}

pub(crate) fn hex(b: &[u8]) -> String {
    b.iter().map(|x| format!("{x:02x}")).collect()
}

fn is_lower_addr(a: &str) -> bool {
    let b = a.as_bytes();
    b.len() == 42 && &b[..2] == b"0x" && b[2..].iter().all(|c| c.is_ascii_digit() || (b'a'..=b'f').contains(c))
}

pub fn load_sanctions(json: &Value, max_age_ms: i64) -> Result<Sanctions, SanctionsError> {
    let e = |m: &str| Err(SanctionsError(m.to_string()));
    if json.get("schema").and_then(Value::as_str) != Some(SCHEMA) {
        return e("schema must be postern-perps/sanctions-evm");
    }
    let Some(Value::Array(evm)) = json.get("evm") else { return e("evm list missing") };
    let mut addrs: Vec<&str> = Vec::with_capacity(evm.len());
    for (i, x) in evm.iter().enumerate() {
        let a = x.get("address").and_then(Value::as_str).unwrap_or("");
        if !is_lower_addr(a) {
            return Err(SanctionsError(format!("address {i} not lowercase 0x+40 hex")));
        }
        if let Some(prev) = addrs.last() {
            if *prev >= a {
                return e("addresses not strictly sorted (duplicate or reordered)");
            }
        }
        addrs.push(a);
    }
    if Some(evm_addresses_sha256(&addrs).as_str()) != json.get("evmAddressesSha256").and_then(Value::as_str) {
        return e("content hash mismatch");
    }
    let Some(built_ms) = json.get("builtUtc").and_then(Value::as_str).and_then(parse_utc_ms) else {
        return e("builtUtc missing");
    };
    Ok(Sanctions {
        version: json.get("version").and_then(Value::as_str).unwrap_or("").to_string(),
        sha256: json.get("evmAddressesSha256").and_then(Value::as_str).unwrap_or("").to_string(),
        built_ms,
        max_age_ms,
        set: addrs.iter().map(|s| s.to_string()).collect(),
    })
}

#[cfg(test)]
mod t {
    use super::*;
    #[test]
    fn utc() {
        assert_eq!(parse_utc_ms("1970-01-01T00:00:00Z"), Some(0));
        assert_eq!(parse_utc_ms("2026-11-17T00:00:00Z"), Some(1_794_873_600_000));
        assert_eq!(parse_utc_ms("2026-11-17T04:10:00.250Z"), Some(1_794_873_600_000 + 15_000_000 + 250));
        for bad in ["2026-11-17", "2026-11-17T00:00:00", "2026-02-30T00:00:00Z", "2026-11-17T24:00:00Z", "2026-11-17T00:00:00.5Z", "2026-11-17 00:00:00Z"] {
            assert_eq!(parse_utc_ms(bad), None, "{bad}");
        }
    }
}

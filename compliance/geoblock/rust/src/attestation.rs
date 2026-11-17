//! Attestation verification (port of verifyAttestation in ../src/attestation.mjs): EIP-712 under the domain
//! {name "PosternPerps Attestation", version "1", chainId 7007701}; 65-byte r||s||v, v in {27,28}, 1 <= r,s < n,
//! s <= n/2; recovered address = account.

use crate::addr::{keccak256, normalize_evm_address};
use crate::list::RESERVED_CODES;
use k256::ecdsa::{RecoveryId, Signature, VerifyingKey};
use serde_json::Value;
use std::collections::BTreeMap;

pub const DOMAIN_NAME: &str = "PosternPerps Attestation";
pub const DOMAIN_VERSION: &str = "1";
pub const DOMAIN_CHAIN_ID: u64 = 7_007_701;
pub const DOMAIN_TYPE: &str = "EIP712Domain(string name,string version,uint256 chainId)";
pub const ATTESTATION_TYPE: &str = "Attestation(address account,string statementId,bytes32 statementHash,string residenceCountry,uint64 issuedAt,uint64 expiresAt)";
pub const MAX_VALIDITY_MS: u64 = 400 * 24 * 3600 * 1000;
pub const MAX_CLOCK_SKEW_MS: u64 = 5 * 60 * 1000;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Verification {
    pub status: &'static str,
    pub reason: Option<String>,
    pub residence_country: Option<String>,
    pub signer: Option<String>,
}

fn word_u64(v: u64) -> [u8; 32] {
    let mut o = [0u8; 32];
    o[24..].copy_from_slice(&v.to_be_bytes());
    o
}

pub fn domain_separator() -> [u8; 32] {
    let mut b = Vec::with_capacity(128);
    b.extend_from_slice(&keccak256(DOMAIN_TYPE.as_bytes()));
    b.extend_from_slice(&keccak256(DOMAIN_NAME.as_bytes()));
    b.extend_from_slice(&keccak256(DOMAIN_VERSION.as_bytes()));
    b.extend_from_slice(&word_u64(DOMAIN_CHAIN_ID));
    keccak256(&b)
}

pub struct Message {
    pub account: [u8; 20],
    pub statement_id: String,
    pub statement_hash: [u8; 32],
    pub residence_country: String,
    pub issued_at: u64,
    pub expires_at: u64,
}

fn hex_bytes(s: &str) -> Option<Vec<u8>> {
    let h = s.strip_prefix("0x")?;
    if h.len() % 2 != 0 || !h.bytes().all(|c| c.is_ascii_hexdigit()) {
        return None;
    }
    (0..h.len()).step_by(2).map(|i| u8::from_str_radix(&h[i..i + 2], 16).ok()).collect()
}

fn dec_u64(v: Option<&Value>) -> Option<u64> {
    let s = v?.as_str()?;
    if s.is_empty() || s.len() > 20 || !s.bytes().all(|b| b.is_ascii_digit()) || (s.len() > 1 && s.starts_with('0')) {
        return None;
    }
    s.parse().ok()
}

/// Parse and check the message; Err(field) names the first bad field (same words as the JS).
pub fn parse_message(m: &Value) -> Result<Message, &'static str> {
    if !m.is_object() {
        return Err("message missing");
    }
    let acct = m.get("account").and_then(Value::as_str).and_then(normalize_evm_address).ok_or("account")?;
    let sid = m.get("statementId").and_then(Value::as_str).ok_or("statementId")?;
    let sb = sid.as_bytes();
    if sb.is_empty() || sb.len() > 64 || !(sb[0].is_ascii_lowercase() || sb[0].is_ascii_digit())
        || !sb.iter().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || b"./-".contains(c)) {
        return Err("statementId");
    }
    let sh = m.get("statementHash").and_then(Value::as_str).ok_or("statementHash")?;
    if sh.len() != 66 || !sh.starts_with("0x") || !sh[2..].bytes().all(|c| c.is_ascii_digit() || (b'a'..=b'f').contains(&c)) {
        return Err("statementHash");
    }
    let rc = m.get("residenceCountry").and_then(Value::as_str).ok_or("residenceCountry")?;
    if rc.len() != 2 || !rc.bytes().all(|b| b.is_ascii_uppercase()) || RESERVED_CODES.contains(&rc) {
        return Err("residenceCountry");
    }
    let issued_at = dec_u64(m.get("issuedAt")).ok_or("issuedAt")?;
    let expires_at = dec_u64(m.get("expiresAt")).ok_or("expiresAt")?;
    let mut account = [0u8; 20];
    account.copy_from_slice(&hex_bytes(&acct).ok_or("account")?);
    let mut statement_hash = [0u8; 32];
    statement_hash.copy_from_slice(&hex_bytes(sh).ok_or("statementHash")?);
    Ok(Message { account, statement_id: sid.to_string(), statement_hash, residence_country: rc.to_string(), issued_at, expires_at })
}

pub fn digest(m: &Message) -> [u8; 32] {
    let mut s = Vec::with_capacity(224);
    s.extend_from_slice(&keccak256(ATTESTATION_TYPE.as_bytes()));
    let mut a = [0u8; 32];
    a[12..].copy_from_slice(&m.account);
    s.extend_from_slice(&a);
    s.extend_from_slice(&keccak256(m.statement_id.as_bytes()));
    s.extend_from_slice(&m.statement_hash);
    s.extend_from_slice(&keccak256(m.residence_country.as_bytes()));
    s.extend_from_slice(&word_u64(m.issued_at));
    s.extend_from_slice(&word_u64(m.expires_at));
    let mut b = vec![0x19, 0x01];
    b.extend_from_slice(&domain_separator());
    b.extend_from_slice(&keccak256(&s));
    keccak256(&b)
}

/// Strict recovery: None for any malformed or high-s signature.
pub fn recover(digest: &[u8; 32], sig: &[u8]) -> Option<String> {
    if sig.len() != 65 || !(sig[64] == 27 || sig[64] == 28) {
        return None;
    }
    let signature = Signature::from_slice(&sig[..64]).ok()?; // refuses r or s = 0 or >= n
    if signature.normalize_s().is_some() {
        return None; // high-s
    }
    let rid = RecoveryId::from_byte(sig[64] - 27)?;
    let vk = VerifyingKey::recover_from_prehash(digest, &signature, rid).ok()?;
    let pt = vk.to_encoded_point(false);
    let h = keccak256(&pt.as_bytes()[1..]);
    Some(format!("0x{}", crate::sanctions::hex(&h[12..])))
}

/// verify(record {message, signature}, statements {id -> hash}, now_ms).
pub fn verify(record: &Value, statements: &BTreeMap<String, String>, now_ms: u64) -> Verification {
    let no = |status: &'static str, reason: &str| Verification { status, reason: Some(reason.to_string()), residence_country: None, signer: None };
    let m = match record.get("message").map(parse_message) {
        Some(Ok(m)) => m,
        Some(Err(f)) => return no("invalid", &format!("message.{f}")),
        None => return no("invalid", "message.message missing"),
    };
    let sid_hash = record["message"]["statementHash"].as_str().unwrap_or("");
    match statements.get(&m.statement_id) {
        None => return no("invalid", "statement not current"),
        Some(h) if h != sid_hash => return no("invalid", "statement hash differs from the current text"),
        _ => {}
    }
    let sig = match record.get("signature").and_then(Value::as_str) {
        Some(s) if s.len() == 132 => hex_bytes(s),
        _ => None,
    };
    let Some(sig) = sig else { return no("invalid", "signature shape") };
    let Some(signer) = recover(&digest(&m), &sig) else {
        return no("invalid", "signature does not recover (length, v, r/s range or high-s)");
    };
    let acct = format!("0x{}", crate::sanctions::hex(&m.account));
    if signer != acct {
        return no("invalid", "signer is not the account");
    }
    if m.expires_at <= m.issued_at || m.expires_at - m.issued_at > MAX_VALIDITY_MS {
        return no("invalid", "validity period");
    }
    if m.issued_at > now_ms.saturating_add(MAX_CLOCK_SKEW_MS) {
        return no("invalid", "issued in the future");
    }
    if now_ms > m.expires_at {
        return Verification { status: "expired", reason: Some("expired".into()), residence_country: Some(m.residence_country), signer: Some(signer) };
    }
    Verification { status: "valid", reason: None, residence_country: Some(m.residence_country), signer: Some(signer) }
}

impl Verification {
    /// The `attestation` field of a decision request.
    pub fn request_field(&self) -> Value {
        serde_json::json!({"status": self.status, "residenceCountry": self.residence_country})
    }
}

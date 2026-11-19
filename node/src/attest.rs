//! Attestation records (agent 397's design, `compliance/geoblock/DESIGN.md` decisions 3 and 10, step 6 of its
//! decision table): the user's signed statement of eligibility and residence, an EIP-712 message of primary type
//! `Attestation` under the engine domain `{PosternPerps, 1, 700770, vault}`. Everything legal about it (the text,
//! whether a signature is the right form, how long records are kept) is for counsel; this module only verifies,
//! stores and answers.
//!
//! - Verification is 397's Rust port (`postern_geoblock::attestation::verify`): the statement id and hash must
//!   be the CURRENT text's (`compliance/legal/notices.json`), the signature strict (65 bytes, v 27/28, low-s) and
//!   by the account itself, the validity at most 400 days, not issued in the future.
//! - One record per account: the newest valid one (by `issuedAt`). Records are appended to
//!   `attestations.jsonl` in the data directory and re-verified when loaded: a record that no longer verifies
//!   (the text changed, the vault differs) is not loaded.
//! - Enforcement at admission (only with `--require-attestation`; step 6): an action that opens or changes
//!   exposure, from a geo-allowed request, needs a valid record whose declared residence the list allows.
//!   Decision 3 holds with or without the flag: a valid record declaring a residence the list does NOT allow
//!   makes the account exit-only, whatever its IP says (a declaration can only add a refusal).
//!
//! WHO MAY READ A RECORD (R436-17; decided here as a technical default, the legal question is for counsel): a
//! record names an address's declared country of residence, so it is not public. `GET /v1/attestations/{address}`
//! answers only (a) the account itself, proven by a fresh signature (`read_digest`: EIP-712 `AttestationRead
//! (address account,uint64 timestamp)` under the engine domain, timestamp within `READ_WINDOW_MS` of the node's
//! clock), or (b) the operator, by the bearer token of `--operator-token-file`. Anyone else gets 401, the same
//! answer whether a record exists or not. The node's own admission reads the store in process.
//!
//! WHAT THE STORE HOLDS (R436-17): the same record again stores nothing; a replacement must be issued at least
//! `MIN_REPLACE_MS` after the record it replaces (at most one line per account per minute); records of accounts
//! the engine does not know (no deposit) are held up to `MAX_UNKNOWN_ACCOUNTS`, then refused.
//!
//! UNAUDITED PROTOTYPE.

use crate::util::{parse_address, to_hex, Address};
use std::collections::BTreeMap;
use std::io::Write;
use std::path::PathBuf;

#[derive(Default)]
pub struct Attestations {
    /// statementId -> statementHash of the texts accepted now
    pub statements: BTreeMap<String, String>,
    pub vault: String,
    records: BTreeMap<Address, serde_json::Value>,
    /// accounts whose record was stored while the engine did not know them
    unknown: std::collections::BTreeSet<Address>,
    file: Option<PathBuf>,
    /// step 6 enforced at admission
    pub required: bool,
    /// places for accounts the engine does not know (`MAX_UNKNOWN_ACCOUNTS` unless a test sets it lower)
    pub max_unknown: usize,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Status {
    /// `valid`, `expired`, or `missing`
    pub status: &'static str,
    pub residence_country: Option<String>,
    pub record: Option<serde_json::Value>,
}

/// The current statement set from `compliance/legal/notices.json` (`attestation.statementId` / `.statementHash`).
pub fn statements_from_notices(notices: &serde_json::Value) -> Result<BTreeMap<String, String>, String> {
    let a = notices.get("attestation").ok_or("notices: no attestation section")?;
    let id = a.get("statementId").and_then(|v| v.as_str()).ok_or("notices: attestation.statementId")?;
    let hash = a.get("statementHash").and_then(|v| v.as_str()).ok_or("notices: attestation.statementHash")?;
    // the hash must be the hash of the text the file carries (the port's rule at load)
    if let Some(text) = a.get("text").and_then(|v| v.as_str()) {
        if to_hex(&crate::util::keccak(text.as_bytes())) != hash {
            return Err("notices: attestation.statementHash is not keccak256 of attestation.text".into());
        }
    }
    Ok([(id.to_string(), hash.to_string())].into_iter().collect())
}

impl Attestations {
    pub fn new(statements: BTreeMap<String, String>, vault: &Address, file: Option<PathBuf>, now_ms: u64) -> Attestations {
        let mut a = Attestations { statements, vault: to_hex(vault), records: BTreeMap::new(), unknown: Default::default(), file: None, required: false, max_unknown: MAX_UNKNOWN_ACCOUNTS };
        if let Some(p) = &file {
            if let Ok(txt) = std::fs::read_to_string(p) {
                for line in txt.lines().filter(|l| !l.trim().is_empty()) {
                    if let Ok(rec) = serde_json::from_str::<serde_json::Value>(line) {
                        // re-verified at load; expired ones are kept as records (their status says so)
                        let _ = a.accept(&rec, now_ms, true, &|_| true);
                    }
                }
            }
        }
        a.file = file;
        a
    }

    fn verify(&self, record: &serde_json::Value, now_ms: u64) -> Result<postern_geoblock::attestation::Verification, String> {
        postern_geoblock::attestation::verify(record, &self.statements, now_ms, Some(&self.vault)).map_err(|e| e.to_string())
    }

    /// `Ok((account, stored))`: `stored` is false when the identical record was already held.
    fn accept(&mut self, record: &serde_json::Value, now_ms: u64, loading: bool, known: &dyn Fn(&Address) -> bool) -> Result<(Address, bool), String> {
        let only: Vec<&str> = record.as_object().map(|o| o.keys().map(|k| k.as_str()).collect()).unwrap_or_default();
        if only.len() != 2 || !only.contains(&"message") || !only.contains(&"signature") {
            return Err("record: exactly {message, signature}".into());
        }
        let v = self.verify(record, now_ms)?;
        if v.status != "valid" && !(loading && v.status == "expired") {
            return Err(format!("{}: {}", v.status, v.reason.unwrap_or_default()));
        }
        let account = parse_address(v.signer.as_deref().ok_or("no signer")?)?;
        let issued = |r: &serde_json::Value| r["message"]["issuedAt"].as_str().and_then(|s| s.parse::<u64>().ok()).unwrap_or(0);
        if let Some(old) = self.records.get(&account) {
            if old == record {
                return Ok((account, false));
            }
            if issued(old) > issued(record) {
                return Err("an attestation issued later is already held for this account".into());
            }
            if !loading && issued(record) < issued(old).saturating_add(MIN_REPLACE_MS) {
                return Err("a replacement must be issued at least one minute after the record it replaces".into());
            }
        } else if !loading {
            // bounded: accounts the engine does not know (no deposit yet) share a fixed number of places
            self.unknown.retain(|a| !known(a));
            if !known(&account) {
                if self.unknown.len() >= self.max_unknown {
                    return Err("STORE FULL for accounts without a deposit: deposit first, then attest".into());
                }
                self.unknown.insert(account);
            }
        }
        self.records.insert(account, record.clone());
        Ok((account, true))
    }

    /// Verifies and stores a record `{message, signature}`. Returns the account it speaks for.
    /// `known(account)`: does the engine have this account (a credited deposit)?
    pub fn submit(&mut self, record: &serde_json::Value, now_ms: u64, known: &dyn Fn(&Address) -> bool) -> Result<Address, String> {
        let (account, stored) = self.accept(record, now_ms, false, known)?;
        if !stored {
            return Ok(account);
        }
        if let Some(p) = &self.file {
            let line = serde_json::to_string(record).map_err(|e| e.to_string())?;
            let r = std::fs::OpenOptions::new().create(true).append(true).open(p).and_then(|mut f| {
                writeln!(f, "{line}")?;
                f.sync_data()
            });
            if let Err(e) = r {
                self.records.remove(&account);
                self.unknown.remove(&account);
                return Err(format!("the record could not be stored: {e}"));
            }
        }
        Ok(account)
    }

    /// The account's record judged NOW (a stored record expires by itself).
    pub fn status(&self, account: &Address, now_ms: u64) -> Status {
        let Some(rec) = self.records.get(account) else { return Status { status: "missing", residence_country: None, record: None } };
        match self.verify(rec, now_ms) {
            Ok(v) if v.status == "valid" => Status { status: "valid", residence_country: v.residence_country, record: Some(rec.clone()) },
            Ok(v) if v.status == "expired" => Status { status: "expired", residence_country: v.residence_country, record: Some(rec.clone()) },
            _ => Status { status: "missing", residence_country: None, record: None },
        }
    }

    pub fn len(&self) -> usize {
        self.records.len()
    }

    pub fn is_empty(&self) -> bool {
        self.records.is_empty()
    }
}

pub const READ_WINDOW_MS: u64 = 60_000;
pub const MIN_REPLACE_MS: u64 = 60_000;
pub const MAX_UNKNOWN_ACCOUNTS: usize = 10_000;

/// The digest an account signs to read its own record: EIP-712 `AttestationRead(address account,uint64 timestamp)`
/// under the engine domain of this deployment (`timestamp` = Unix ms).
pub fn read_digest(g: &crate::genesis::Genesis, account: &Address, timestamp_ms: u64) -> crate::util::H256 {
    use crate::eip712::{digest, Registry, Val};
    use std::sync::OnceLock;
    static R: OnceLock<Registry> = OnceLock::new();
    let r = R.get_or_init(|| Registry::from_json(&serde_json::json!({"AttestationRead": [{"name": "account", "type": "address"}, {"name": "timestamp", "type": "uint64"}]})).expect("static type"));
    let sh = r.hash_struct("AttestationRead", &Val::Struct(vec![Val::Address(*account), Val::Uint(timestamp_ms as u128)])).expect("static type");
    digest(&g.engine_domain.separator(), &sh)
}

/// Is this read of `account`'s record signed by the account itself, now? (`time` and `signature` are the values
/// of the headers `X-Postern-Read-Time` and `X-Postern-Read-Signature`.)
pub fn read_authorised(g: &crate::genesis::Genesis, account: &Address, time: Option<&str>, signature: Option<&str>, now_ms: u64) -> bool {
    let (Some(t), Some(sig)) = (time, signature) else { return false };
    let Ok(t) = t.parse::<u64>() else { return false };
    if t > now_ms.saturating_add(READ_WINDOW_MS) || now_ms > t.saturating_add(READ_WINDOW_MS) {
        return false;
    }
    let Ok(sig) = crate::util::parse_hex_fixed::<65>(sig) else { return false };
    crate::sig::recover(&read_digest(g, account, t), &sig).is_ok_and(|a| a == *account)
}

/// 397's "exit-like" set of step 6: actions that never need an attestation.
pub fn exit_like(primary: &str, reduce_only: Option<bool>) -> bool {
    match primary {
        "PlaceOrder" => reduce_only == Some(true),
        "CancelOrder" | "CancelAll" | "Withdraw" | "RevokeAgent" => true,
        _ => false,
    }
}

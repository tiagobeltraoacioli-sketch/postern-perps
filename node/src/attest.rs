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
    file: Option<PathBuf>,
    /// step 6 enforced at admission
    pub required: bool,
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
        let mut a = Attestations { statements, vault: to_hex(vault), records: BTreeMap::new(), file: None, required: false };
        if let Some(p) = &file {
            if let Ok(txt) = std::fs::read_to_string(p) {
                for line in txt.lines().filter(|l| !l.trim().is_empty()) {
                    if let Ok(rec) = serde_json::from_str::<serde_json::Value>(line) {
                        // re-verified at load; expired ones are kept as records (their status says so)
                        let _ = a.accept(&rec, now_ms, true);
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

    fn accept(&mut self, record: &serde_json::Value, now_ms: u64, loading: bool) -> Result<Address, String> {
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
        if self.records.get(&account).is_some_and(|old| issued(old) > issued(record)) {
            return Err("an attestation issued later is already held for this account".into());
        }
        self.records.insert(account, record.clone());
        Ok(account)
    }

    /// Verifies and stores a record `{message, signature}`. Returns the account it speaks for.
    pub fn submit(&mut self, record: &serde_json::Value, now_ms: u64) -> Result<Address, String> {
        let account = self.accept(record, now_ms, false)?;
        if let Some(p) = &self.file {
            let line = serde_json::to_string(record).map_err(|e| e.to_string())?;
            let r = std::fs::OpenOptions::new().create(true).append(true).open(p).and_then(|mut f| {
                writeln!(f, "{line}")?;
                f.sync_data()
            });
            if let Err(e) = r {
                self.records.remove(&account);
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

/// 397's "exit-like" set of step 6: actions that never need an attestation.
pub fn exit_like(primary: &str, reduce_only: Option<bool>) -> bool {
    match primary {
        "PlaceOrder" => reduce_only == Some(true),
        "CancelOrder" | "CancelAll" | "Withdraw" | "RevokeAgent" => true,
        _ => false,
    }
}

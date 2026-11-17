//! Checkpoints: what the k-of-n validators sign, how signatures are collected, and the verifier that mirrors
//! what the vault does (bridge `_checkSignatures` pattern: exactly `threshold` 65-byte signatures, strictly
//! ascending signer addresses, each a committee member, low-s, v in {27, 28}).
//!
//! PROPOSED digest (for agent 390's `docs/VAULT.md`; extends the 03:50 UTC proposal (B) with `withdrawalCount`,
//! which the Merkle verifier needs): EIP-712 under the VAULT domain
//! `{name "PosternPerpsVault", version "1", chainId 700770, verifyingContract vault}`, primary type
//! `Checkpoint(uint64 height,uint64 time,bytes32 stateRoot,bytes32 logHash,bytes32 withdrawalsRoot,uint64 withdrawalCount,uint64 depositsCredited,uint64 forcedProcessed)`.
//! The engine's user-action domain (`PosternPerps`, chain 7007701, no contract) is a different domain separator,
//! so no user signature can ever be a checkpoint signature and vice versa.
//!
//! UNAUDITED PROTOTYPE.

use crate::eip712::{digest, Domain};
use crate::genesis::Genesis;
use crate::sig::{recover, sign};
use crate::util::{keccak, parse_hex_fixed, to_hex, word_u128, Address, H256};
use k256::ecdsa::SigningKey;
use std::collections::BTreeMap;

pub const CHECKPOINT_TYPE: &str =
    "Checkpoint(uint64 height,uint64 time,bytes32 stateRoot,bytes32 logHash,bytes32 withdrawalsRoot,uint64 withdrawalCount,uint64 depositsCredited,uint64 forcedProcessed)";

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CheckpointBody {
    pub height: u64,
    pub time: u64,
    pub state_root: H256,
    pub log_hash: H256,
    pub withdrawals_root: H256,
    pub withdrawal_count: u64,
    pub deposits_credited: u64,
    pub forced_processed: u64,
}

impl CheckpointBody {
    pub fn struct_hash(&self) -> H256 {
        let mut b = Vec::with_capacity(32 * 9);
        b.extend_from_slice(&keccak(CHECKPOINT_TYPE.as_bytes()));
        b.extend_from_slice(&word_u128(self.height as u128));
        b.extend_from_slice(&word_u128(self.time as u128));
        b.extend_from_slice(&self.state_root);
        b.extend_from_slice(&self.log_hash);
        b.extend_from_slice(&self.withdrawals_root);
        b.extend_from_slice(&word_u128(self.withdrawal_count as u128));
        b.extend_from_slice(&word_u128(self.deposits_credited as u128));
        b.extend_from_slice(&word_u128(self.forced_processed as u128));
        keccak(&b)
    }

    pub fn digest(&self, vault_domain: &Domain) -> H256 {
        digest(&vault_domain.separator(), &self.struct_hash())
    }

    pub fn to_json(&self) -> serde_json::Value {
        serde_json::json!({
            "height": self.height, "time": self.time, "stateRoot": to_hex(&self.state_root), "logHash": to_hex(&self.log_hash),
            "withdrawalsRoot": to_hex(&self.withdrawals_root), "withdrawalCount": self.withdrawal_count,
            "depositsCredited": self.deposits_credited, "forcedProcessed": self.forced_processed
        })
    }

    pub fn from_json(j: &serde_json::Value) -> Result<CheckpointBody, String> {
        let u = |k: &str| j.get(k).and_then(|v| v.as_u64()).ok_or_else(|| format!("checkpoint.{k}"));
        let h = |k: &str| -> Result<H256, String> { parse_hex_fixed::<32>(j.get(k).and_then(|v| v.as_str()).ok_or_else(|| format!("checkpoint.{k}"))?) };
        Ok(CheckpointBody {
            height: u("height")?,
            time: u("time")?,
            state_root: h("stateRoot")?,
            log_hash: h("logHash")?,
            withdrawals_root: h("withdrawalsRoot")?,
            withdrawal_count: u("withdrawalCount")?,
            deposits_credited: u("depositsCredited")?,
            forced_processed: u("forcedProcessed")?,
        })
    }
}

pub fn sign_checkpoint(key: &SigningKey, body: &CheckpointBody, vault_domain: &Domain) -> [u8; 65] {
    sign(key, &body.digest(vault_domain))
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum CertError {
    Count { got: usize, need: usize },
    NotAscending,
    NotMember(Address),
    Signature(String),
}

impl std::fmt::Display for CertError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            CertError::Count { got, need } => write!(f, "exactly {need} signatures required, got {got}"),
            CertError::NotAscending => write!(f, "signers not strictly ascending"),
            CertError::NotMember(a) => write!(f, "signer {} is not in the committee", to_hex(a)),
            CertError::Signature(e) => write!(f, "bad signature: {e}"),
        }
    }
}

/// The vault's check, reproduced: exactly `threshold` signatures, recovered signers strictly ascending and members.
pub fn verify_signatures(committee: &[Address], threshold: usize, d: &H256, sigs: &[[u8; 65]]) -> Result<Vec<Address>, CertError> {
    if sigs.len() != threshold {
        return Err(CertError::Count { got: sigs.len(), need: threshold });
    }
    let mut out = Vec::with_capacity(sigs.len());
    for s in sigs {
        let a = recover(d, s).map_err(|e| CertError::Signature(e.to_string()))?;
        if out.last().is_some_and(|l: &Address| a <= *l) {
            return Err(CertError::NotAscending);
        }
        if committee.binary_search(&a).is_err() {
            return Err(CertError::NotMember(a));
        }
        out.push(a);
    }
    Ok(out)
}

/// Collected signatures for one checkpoint body. Only a signature that recovers to a committee member over THIS
/// body's digest is stored; a second, different signature by the same validator is kept as equivocation
/// evidence (two valid signatures by one key over two different bodies at one height are never both stored as
/// votes).
#[derive(Clone, Debug)]
pub struct Collected {
    pub body: CheckpointBody,
    pub digest: H256,
    pub sigs: BTreeMap<Address, [u8; 65]>,
}

impl Collected {
    pub fn final_signatures(&self, threshold: usize) -> Option<Vec<(Address, [u8; 65])>> {
        if self.sigs.len() < threshold {
            return None;
        }
        Some(self.sigs.iter().take(threshold).map(|(a, s)| (*a, *s)).collect())
    }
}

#[derive(Default)]
pub struct Aggregator {
    /// height -> collected signatures for the body the local engine computed
    pub by_height: BTreeMap<u64, Collected>,
    pub latest_final: Option<u64>,
    /// (validator, height) -> conflicting digest it signed (equivocation or divergence evidence)
    pub conflicts: Vec<(Address, u64, H256)>,
}

#[derive(Debug, PartialEq, Eq)]
pub enum AddResult {
    Added { now_final: bool },
    Duplicate,
    Conflict,
}

impl Aggregator {
    /// Registers the body this node computed for `height` (the only body it will collect votes for).
    pub fn expect(&mut self, g: &Genesis, body: CheckpointBody) {
        let d = body.digest(&g.vault_domain);
        self.by_height.entry(body.height).or_insert(Collected { body, digest: d, sigs: BTreeMap::new() });
    }

    pub fn add(&mut self, g: &Genesis, height: u64, sig: [u8; 65]) -> Result<AddResult, String> {
        let c = self.by_height.get_mut(&height).ok_or("no checkpoint expected at this height")?;
        let a = recover(&c.digest, &sig).map_err(|e| e.to_string());
        let a = match a {
            Ok(a) if g.committee.binary_search(&a).is_ok() => a,
            _ => {
                return Err("signature does not recover to a committee member over this node's checkpoint body (a validator that computed a different root signs a different digest)".into())
            }
        };
        match c.sigs.get(&a) {
            Some(s) if *s == sig => return Ok(AddResult::Duplicate),
            Some(_) => {
                // two valid signatures by one key over the same digest: ECDSA nonce reuse or a non-deterministic
                // signer; keep the first, record it
                self.conflicts.push((a, height, c.digest));
                return Ok(AddResult::Conflict);
            }
            None => {}
        }
        c.sigs.insert(a, sig);
        let now_final = c.sigs.len() == g.threshold;
        if c.sigs.len() >= g.threshold && self.latest_final.is_none_or(|h| height > h) {
            self.latest_final = Some(height);
        }
        Ok(AddResult::Added { now_final })
    }

    pub fn checkpoint_json(&self, g: &Genesis, height: u64, block_hash: Option<H256>) -> Option<serde_json::Value> {
        let c = self.by_height.get(&height)?;
        let fin = c.final_signatures(g.threshold);
        let sigs = match &fin {
            Some(v) => v.clone(),
            None => c.sigs.iter().map(|(a, s)| (*a, *s)).collect(),
        };
        let mut j = c.body.to_json();
        j["digest"] = serde_json::Value::String(to_hex(&c.digest));
        j["threshold"] = serde_json::json!(g.threshold);
        j["final"] = serde_json::Value::Bool(fin.is_some());
        j["blockHash"] = match block_hash {
            Some(h) => serde_json::Value::String(to_hex(&h)),
            None => serde_json::Value::Null,
        };
        j["signatures"] = serde_json::Value::Array(
            sigs.iter().map(|(a, s)| serde_json::json!({"validator": to_hex(a), "signature": to_hex(s)})).collect(),
        );
        Some(j)
    }

    /// Latest final checkpoint at or below `height`.
    pub fn final_at_or_below(&self, g: &Genesis, height: u64) -> Option<u64> {
        self.by_height.range(..=height).rev().find(|(_, c)| c.sigs.len() >= g.threshold).map(|(h, _)| *h)
    }

    /// Drops collected entries older than `keep_from` except final ones (bounded memory).
    pub fn prune_unfinal_below(&mut self, g: &Genesis, keep_from: u64) {
        let th = g.threshold;
        self.by_height.retain(|h, c| *h >= keep_from || c.sigs.len() >= th);
    }
}

/// Verifies a checkpoint JSON object exactly as `docs/API.md` 3.2 promises a client can: from the committee and
/// the object only.
pub fn verify_checkpoint_json(g: &Genesis, j: &serde_json::Value) -> Result<CheckpointBody, String> {
    let body = CheckpointBody::from_json(j)?;
    let d = body.digest(&g.vault_domain);
    if let Some(ds) = j.get("digest").and_then(|v| v.as_str()) {
        if parse_hex_fixed::<32>(ds)? != d {
            return Err("digest field does not match the body".into());
        }
    }
    let sigs: Vec<[u8; 65]> = j
        .get("signatures")
        .and_then(|v| v.as_array())
        .ok_or("signatures")?
        .iter()
        .map(|s| parse_hex_fixed::<65>(s.get("signature").and_then(|v| v.as_str()).unwrap_or("")))
        .collect::<Result<_, _>>()?;
    verify_signatures(&g.committee, g.threshold, &d, &sigs).map_err(|e| e.to_string())?;
    Ok(body)
}

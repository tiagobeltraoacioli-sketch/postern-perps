//! Checkpoints: what the k-of-n validators sign, how signatures are collected, and the verifier that mirrors
//! what the vault does (bridge `_checkSignatures` pattern: exactly `threshold` 65-byte signatures, strictly
//! ascending signer addresses, each a committee member, low-s, v in {27, 28}).
//!
//! Digest: `docs/VAULT.md` v0.2 section 5.2 (agent 390, contracts 27c1969; vectors
//! `contracts/test/vectors/vault-vectors.json`): EIP-712 under the VAULT domain
//! `{name "PosternPerpsVault", version "1", chainId 700770, verifyingContract vault}`, primary type
//! `Checkpoint(uint64 height,uint64 time,bytes32 logHash,bytes32 accountsRoot,bytes32 marketsRoot,bytes32 withdrawalsRoot,uint64 nextDepositIndex,uint64 nextForcedIndex,uint64 nextOrderId,uint128 totalEscape,uint128 dust,bytes32 auxHash,bytes32 depositsHead,bytes32 forcedHead,uint64 withdrawalCount,uint64 withdrawalsTotal)`.
//! The first twelve fields plus withdrawalCount and withdrawalsTotal (SPEC 0.1.5 systemHash, VAULT 64072cb) are the inputs of SPEC 11's `stateRoot` (the vault recomputes it); the node checks the
//! same equation against the replayed block before signing (`CheckpointBody::state_root`).
//! The engine's user-action domain has another `name` (`PosternPerps`), hence another separator.
//!
//! UNAUDITED PROTOTYPE.

use crate::eip712::{digest, Domain};
use crate::genesis::Genesis;
use crate::sig::{recover, sign};
use crate::util::{keccak, keccak_parts, parse_hex_fixed, to_hex, word_u128, Address, H256};
use k256::ecdsa::SigningKey;
use std::collections::BTreeMap;

pub const CHECKPOINT_TYPE: &str = "Checkpoint(uint64 height,uint64 time,bytes32 logHash,bytes32 accountsRoot,bytes32 marketsRoot,bytes32 withdrawalsRoot,uint64 nextDepositIndex,uint64 nextForcedIndex,uint64 nextOrderId,uint128 totalEscape,uint128 dust,bytes32 auxHash,bytes32 depositsHead,bytes32 forcedHead,uint64 withdrawalCount,uint64 withdrawalsTotal)";

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CheckpointBody {
    pub height: u64,
    pub time: u64,
    pub log_hash: H256,
    pub accounts_root: H256,
    pub markets_root: H256,
    pub withdrawals_root: H256,
    pub next_deposit_index: u64,
    pub next_forced_index: u64,
    pub next_order_id: u64,
    pub total_escape: u128,
    /// two's complement of the engine's i128 dust (be128 in systemHash, uint128 in the typed data)
    pub dust: u128,
    pub aux_hash: H256,
    pub deposits_head: H256,
    pub forced_head: H256,
    pub withdrawal_count: u64,
    pub withdrawals_total: u64,
}

impl CheckpointBody {
    /// VAULT.md 5.2 / SPEC 11 state root from the first twelve fields.
    pub fn state_root(&self) -> H256 {
        let sys = keccak_parts(&[
            b"PPv1sys",
            &self.next_deposit_index.to_be_bytes(),
            &self.next_forced_index.to_be_bytes(),
            &self.next_order_id.to_be_bytes(),
            &self.total_escape.to_be_bytes(),
            &self.dust.to_be_bytes(),
            &self.aux_hash,
            &self.withdrawal_count.to_be_bytes(),
            &self.withdrawals_total.to_be_bytes(),
        ]);
        keccak_parts(&[
            b"PosternPerps/state/v1",
            &self.height.to_be_bytes(),
            &self.time.to_be_bytes(),
            &self.accounts_root,
            &self.markets_root,
            &self.withdrawals_root,
            &sys,
        ])
    }

    pub fn struct_hash(&self) -> H256 {
        let mut b = Vec::with_capacity(32 * 17);
        b.extend_from_slice(&keccak(CHECKPOINT_TYPE.as_bytes()));
        b.extend_from_slice(&word_u128(self.height as u128));
        b.extend_from_slice(&word_u128(self.time as u128));
        b.extend_from_slice(&self.log_hash);
        b.extend_from_slice(&self.accounts_root);
        b.extend_from_slice(&self.markets_root);
        b.extend_from_slice(&self.withdrawals_root);
        b.extend_from_slice(&word_u128(self.next_deposit_index as u128));
        b.extend_from_slice(&word_u128(self.next_forced_index as u128));
        b.extend_from_slice(&word_u128(self.next_order_id as u128));
        b.extend_from_slice(&word_u128(self.total_escape));
        b.extend_from_slice(&word_u128(self.dust));
        b.extend_from_slice(&self.aux_hash);
        b.extend_from_slice(&self.deposits_head);
        b.extend_from_slice(&self.forced_head);
        b.extend_from_slice(&word_u128(self.withdrawal_count as u128));
        b.extend_from_slice(&word_u128(self.withdrawals_total as u128));
        keccak(&b)
    }

    pub fn digest(&self, vault_domain: &Domain) -> H256 {
        digest(&vault_domain.separator(), &self.struct_hash())
    }

    pub fn to_json(&self) -> serde_json::Value {
        serde_json::json!({
            "height": self.height, "time": self.time, "logHash": to_hex(&self.log_hash), "stateRoot": to_hex(&self.state_root()),
            "accountsRoot": to_hex(&self.accounts_root), "marketsRoot": to_hex(&self.markets_root), "withdrawalsRoot": to_hex(&self.withdrawals_root),
            "nextDepositIndex": self.next_deposit_index, "nextForcedIndex": self.next_forced_index, "nextOrderId": self.next_order_id.to_string(),
            "totalEscape": self.total_escape.to_string(), "dust": self.dust.to_string(), "auxHash": to_hex(&self.aux_hash),
            "depositsHead": to_hex(&self.deposits_head), "forcedHead": to_hex(&self.forced_head),
            "withdrawalCount": self.withdrawal_count, "withdrawalsTotal": self.withdrawals_total.to_string()
        })
    }

    pub fn from_json(j: &serde_json::Value) -> Result<CheckpointBody, String> {
        let u = |k: &str| -> Result<u128, String> {
            let v = j.get(k).ok_or_else(|| format!("checkpoint.{k}"))?;
            v.as_u64().map(|x| x as u128).or_else(|| v.as_str().and_then(|s| s.parse().ok())).ok_or_else(|| format!("checkpoint.{k}"))
        };
        let u64f = |k: &str| -> Result<u64, String> { u64::try_from(u(k)?).map_err(|_| format!("checkpoint.{k} above u64")) };
        let h = |k: &str| -> Result<H256, String> { parse_hex_fixed::<32>(j.get(k).and_then(|v| v.as_str()).ok_or_else(|| format!("checkpoint.{k}"))?) };
        let b = CheckpointBody {
            height: u64f("height")?,
            time: u64f("time")?,
            log_hash: h("logHash")?,
            accounts_root: h("accountsRoot")?,
            markets_root: h("marketsRoot")?,
            withdrawals_root: h("withdrawalsRoot")?,
            next_deposit_index: u64f("nextDepositIndex")?,
            next_forced_index: u64f("nextForcedIndex")?,
            next_order_id: u64f("nextOrderId")?,
            total_escape: u("totalEscape")?,
            dust: u("dust")?,
            aux_hash: h("auxHash")?,
            deposits_head: h("depositsHead")?,
            forced_head: h("forcedHead")?,
            withdrawal_count: u64f("withdrawalCount")?,
            withdrawals_total: u64f("withdrawalsTotal")?,
        };
        if let Some(sr) = j.get("stateRoot").and_then(|v| v.as_str()) {
            if parse_hex_fixed::<32>(sr)? != b.state_root() {
                return Err("stateRoot field does not equal the root of the checkpoint's components".into());
            }
        }
        Ok(b)
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
            CertError::Count { got, need } => write!(f, "at least {need} signatures required, got {got}"),
            CertError::NotAscending => write!(f, "signers not strictly ascending"),
            CertError::NotMember(a) => write!(f, "signer {} is not in the committee", to_hex(a)),
            CertError::Signature(e) => write!(f, "bad signature: {e}"),
        }
    }
}

/// The vault's check, reproduced: at least `threshold` signatures (the node itself always publishes exactly
/// `threshold`), recovered signers strictly ascending and committee members.
pub fn verify_signatures(committee: &[Address], threshold: usize, d: &H256, sigs: &[[u8; 65]]) -> Result<Vec<Address>, CertError> {
    if sigs.len() < threshold || sigs.len() > committee.len() {
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

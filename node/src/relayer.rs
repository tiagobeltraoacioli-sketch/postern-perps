//! The checkpoint relayer: the keeper of `docs/VAULT.md` section 8a (R395-19). It submits final checkpoints to the
//! PerpsVault on the L2 with a gas-only hot key. It has NO power over the vault: `submitCheckpoint` takes any
//! caller, and the vault accepts a checkpoint only with `threshold` committee signatures over a body that passes
//! its own rules. A lying or dead relayer can only fail to submit; what that costs is 8a's subject.
//!
//! When a checkpoint is submitted (the newest FINAL one of this node, whenever ANY holds; 8a "How often"):
//!
//! 1. `withdrawal`: its `withdrawalCount` exceeds the vault's (a user waits for a record);
//! 2. `forced`: its `nextForcedIndex` exceeds the vault's (a forced request was processed): at once. This is the
//!    safety bound: a processed forced request the vault does not know of lets anyone trigger escape at the
//!    request's deadline;
//! 3. `heartbeat`: `forcedDelay / 4` of L2 time passed since the vault's `lastCheckpointAt` (not `escapeDelay / 4`:
//!    any depositor can shorten the venue's time to `forcedDelay` with a forced request).
//!
//! Nothing is submitted while the vault has no checkpoint and none of 1 and 2 holds (the silence clock starts with
//! the first accepted checkpoint), nor when the vault is in escape (checkpoints are refused for good).
//!
//! THE L2 ENDPOINTS ARE NOT TRUSTED ONE BY ONE (R436-15; the watcher's own standard applied to the relayer):
//!
//! - what the relayer READS is the vault's state at the L2's `finalized` block, and only when EVERY configured
//!   endpoint (at least two, of independent operators) names the same finalized block (number and hash) and
//!   returns the same vault state at it (`read_vault_agreed`). Disagreement is an error: nothing is decided on it;
//! - what they agree on is then checked against what THIS NODE knows (`plausible`): the vault can only hold a
//!   checkpoint this node finalized, with that checkpoint's own withdrawal count and forced index. A reading
//!   that names anything else ("the vault is already ahead") is refused as a lie, however many endpoints tell it;
//! - the gas price is twice the HIGHEST `eth_gasPrice` any endpoint names, and never above the configured cap
//!   (`--relay-max-gas-price-gwei`): above it nothing is signed and the relayer alarms (fail closed). With the gas
//!   limit capped at 15,000,000 one transaction can never cost more than `15,000,000 x cap`;
//! - a submission counts only when EVERY endpoint returns the transaction's receipt with status 1.
//!
//! Retry rule: a submission that fails (estimate reverted, price above the cap, send refused, no receipt on every
//! endpoint in time, a receipt with status 0) is retried at the next poll with the then-newest final checkpoint,
//! sent through the next endpoint of the list, after a backoff that doubles from the poll interval up to 30 s.
//! After a confirmed submission the same checkpoint is not sent again for `resubmit_after_ms` (the finalized
//! reading lags the transaction); if the finalized vault still does not hold it then, the rules fire again.
//!
//! Gas limit (F434-1: the estimate of a vault call can be the exact minimum and the next block needs more):
//! `gas = estimate * 3 / 2 + 40,000`, at most 15,000,000. Transactions are legacy (EIP-155), chain id 700770.
//!
//! ALARMS (stderr and `/v1/node`). R395-19: a silent relayer must be loud, and its alarm must not depend on an
//! endpoint being honest. Two kinds:
//! - from the agreed, plausible reading (8a): `forcedDelay / 2` of L2 time since an open forced request was
//!   posted without the vault knowing it processed; `escapeDelay / 2` since the vault's last checkpoint; escape;
//! - from THIS NODE'S OWN CLOCK AND KNOWLEDGE, whatever the endpoints say (`local_alarm`): no agreed and plausible
//!   reading for `stall_alarm_ms` (endpoints down, disagreeing, or lying); a checkpoint this node knows to be due
//!   (by its own last confirmed delivery) not confirmed for `delivery_alarm_ms`; a gas price above the cap.
//!
//! The frame (engine state) of the checkpoint the vault holds is pinned in the sequencer, so the account proofs
//! users need in escape stay available from this node (`Sequencer::pin_frame`).
//!
//! UNAUDITED PROTOTYPE. The key file holds a THROWAWAY key in every use of this prototype.

use crate::checkpoint::CheckpointBody;
use crate::node::App;
use crate::sig::{key_address, sign};
use crate::util::{keccak, parse_hex_vec, to_hex, Address, H256};
use crate::watcher::rpc;
use k256::ecdsa::SigningKey;
use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::Duration;

pub const L2_CHAIN_ID: u64 = 700_770;
pub const GAS_CAP: u64 = 15_000_000;
pub const PINNED_FRAMES: usize = 8;

/// What the relayer last learned from the vault, served by `/v1/status` (`vaultMode`) and `/v1/node`.
#[derive(Clone, Debug)]
pub struct RelayState {
    /// `normal`, `paused`, `escape`, or `unknown` (no relayer, or the vault was never read)
    pub vault_mode: &'static str,
    /// withdrawal id -> L2 transaction hash that paid it (not tracked in this prototype: empty)
    pub paid: BTreeMap<u64, String>,
    pub enabled: bool,
    pub address: Option<Address>,
    pub last_error: Option<String>,
    pub alarm: Option<String>,
    pub submitted: u64,
    pub accepted: u64,
    pub failures: u64,
    pub last_reason: Option<&'static str>,
    pub last_submitted_height: Option<u64>,
    pub last_tx: Option<String>,
    pub vault_checkpoints: Option<u64>,
    pub vault_latest_height: Option<u64>,
    pub vault_latest_height_finalized: Option<u64>,
    pub l2_time_s: Option<u64>,
    /// wall clock (ms) of the relayer's start and of its last agreed and plausible reading
    pub started_ms: u64,
    pub last_good_read_ms: Option<u64>,
    /// wall clock (ms) since when a checkpoint has been due and not confirmed delivered
    pub due_since_ms: Option<u64>,
    /// the 8a alarm of the last agreed and plausible reading
    pub view_alarm: Option<String>,
    /// the last gas price refused for being above the cap
    pub gas_refused: Option<String>,
    /// (height, wall ms) of the last submission every endpoint confirmed
    pub confirmed: Option<(u64, u64)>,
}

impl Default for RelayState {
    fn default() -> Self {
        RelayState {
            vault_mode: "unknown",
            paid: BTreeMap::new(),
            enabled: false,
            address: None,
            last_error: None,
            alarm: None,
            submitted: 0,
            accepted: 0,
            failures: 0,
            last_reason: None,
            last_submitted_height: None,
            last_tx: None,
            vault_checkpoints: None,
            vault_latest_height: None,
            vault_latest_height_finalized: None,
            l2_time_s: None,
            started_ms: 0,
            last_good_read_ms: None,
            due_since_ms: None,
            view_alarm: None,
            gas_refused: None,
            confirmed: None,
        }
    }
}

impl RelayState {
    pub fn to_json(&self) -> serde_json::Value {
        serde_json::json!({"enabled": self.enabled, "address": self.address.map(|a| to_hex(&a)), "vaultMode": self.vault_mode,
            "submitted": self.submitted, "accepted": self.accepted, "failures": self.failures, "lastReason": self.last_reason,
            "lastSubmittedHeight": self.last_submitted_height, "lastTx": self.last_tx, "vaultCheckpoints": self.vault_checkpoints,
            "vaultLatestHeight": self.vault_latest_height, "vaultLatestHeightFinalized": self.vault_latest_height_finalized,
            "l2TimeS": self.l2_time_s, "alarm": self.alarm, "lastError": self.last_error,
            "lastGoodReadMs": self.last_good_read_ms, "dueSinceMs": self.due_since_ms, "gasRefused": self.gas_refused,
            "lastConfirmedHeight": self.confirmed.map(|c| c.0)})
    }
}

// ------------------------------------------------------------------------------------------------ RLP, EIP-155

fn rlp_len_prefix(len: usize, short: u8, long: u8, out: &mut Vec<u8>) {
    if len <= 55 {
        out.push(short + len as u8);
    } else {
        let be = (len as u64).to_be_bytes();
        let skip = be.iter().take_while(|b| **b == 0).count();
        out.push(long + (8 - skip) as u8);
        out.extend_from_slice(&be[skip..]);
    }
}

/// RLP of a byte string.
pub fn rlp_bytes(b: &[u8], out: &mut Vec<u8>) {
    if b.len() == 1 && b[0] < 0x80 {
        out.push(b[0]);
    } else {
        rlp_len_prefix(b.len(), 0x80, 0xb7, out);
        out.extend_from_slice(b);
    }
}

/// RLP of an unsigned integer (big-endian, no leading zeros; zero is the empty string).
pub fn rlp_uint(v: u128, out: &mut Vec<u8>) {
    let be = v.to_be_bytes();
    let skip = be.iter().take_while(|b| **b == 0).count();
    rlp_bytes(&be[skip..], out);
}

pub fn rlp_list(items: &[u8], out: &mut Vec<u8>) {
    rlp_len_prefix(items.len(), 0xc0, 0xf7, out);
    out.extend_from_slice(items);
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LegacyTx {
    pub nonce: u64,
    pub gas_price: u128,
    pub gas: u64,
    pub to: Address,
    pub value: u128,
    pub data: Vec<u8>,
    pub chain_id: u64,
}

impl LegacyTx {
    fn fields(&self, out: &mut Vec<u8>) {
        rlp_uint(self.nonce as u128, out);
        rlp_uint(self.gas_price, out);
        rlp_uint(self.gas as u128, out);
        rlp_bytes(&self.to, out);
        rlp_uint(self.value, out);
        rlp_bytes(&self.data, out);
    }

    /// EIP-155: `rlp([nonce, gasPrice, gas, to, value, data, chainId, 0, 0])`.
    pub fn unsigned_rlp(&self) -> Vec<u8> {
        let mut items = Vec::new();
        self.fields(&mut items);
        rlp_uint(self.chain_id as u128, &mut items);
        rlp_uint(0, &mut items);
        rlp_uint(0, &mut items);
        let mut out = Vec::new();
        rlp_list(&items, &mut out);
        out
    }

    pub fn signing_hash(&self) -> H256 {
        keccak(&self.unsigned_rlp())
    }

    /// The raw signed transaction: `rlp([..., v = recid + 35 + 2 chainId, r, s])` (low-s by `sig::sign`).
    pub fn sign(&self, key: &SigningKey) -> Vec<u8> {
        let sig = sign(key, &self.signing_hash());
        let v = (sig[64] - 27) as u128 + 35 + 2 * self.chain_id as u128;
        let strip = |b: &[u8]| -> Vec<u8> { b.iter().skip_while(|x| **x == 0).copied().collect() };
        let mut items = Vec::new();
        self.fields(&mut items);
        rlp_uint(v, &mut items);
        rlp_bytes(&strip(&sig[..32]), &mut items);
        rlp_bytes(&strip(&sig[32..64]), &mut items);
        let mut out = Vec::new();
        rlp_list(&items, &mut out);
        out
    }
}

// ------------------------------------------------------------------------------------------------ ABI

pub fn selector(signature: &str) -> [u8; 4] {
    keccak(signature.as_bytes())[..4].try_into().unwrap()
}

pub const SUBMIT_CHECKPOINT: &str = "submitCheckpoint((uint64,uint64,bytes32,bytes32,bytes32,bytes32,uint64,uint64,uint64,uint128,uint128,bytes32,bytes32,bytes32,uint64,uint64),bytes[])";

fn word(v: u128) -> [u8; 32] {
    let mut w = [0u8; 32];
    w[16..].copy_from_slice(&v.to_be_bytes());
    w
}

/// Calldata of `submitCheckpoint(Checkpoint cp, bytes[] signatures)`: the sixteen fields of VAULT.md 5.2 in the
/// struct's order (a static tuple, encoded in place), then the dynamic array of 65-byte signatures.
pub fn encode_submit_checkpoint(b: &CheckpointBody, sigs: &[[u8; 65]]) -> Vec<u8> {
    let mut out = Vec::with_capacity(4 + 32 * (18 + sigs.len() * 5));
    out.extend_from_slice(&selector(SUBMIT_CHECKPOINT));
    for w in [
        word(b.height as u128),
        word(b.time as u128),
        b.log_hash,
        b.accounts_root,
        b.markets_root,
        b.withdrawals_root,
        word(b.next_deposit_index as u128),
        word(b.next_forced_index as u128),
        word(b.next_order_id as u128),
        word(b.total_escape),
        word(b.dust),
        b.aux_hash,
        b.deposits_head,
        b.forced_head,
        word(b.withdrawal_count as u128),
        word(b.withdrawals_total as u128),
    ] {
        out.extend_from_slice(&w);
    }
    // offset of the bytes[] from the start of the arguments: 16 static words + this offset word
    out.extend_from_slice(&word(17 * 32));
    out.extend_from_slice(&word(sigs.len() as u128));
    // each element: offset from the start of the offsets area; an element is length word + 96 bytes (65 padded)
    for i in 0..sigs.len() {
        out.extend_from_slice(&word((sigs.len() * 32 + i * 128) as u128));
    }
    for s in sigs {
        out.extend_from_slice(&word(65));
        out.extend_from_slice(s);
        out.extend_from_slice(&[0u8; 31]);
    }
    out
}

// ------------------------------------------------------------------------------------------------ vault reads

/// What the relayer reads from the vault at one block tag.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct VaultView {
    pub checkpoint_count: u64,
    pub latest_height: u64,
    pub latest_next_forced: u64,
    pub latest_withdrawal_count: u64,
    pub last_checkpoint_at: u64,
    pub forced_count: u64,
    /// deadline (L2 seconds) of the first forced request the vault's latest checkpoint has not processed
    pub open_forced_deadline: Option<u64>,
    pub forced_delay: u64,
    pub escape_delay: u64,
    pub paused: bool,
    pub in_escape: bool,
    pub l2_time_s: u64,
}

fn hex_qty(v: &serde_json::Value) -> Result<u64, String> {
    let s = v.as_str().ok_or("expected a hex quantity")?;
    u64::from_str_radix(s.strip_prefix("0x").ok_or("expected 0x")?, 16).map_err(|_| format!("bad quantity {s}"))
}

async fn call(url: &str, vault: &Address, data: &[u8], tag: &str) -> Result<Vec<u8>, String> {
    let r = rpc(url, "eth_call", serde_json::json!([{"to": to_hex(vault), "data": to_hex(data)}, tag])).await?;
    parse_hex_vec(r.as_str().ok_or("eth_call result")?)
}

fn ret_u64(ret: &[u8], i: usize) -> Result<u64, String> {
    let w = ret.get(i * 32..i * 32 + 32).ok_or("short return data")?;
    if w[..24].iter().any(|b| *b != 0) {
        return Err("returned value does not fit 64 bits".into());
    }
    Ok(u64::from_be_bytes(w[24..].try_into().unwrap()))
}

async fn view_u64(url: &str, vault: &Address, sig: &str, tag: &str) -> Result<u64, String> {
    ret_u64(&call(url, vault, &selector(sig), tag).await.map_err(|e| format!("{sig}: {e}"))?, 0)
}

pub async fn read_vault(url: &str, vault: &Address, tag: &str) -> Result<VaultView, String> {
    let chain = hex_qty(&rpc(url, "eth_chainId", serde_json::json!([])).await?)?;
    if chain != L2_CHAIN_ID {
        return Err(format!("endpoint reports chain {chain}, not {L2_CHAIN_ID}"));
    }
    let code = rpc(url, "eth_getCode", serde_json::json!([to_hex(vault), tag])).await?;
    if code.as_str().is_none_or(|c| c.len() <= 2) {
        return Err("no contract code at the vault address".into());
    }
    // Stored public latest: (height, time, nextDepositIndex, nextForcedIndex, withdrawalCount, withdrawalsTotal,
    // totalEscape, accountsRoot, stateRoot)
    let latest = call(url, vault, &selector("latest()"), tag).await.map_err(|e| format!("latest(): {e}"))?;
    let mut v = VaultView {
        checkpoint_count: view_u64(url, vault, "checkpointCount()", tag).await?,
        latest_height: ret_u64(&latest, 0)?,
        latest_next_forced: ret_u64(&latest, 3)?,
        latest_withdrawal_count: ret_u64(&latest, 4)?,
        last_checkpoint_at: view_u64(url, vault, "lastCheckpointAt()", tag).await?,
        forced_count: view_u64(url, vault, "forcedCount()", tag).await?,
        open_forced_deadline: None,
        forced_delay: view_u64(url, vault, "forcedDelay()", tag).await?,
        escape_delay: view_u64(url, vault, "escapeDelay()", tag).await?,
        paused: view_u64(url, vault, "paused()", tag).await? != 0,
        in_escape: view_u64(url, vault, "inEscape()", tag).await? != 0,
        l2_time_s: 0,
    };
    if v.forced_count > v.latest_next_forced {
        let mut data = selector("forcedDeadline(uint64)").to_vec();
        data.extend_from_slice(&word(v.latest_next_forced as u128));
        v.open_forced_deadline = Some(ret_u64(&call(url, vault, &data, tag).await.map_err(|e| format!("forcedDeadline: {e}"))?, 0)?);
    }
    let blk = rpc(url, "eth_getBlockByNumber", serde_json::json!([tag, false])).await?;
    v.l2_time_s = hex_qty(blk.get("timestamp").ok_or("block.timestamp")?)?;
    Ok(v)
}

// ------------------------------------------------------------------------------------------------ the rule

/// VAULT.md 8a: must the relayer submit checkpoint `cp` (this node's newest final one) to a vault in state `v`?
pub fn due(v: &VaultView, cp: &CheckpointBody) -> Option<&'static str> {
    if v.in_escape {
        return None; // checkpoints are refused for good
    }
    if v.checkpoint_count > 0 && cp.height <= v.latest_height {
        return None; // the vault already holds this one or a newer one
    }
    if cp.withdrawal_count > v.latest_withdrawal_count {
        return Some("withdrawal");
    }
    if cp.next_forced_index > v.latest_next_forced {
        return Some("forced");
    }
    if v.checkpoint_count > 0 && v.l2_time_s >= v.last_checkpoint_at.saturating_add(v.forced_delay / 4) {
        return Some("heartbeat");
    }
    None
}

/// 8a alarms: an open forced request older than `forcedDelay / 2`, else silence longer than `escapeDelay / 2`.
pub fn alarm(v: &VaultView) -> Option<String> {
    if v.in_escape {
        return Some("the vault is in ESCAPE: checkpoints are refused for good".into());
    }
    if let Some(deadline) = v.open_forced_deadline {
        let posted = deadline.saturating_sub(v.forced_delay);
        if v.l2_time_s >= posted.saturating_add(v.forced_delay / 2) {
            return Some(format!("forced request {} is open on the vault for more than forcedDelay / 2 (deadline {deadline}, L2 time {}): anyone can trigger escape at the deadline", v.latest_next_forced, v.l2_time_s));
        }
    }
    if v.checkpoint_count > 0 && v.l2_time_s >= v.last_checkpoint_at.saturating_add(v.escape_delay / 2) {
        return Some(format!("no checkpoint accepted by the vault for more than escapeDelay / 2 (last at {}, L2 time {})", v.last_checkpoint_at, v.l2_time_s));
    }
    None
}

/// F434-1: the gas limit set for a vault call from its estimate.
pub fn padded_gas(estimate: u64) -> u64 {
    (estimate.saturating_mul(3) / 2).saturating_add(40_000).min(GAS_CAP)
}

// ------------------------------------------------------------------------------------------------ the loop

pub struct RelayConfig {
    /// L2 JSON-RPC endpoints (plain http, loopback or a private network) of INDEPENDENT operators: at least two.
    /// Every read needs all of them to agree; transactions are sent through them in turn
    pub endpoints: Vec<String>,
    pub key: SigningKey,
    pub vault: Address,
    pub poll_ms: u64,
    pub receipt_timeout_ms: u64,
    /// the highest gas price (wei) this relayer ever signs
    pub max_gas_price_wei: u128,
    /// how long a confirmed submission is trusted before the finalized reading must show it
    pub resubmit_after_ms: u64,
    /// alarm when no agreed and plausible reading was had for this long (wall clock)
    pub stall_alarm_ms: u64,
    /// alarm when a due checkpoint is not confirmed delivered for this long (wall clock)
    pub delivery_alarm_ms: u64,
}

pub const DEFAULT_MAX_GAS_PRICE_WEI: u128 = 50_000_000_000;
pub const DEFAULT_RESUBMIT_AFTER_MS: u64 = 120_000;
pub const DEFAULT_STALL_ALARM_MS: u64 = 120_000;
pub const DEFAULT_DELIVERY_ALARM_MS: u64 = 600_000;

impl RelayConfig {
    pub fn new(endpoints: Vec<String>, key: SigningKey, vault: Address) -> Result<RelayConfig, String> {
        let mut distinct = endpoints.clone();
        distinct.sort();
        distinct.dedup();
        if distinct.len() < 2 {
            return Err("the relayer needs at least two distinct L2 endpoints (independent operators): it decides only on what they agree on (R436-15)".into());
        }
        Ok(RelayConfig {
            endpoints,
            key,
            vault,
            poll_ms: 2000,
            receipt_timeout_ms: 30_000,
            max_gas_price_wei: DEFAULT_MAX_GAS_PRICE_WEI,
            resubmit_after_ms: DEFAULT_RESUBMIT_AFTER_MS,
            stall_alarm_ms: DEFAULT_STALL_ALARM_MS,
            delivery_alarm_ms: DEFAULT_DELIVERY_ALARM_MS,
        })
    }
}

/// Reads a hex private key (64 hex digits, optional 0x) from a file readable by its owner only.
pub fn read_key_file(path: &std::path::Path) -> Result<SigningKey, String> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = std::fs::metadata(path).map_err(|e| format!("relay key file: {e}"))?.permissions().mode();
        if mode & 0o077 != 0 {
            return Err("relay key file must not be readable by group or others (chmod 600)".into());
        }
    }
    let raw = std::fs::read_to_string(path).map_err(|e| format!("relay key file: {e}"))?;
    let hexs = raw.trim().trim_start_matches("0x");
    let bytes = hex::decode(hexs).map_err(|_| "relay key file: not hex".to_string())?;
    if bytes.len() != 32 {
        return Err("relay key file: 32 bytes expected".into());
    }
    SigningKey::from_slice(&bytes).map_err(|_| "relay key file: not a valid secp256k1 key".to_string())
}

/// The vault's state at the L2's `finalized` block as EVERY endpoint reports it: the same block (number and
/// hash) and the same state, or an error. Nothing is decided on one endpoint's word.
pub async fn read_vault_agreed(endpoints: &[String], vault: &Address) -> Result<VaultView, String> {
    if endpoints.len() < 2 {
        return Err("at least two L2 endpoints are needed".into());
    }
    let mut pinned: Option<(u64, String)> = None;
    for url in endpoints {
        let b = rpc(url, "eth_getBlockByNumber", serde_json::json!(["finalized", false])).await.map_err(|e| format!("{url}: {e}"))?;
        let n = hex_qty(b.get("number").ok_or("finalized block without a number")?)?;
        let h = b.get("hash").and_then(|h| h.as_str()).ok_or("finalized block without a hash")?.to_string();
        match &pinned {
            None => pinned = Some((n, h)),
            Some(p) if *p != (n, h.clone()) => return Err(format!("the L2 endpoints disagree on the finalized block ({} {} vs {n} {h})", p.0, p.1)),
            _ => {}
        }
    }
    let (n, _) = pinned.expect("two endpoints");
    let tag = format!("0x{n:x}");
    let mut agreed: Option<VaultView> = None;
    for url in endpoints {
        let v = read_vault(url, vault, &tag).await.map_err(|e| format!("{url}: {e}"))?;
        match &agreed {
            None => agreed = Some(v),
            Some(a) if *a != v => return Err(format!("the L2 endpoints disagree on the vault's state at finalized block {n}")),
            _ => {}
        }
    }
    Ok(agreed.expect("two endpoints"))
}

/// Is the agreed reading something THIS node can have caused? `final_at(height)` gives the node's own final
/// checkpoint at that height. The vault accepts only checkpoints the committee signed, and this node holds every
/// one of them: a reading that names a height the node never finalized, or other counts than that checkpoint
/// carries, is a lie (or another deployment), whoever tells it.
pub fn plausible(v: &VaultView, final_at: &dyn Fn(u64) -> Option<CheckpointBody>) -> Result<(), String> {
    if v.checkpoint_count == 0 {
        if v.latest_height != 0 || v.latest_withdrawal_count != 0 || v.latest_next_forced != 0 || v.last_checkpoint_at != 0 {
            return Err("the endpoints report no accepted checkpoint and yet a latest checkpoint".into());
        }
        return Ok(());
    }
    let Some(cp) = final_at(v.latest_height) else {
        return Err(format!("the endpoints report a vault checkpoint at height {} that this node never finalized: not believed", v.latest_height));
    };
    if cp.withdrawal_count != v.latest_withdrawal_count || cp.next_forced_index != v.latest_next_forced {
        return Err(format!("the endpoints report a vault checkpoint at height {} with other counts than this node's checkpoint there: not believed", v.latest_height));
    }
    if v.last_checkpoint_at == 0 || v.last_checkpoint_at > v.l2_time_s {
        return Err("the endpoints report an acceptance time that is zero or in the L2's future: not believed".into());
    }
    Ok(())
}

/// The alarms that depend on nothing an endpoint says: this node's own clock and what it knows it is owed.
pub fn local_alarm(r: &RelayState, cfg: &RelayConfig, now_ms: u64) -> Option<String> {
    if let Some(g) = &r.gas_refused {
        return Some(format!("NOT SUBMITTING: {g}"));
    }
    let last_good = r.last_good_read_ms.unwrap_or(r.started_ms);
    if now_ms.saturating_sub(last_good) >= cfg.stall_alarm_ms {
        return Some(format!(
            "no agreed and plausible reading of the vault from the L2 endpoints for {} s (endpoints down, disagreeing or lying; last error: {}): checkpoints are NOT being delivered",
            now_ms.saturating_sub(last_good) / 1000,
            r.last_error.as_deref().unwrap_or("none")
        ));
    }
    if let Some(since) = r.due_since_ms {
        if now_ms.saturating_sub(since) >= cfg.delivery_alarm_ms {
            return Some(format!("a checkpoint has been due for {} s and its delivery to the vault is not confirmed (last error: {})", now_ms.saturating_sub(since) / 1000, r.last_error.as_deref().unwrap_or("none")));
        }
    }
    None
}

/// The gas price to sign: twice the highest price any endpoint names, refused above the cap (R436-15).
pub async fn capped_gas_price(cfg: &RelayConfig) -> Result<u128, String> {
    let mut highest = 0u128;
    for url in &cfg.endpoints {
        let p = rpc(url, "eth_gasPrice", serde_json::json!([])).await.map_err(|e| format!("{url}: {e}"))?;
        let s = p.as_str().and_then(|s| s.strip_prefix("0x")).ok_or("eth_gasPrice result")?;
        // a price that does not even fit 128 bits is above any cap
        let v = u128::from_str_radix(s, 16).unwrap_or(u128::MAX);
        highest = highest.max(v);
    }
    let price = highest.max(1).saturating_mul(2);
    if price > cfg.max_gas_price_wei {
        return Err(format!("GAS PRICE ABOVE THE CAP: the endpoints name {highest} wei (x2 = {price}), the cap is {} wei: nothing is signed", cfg.max_gas_price_wei));
    }
    Ok(price)
}

/// Submits `cp` through `cfg.endpoints[via]` and waits until EVERY endpoint returns its receipt with status 1.
/// Returns the transaction hash.
pub async fn submit(cfg: &RelayConfig, via: usize, cp: &CheckpointBody, sigs: &[[u8; 65]]) -> Result<String, String> {
    if cfg.endpoints.len() < 2 {
        return Err("at least two L2 endpoints are needed".into());
    }
    let url = &cfg.endpoints[via % cfg.endpoints.len()];
    let from = key_address(&cfg.key);
    let data = encode_submit_checkpoint(cp, sigs);
    let gas_price = capped_gas_price(cfg).await?;
    let call_obj = serde_json::json!({"from": to_hex(&from), "to": to_hex(&cfg.vault), "data": to_hex(&data)});
    let est = hex_qty(&rpc(url, "eth_estimateGas", serde_json::json!([call_obj])).await.map_err(|e| format!("estimate (the vault would refuse it): {e}"))?)?;
    let nonce = hex_qty(&rpc(url, "eth_getTransactionCount", serde_json::json!([to_hex(&from), "pending"])).await?)?;
    let tx = LegacyTx { nonce, gas_price, gas: padded_gas(est), to: cfg.vault, value: 0, data, chain_id: L2_CHAIN_ID };
    let raw = tx.sign(&cfg.key);
    let hash = rpc(url, "eth_sendRawTransaction", serde_json::json!([to_hex(&raw)])).await?;
    let hash = hash.as_str().ok_or("eth_sendRawTransaction result")?.to_string();
    // the hash must be the hash of what was signed (an endpoint cannot name another transaction)
    if hash != to_hex(&keccak(&raw)) {
        return Err(format!("the endpoint answered eth_sendRawTransaction with {hash}, which is not the hash of the signed transaction"));
    }
    let deadline = tokio::time::Instant::now() + Duration::from_millis(cfg.receipt_timeout_ms);
    loop {
        let mut confirmed = 0;
        for u in &cfg.endpoints {
            if let Ok(r) = rpc(u, "eth_getTransactionReceipt", serde_json::json!([hash])).await {
                if !r.is_null() {
                    match r.get("status").and_then(|s| s.as_str()) {
                        Some("0x1") => confirmed += 1,
                        other => return Err(format!("transaction {hash} failed on {u} (status {other:?}, gas used {:?} of {})", r.get("gasUsed"), tx.gas)),
                    }
                }
            }
        }
        if confirmed == cfg.endpoints.len() {
            return Ok(hash);
        }
        if tokio::time::Instant::now() >= deadline {
            return Err(format!("transaction {hash}: a receipt from {confirmed} of {} endpoints in {} ms (every endpoint must confirm)", cfg.endpoints.len(), cfg.receipt_timeout_ms));
        }
        tokio::time::sleep(Duration::from_millis(200)).await;
    }
}

/// One relayer step: the agreed and plausible reading, the 8a rule, a submission if due. Returns the reason when
/// it submitted. `via` chooses the endpoint a transaction is sent through.
pub async fn step(app: &Arc<App>, cfg: &RelayConfig, via: usize) -> Result<Option<&'static str>, String> {
    let now = crate::node::now_ms();
    let v = read_vault_agreed(&cfg.endpoints, &cfg.vault).await?;
    // this node's own final checkpoints: what the vault can hold at all, and the newest one to deliver
    let newest = {
        let s = app.seq.lock().unwrap();
        let th = app.g.threshold;
        let final_at = |h: u64| s.aggregator.by_height.get(&h).filter(|c| c.sigs.len() >= th).map(|c| c.body.clone());
        plausible(&v, &final_at)?;
        s.latest_final().and_then(|h| s.aggregator.by_height.get(&h).and_then(|c| c.final_signatures(th).map(|sg| (c.body.clone(), sg.into_iter().map(|(_, x)| x).collect::<Vec<_>>()))))
    };
    {
        // lock order: seq before relay
        let mut s = app.seq.lock().unwrap();
        if v.checkpoint_count > 0 && s.frames.contains_key(&v.latest_height) && !s.pinned_frames.contains(&v.latest_height) {
            s.pin_frame(v.latest_height, PINNED_FRAMES);
        }
        let mut r = app.relay.lock().unwrap();
        r.vault_mode = if v.in_escape { "escape" } else if v.paused { "paused" } else { "normal" };
        r.vault_checkpoints = Some(v.checkpoint_count);
        r.vault_latest_height = Some(v.latest_height);
        r.vault_latest_height_finalized = Some(v.latest_height);
        r.l2_time_s = Some(v.l2_time_s);
        r.last_good_read_ms = Some(now);
        r.view_alarm = alarm(&v);
    }
    let reason = newest.as_ref().and_then(|(cp, _)| due(&v, cp));
    let (Some((cp, sigs)), Some(reason)) = (newest, reason) else {
        // nothing is due by the agreed reading: nothing is owed
        app.relay.lock().unwrap().due_since_ms = None;
        return Ok(None);
    };
    {
        let mut r = app.relay.lock().unwrap();
        r.due_since_ms.get_or_insert(now);
        // every endpoint confirmed this checkpoint (or a newer one) a moment ago: the finalized reading lags it
        if r.confirmed.is_some_and(|(h, at)| h >= cp.height && now.saturating_sub(at) < cfg.resubmit_after_ms) {
            return Ok(None);
        }
        r.submitted += 1;
        r.last_reason = Some(reason);
        r.last_submitted_height = Some(cp.height);
    }
    let sent = submit(cfg, via, &cp, &sigs).await;
    let mut s = app.seq.lock().unwrap();
    let mut r = app.relay.lock().unwrap();
    match sent {
        Ok(hash) => {
            s.pin_frame(cp.height, PINNED_FRAMES);
            r.accepted += 1;
            r.last_tx = Some(hash);
            r.last_error = None;
            r.gas_refused = None;
            r.confirmed = Some((cp.height, crate::node::now_ms()));
            Ok(Some(reason))
        }
        Err(e) => {
            if e.starts_with("GAS PRICE ABOVE THE CAP") {
                r.gas_refused = Some(e.clone());
            }
            Err(e)
        }
    }
}

pub async fn run(app: Arc<App>, cfg: RelayConfig, mut stop: tokio::sync::watch::Receiver<bool>) {
    {
        let mut r = app.relay.lock().unwrap();
        r.enabled = true;
        r.address = Some(key_address(&cfg.key));
        r.started_ms = crate::node::now_ms();
    }
    let mut via = 0usize;
    let mut backoff = cfg.poll_ms;
    loop {
        let wait = match step(&app, &cfg, via).await {
            Ok(_) => {
                backoff = cfg.poll_ms;
                cfg.poll_ms
            }
            Err(e) => {
                eprintln!("relayer: {e}");
                let mut r = app.relay.lock().unwrap();
                r.failures += 1;
                r.last_error = Some(e);
                // the retry rule: the next endpoint, after a backoff that doubles up to 30 s
                via = via.wrapping_add(1);
                backoff = (backoff * 2).min(30_000);
                backoff
            }
        };
        {
            // the alarm is recomputed on every turn, also when the step failed: the local alarms need no endpoint
            let mut r = app.relay.lock().unwrap();
            let a = local_alarm(&r, &cfg, crate::node::now_ms()).or_else(|| r.view_alarm.clone());
            if a != r.alarm {
                if let Some(text) = &a {
                    eprintln!("RELAYER ALARM: {text}");
                }
                r.alarm = a;
            }
        }
        tokio::select! {
            _ = tokio::time::sleep(Duration::from_millis(wait)) => {}
            _ = stop.changed() => return,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// EIP-155's own example: nonce 9, 20 gwei, 21000 gas, to 0x3535…35, 1 ether, chain id 1, key 0x4646…46.
    #[test]
    fn eip155_example_vector() {
        let tx = LegacyTx { nonce: 9, gas_price: 20_000_000_000, gas: 21_000, to: [0x35; 20], value: 1_000_000_000_000_000_000, data: vec![], chain_id: 1 };
        assert_eq!(to_hex(&tx.unsigned_rlp()), "0xec098504a817c800825208943535353535353535353535353535353535353535880de0b6b3a764000080018080");
        assert_eq!(to_hex(&tx.signing_hash()), "0xdaf5a779ae972f972197303d7b574746c7ef83eadac0f2791ad23db92e4c8e53");
        let key = SigningKey::from_slice(&[0x46; 32]).unwrap();
        assert_eq!(to_hex(&key_address(&key)), "0x9d8a62f656a8d1615c1294fd71e9cfb3e4855a4f");
        let raw = tx.sign(&key);
        // the example's signed transaction: v = 37 (0 + 35 + 2 * 1), r and s as published in the EIP (RFC 6979)
        let expect = "0xf86c098504a817c800825208943535353535353535353535353535353535353535880de0b6b3a76400008025a028ef61340bd939bc2195fe537567866003e1a15d3c71ff63e1590620aa636276a067cbe9d8997f761aecb703304b3800ccf555c9f3dc64214b297fb1966a3b6d83";
        assert_eq!(to_hex(&raw), expect);
        // and the signature recovers the sender
        let mut sig = [0u8; 65];
        sig[..32].copy_from_slice(&raw[raw.len() - 65..raw.len() - 33]);
        sig[32..64].copy_from_slice(&raw[raw.len() - 32..]);
        sig[64] = 27;
        assert_eq!(crate::sig::recover(&tx.signing_hash(), &sig).unwrap(), key_address(&key));
    }

    #[test]
    fn rlp_rules() {
        let enc = |f: &dyn Fn(&mut Vec<u8>)| {
            let mut o = Vec::new();
            f(&mut o);
            to_hex(&o)
        };
        assert_eq!(enc(&|o| rlp_uint(0, o)), "0x80");
        assert_eq!(enc(&|o| rlp_uint(0x7f, o)), "0x7f");
        assert_eq!(enc(&|o| rlp_uint(0x80, o)), "0x8180");
        assert_eq!(enc(&|o| rlp_uint(1024, o)), "0x820400");
        assert_eq!(enc(&|o| rlp_bytes(b"dog", o)), "0x83646f67");
        assert_eq!(enc(&|o| rlp_bytes(&[], o)), "0x80");
        let long = [0x61u8; 56];
        let e = enc(&|o| rlp_bytes(&long, o));
        assert!(e.starts_with("0xb838"), "a 56-byte string has a one-byte length of length: {e}");
        let mut items = Vec::new();
        rlp_bytes(b"cat", &mut items);
        rlp_bytes(b"dog", &mut items);
        assert_eq!(enc(&|o| rlp_list(&items, o)), "0xc88363617483646f67");
    }

    fn body(height: u64, wc: u64, nf: u64) -> CheckpointBody {
        CheckpointBody {
            height,
            time: height * 1000,
            log_hash: [1; 32],
            accounts_root: [2; 32],
            markets_root: [3; 32],
            withdrawals_root: [4; 32],
            next_deposit_index: 5,
            next_forced_index: nf,
            next_order_id: 7,
            total_escape: 8,
            dust: 9,
            aux_hash: [10; 32],
            deposits_head: [11; 32],
            forced_head: [12; 32],
            withdrawal_count: wc,
            withdrawals_total: 14,
        }
    }

    #[test]
    fn submit_checkpoint_calldata_layout() {
        let b = body(1, 13, 6);
        let sigs = [[0xaa; 65], [0xbb; 65]];
        let d = encode_submit_checkpoint(&b, &sigs);
        assert_eq!(&d[..4], &keccak(SUBMIT_CHECKPOINT.as_bytes())[..4]);
        let w = |i: usize| &d[4 + i * 32..4 + (i + 1) * 32];
        assert_eq!(w(0)[31], 1, "height");
        assert_eq!(w(2), &[1u8; 32], "logHash");
        assert_eq!(w(7)[31], 6, "nextForcedIndex");
        assert_eq!(w(13), &[12u8; 32], "forcedHead");
        assert_eq!(w(14)[31], 13, "withdrawalCount");
        assert_eq!(w(15)[31], 14, "withdrawalsTotal");
        assert_eq!(u64::from_be_bytes(w(16)[24..].try_into().unwrap()), 17 * 32, "offset of bytes[]");
        assert_eq!(w(17)[31], 2, "two signatures");
        let base = 4 + 18 * 32; // start of the offsets area
        let off0 = u64::from_be_bytes(w(18)[24..].try_into().unwrap()) as usize;
        let off1 = u64::from_be_bytes(w(19)[24..].try_into().unwrap()) as usize;
        assert_eq!((off0, off1), (64, 192));
        assert_eq!(d[base + off0 + 31], 65, "length of signature 0");
        assert_eq!(&d[base + off0 + 32..base + off0 + 97], &[0xaa; 65]);
        assert_eq!(&d[base + off1 + 32..base + off1 + 97], &[0xbb; 65]);
        assert_eq!(d.len(), 4 + 18 * 32 + 2 * 32 + 2 * 128, "no trailing bytes");
        assert_eq!(d.len() % 32, 4);
    }

    #[test]
    fn keeper_rule_of_vault_md_8a() {
        let mut v = VaultView { checkpoint_count: 3, latest_height: 100, latest_next_forced: 2, latest_withdrawal_count: 5, last_checkpoint_at: 1_000_000, forced_count: 2, open_forced_deadline: None, forced_delay: 172_800, escape_delay: 604_800, paused: false, in_escape: false, l2_time_s: 1_000_100 };
        // nothing newer, or nothing that asks for it
        assert_eq!(due(&v, &body(100, 9, 9)), None, "not newer than the vault's");
        assert_eq!(due(&v, &body(110, 5, 2)), None, "newer, but no user waits and the heartbeat is not due");
        // 1. a withdrawal record the vault does not have yet; 2. a processed forced request
        assert_eq!(due(&v, &body(110, 6, 2)), Some("withdrawal"));
        assert_eq!(due(&v, &body(110, 5, 3)), Some("forced"));
        // 3. heartbeat at forcedDelay / 4 = 43,200 s of L2 time, not before
        v.l2_time_s = 1_000_000 + 43_199;
        assert_eq!(due(&v, &body(110, 5, 2)), None);
        v.l2_time_s = 1_000_000 + 43_200;
        assert_eq!(due(&v, &body(110, 5, 2)), Some("heartbeat"));
        // a paused vault still takes checkpoints; a vault in escape never does
        v.paused = true;
        assert_eq!(due(&v, &body(110, 5, 2)), Some("heartbeat"));
        v.in_escape = true;
        assert_eq!(due(&v, &body(110, 6, 3)), None);
        // before the first checkpoint: no heartbeat (the silence clock has not started), but 1 and 2 fire
        let fresh = VaultView { forced_delay: 172_800, escape_delay: 604_800, l2_time_s: 9_999_999, ..Default::default() };
        assert_eq!(due(&fresh, &body(10, 0, 0)), None);
        assert_eq!(due(&fresh, &body(10, 1, 0)), Some("withdrawal"));
        assert_eq!(due(&fresh, &body(10, 0, 1)), Some("forced"));
        // alarms
        let mut a = VaultView { checkpoint_count: 3, latest_next_forced: 2, forced_count: 3, open_forced_deadline: Some(2_000_000), forced_delay: 172_800, escape_delay: 604_800, last_checkpoint_at: 1_900_000, l2_time_s: 2_000_000 - 172_800 + 86_399, ..Default::default() };
        assert_eq!(alarm(&a), None);
        a.l2_time_s += 1;
        assert!(alarm(&a).unwrap().contains("forced request 2"));
        let mut q = VaultView { checkpoint_count: 1, forced_delay: 172_800, escape_delay: 604_800, last_checkpoint_at: 1_000_000, l2_time_s: 1_000_000 + 302_399, ..Default::default() };
        assert_eq!(alarm(&q), None);
        q.l2_time_s += 1;
        assert!(alarm(&q).unwrap().contains("escapeDelay / 2"));
        assert_eq!(padded_gas(168_094), 292_141, "F434-1: 168,094 x 1.5 = 252,141, + 40,000");
        assert_eq!(padded_gas(u64::MAX), GAS_CAP);
    }
}

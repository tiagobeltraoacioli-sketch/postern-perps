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
//! Retry rule: a submission that fails (estimate reverted, send refused, no receipt in time, receipt status 0) is
//! retried at the next poll with the then-newest final checkpoint, on the next L2 endpoint of the list, after a
//! backoff that doubles from the poll interval up to 30 s. A submission counts as done when the vault's `latest`
//! height equals the checkpoint's at the L2's `finalized` tag; until then the rules above keep firing.
//!
//! Gas (F434-1: the estimate of a vault call can be the exact minimum and the next block needs more):
//! `gas = estimate * 3 / 2 + 40,000`, at most 15,000,000; gas price = twice the endpoint's `eth_gasPrice`.
//! Transactions are legacy (EIP-155), chain id 700770.
//!
//! Alarms (stderr and `/v1/node`): `forcedDelay / 2` of L2 time since an open forced request was posted without the
//! vault knowing it processed; `escapeDelay / 2` since the vault's last checkpoint.
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
        }
    }
}

impl RelayState {
    pub fn to_json(&self) -> serde_json::Value {
        serde_json::json!({"enabled": self.enabled, "address": self.address.map(|a| to_hex(&a)), "vaultMode": self.vault_mode,
            "submitted": self.submitted, "accepted": self.accepted, "failures": self.failures, "lastReason": self.last_reason,
            "lastSubmittedHeight": self.last_submitted_height, "lastTx": self.last_tx, "vaultCheckpoints": self.vault_checkpoints,
            "vaultLatestHeight": self.vault_latest_height, "vaultLatestHeightFinalized": self.vault_latest_height_finalized,
            "l2TimeS": self.l2_time_s, "alarm": self.alarm, "lastError": self.last_error})
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
    /// L2 JSON-RPC endpoints (plain http, loopback or a private network), used in turn after a failure
    pub endpoints: Vec<String>,
    pub key: SigningKey,
    pub vault: Address,
    pub poll_ms: u64,
    pub receipt_timeout_ms: u64,
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

/// Submits `cp` through `url` and waits for its receipt. Returns the transaction hash.
pub async fn submit(cfg: &RelayConfig, url: &str, cp: &CheckpointBody, sigs: &[[u8; 65]]) -> Result<String, String> {
    let from = key_address(&cfg.key);
    let data = encode_submit_checkpoint(cp, sigs);
    let call_obj = serde_json::json!({"from": to_hex(&from), "to": to_hex(&cfg.vault), "data": to_hex(&data)});
    let est = hex_qty(&rpc(url, "eth_estimateGas", serde_json::json!([call_obj])).await.map_err(|e| format!("estimate (the vault would refuse it): {e}"))?)?;
    let gas_price = rpc(url, "eth_gasPrice", serde_json::json!([])).await.and_then(|v| hex_qty(&v))?.max(1) as u128 * 2;
    let nonce = hex_qty(&rpc(url, "eth_getTransactionCount", serde_json::json!([to_hex(&from), "pending"])).await?)?;
    let tx = LegacyTx { nonce, gas_price, gas: padded_gas(est), to: cfg.vault, value: 0, data, chain_id: L2_CHAIN_ID };
    let raw = tx.sign(&cfg.key);
    let hash = rpc(url, "eth_sendRawTransaction", serde_json::json!([to_hex(&raw)])).await?;
    let hash = hash.as_str().ok_or("eth_sendRawTransaction result")?.to_string();
    let deadline = tokio::time::Instant::now() + Duration::from_millis(cfg.receipt_timeout_ms);
    loop {
        if let Ok(r) = rpc(url, "eth_getTransactionReceipt", serde_json::json!([hash])).await {
            if !r.is_null() {
                return match r.get("status").and_then(|s| s.as_str()) {
                    Some("0x1") => Ok(hash),
                    other => Err(format!("transaction {hash} failed (status {other:?}, gas used {:?} of {})", r.get("gasUsed"), tx.gas)),
                };
            }
        }
        if tokio::time::Instant::now() >= deadline {
            return Err(format!("no receipt for {hash} in {} ms", cfg.receipt_timeout_ms));
        }
        tokio::time::sleep(Duration::from_millis(200)).await;
    }
}

/// One relayer step: read the vault, decide by the 8a rule, submit if due. Returns the reason when it submitted.
pub async fn step(app: &Arc<App>, cfg: &RelayConfig, endpoint: usize) -> Result<Option<&'static str>, String> {
    let url = &cfg.endpoints[endpoint % cfg.endpoints.len()];
    let v = read_vault(url, &cfg.vault, "latest").await?;
    let fin = read_vault(url, &cfg.vault, "finalized").await.ok();
    // this node's newest final checkpoint and its exactly-threshold signatures
    let newest = {
        let s = app.seq.lock().unwrap();
        s.latest_final().and_then(|h| s.aggregator.by_height.get(&h).and_then(|c| c.final_signatures(app.g.threshold).map(|sg| (c.body.clone(), sg.into_iter().map(|(_, x)| x).collect::<Vec<_>>()))))
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
        r.vault_latest_height_finalized = fin.as_ref().map(|f| f.latest_height);
        r.l2_time_s = Some(v.l2_time_s);
        let a = alarm(&v);
        if a != r.alarm {
            if let Some(text) = &a {
                eprintln!("RELAYER ALARM: {text}");
            }
            r.alarm = a;
        }
    }
    let Some((cp, sigs)) = newest else { return Ok(None) };
    let Some(reason) = due(&v, &cp) else { return Ok(None) };
    {
        let mut r = app.relay.lock().unwrap();
        r.submitted += 1;
        r.last_reason = Some(reason);
        r.last_submitted_height = Some(cp.height);
    }
    let hash = submit(cfg, url, &cp, &sigs).await?;
    {
        let mut s = app.seq.lock().unwrap();
        s.pin_frame(cp.height, PINNED_FRAMES);
        let mut r = app.relay.lock().unwrap();
        r.accepted += 1;
        r.last_tx = Some(hash);
        r.last_error = None;
    }
    Ok(Some(reason))
}

pub async fn run(app: Arc<App>, cfg: RelayConfig, mut stop: tokio::sync::watch::Receiver<bool>) {
    {
        let mut r = app.relay.lock().unwrap();
        r.enabled = true;
        r.address = Some(key_address(&cfg.key));
    }
    let mut endpoint = 0usize;
    let mut backoff = cfg.poll_ms;
    loop {
        let wait = match step(&app, &cfg, endpoint).await {
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
                endpoint = endpoint.wrapping_add(1);
                backoff = (backoff * 2).min(30_000);
                backoff
            }
        };
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

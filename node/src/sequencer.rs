//! The sequencer: admission, ordering, block production, the durable log, snapshots, recovery, and the
//! collection of validator checkpoint signatures.
//!
//! Trust model (v1, BRIEF-PERPS item 2): ONE sequencer orders. It cannot forge an action (each carries the user's
//! EIP-712 signature and is re-verified by every validator's engine), cannot invent a deposit (validators compare
//! every deposit input with their own two-endpoint finalized L2 view), cannot move funds (withdrawals need k-of-n
//! validator signatures over a root the validators computed), and cannot hold a signed action for long (nonce
//! window: an action is valid only within 120 s before / 30 s after its nonce-as-milliseconds). It CAN censor
//! and reorder within that window, and choose the block time within the validators' clock bound: bounded by the
//! L2 forced-action path (validators do not sign past a missed forced deadline; the vault then enters escape mode).
//!
//! Order inside a block: one oracle update (newest pending round), then deposits in vault index order, then
//! forced requests in vault index order, then actions in arrival order. Empty blocks are not produced.
//!
//! UNAUDITED PROTOTYPE.

use crate::actions::{parse_request, verify_signed_action, ActionError};
use crate::checkpoint::{verify_checkpoint_json, Aggregator, CheckpointBody};
use crate::engine::{AuthorityError, BlockOutcome, Engine};
use crate::genesis::Genesis;
use crate::log::{block_digest, list_snapshots, read_snapshot, write_snapshot, InputLog, SnapshotMeta};
use crate::sig::{key_address, sign};
use crate::types::{decode_inputs, encode_inputs, log_hash, Block, BlockHeader, Input, OracleUpdate, SignedAction};
use crate::util::{to_hex, Address, H256};
use crate::validator::{checkpoint_body, is_checkpoint_height};
use crate::watcher::SharedView;
use k256::ecdsa::SigningKey;
use std::collections::{BTreeMap, HashMap, VecDeque};
use std::io::Write;
use std::path::PathBuf;
use std::sync::Arc;

pub const MAX_ACTIONS_PER_BLOCK: usize = 5_000;
pub const MAX_MEMPOOL: usize = 100_000;
pub const ACK_RETENTION: usize = 500_000;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AdmissionError {
    pub http: u16,
    pub code: &'static str,
    pub message: String,
}

fn adm(http: u16, code: &'static str, message: impl Into<String>) -> AdmissionError {
    AdmissionError { http, code, message: message.into() }
}

#[derive(Clone, Debug)]
pub struct Ack {
    pub action_hash: H256,
    pub received_at: u64,
    pub status: &'static str,
    pub height: Option<u64>,
    pub index: Option<usize>,
    pub result: Option<serde_json::Value>,
    pub signer: Address,
    pub account: Address,
}

impl Ack {
    pub fn to_json(&self) -> serde_json::Value {
        let mut j = serde_json::json!({"actionHash": to_hex(&self.action_hash), "status": self.status, "receivedAt": self.received_at});
        if let Some(h) = self.height {
            j["height"] = h.into();
        }
        if let Some(i) = self.index {
            j["index"] = i.into();
        }
        if let Some(r) = &self.result {
            j["result"] = r.clone();
        }
        j
    }
}

struct Bucket {
    tokens: f64,
    last_ms: u64,
}

pub struct RateLimit {
    pub per_sec: f64,
    pub burst: f64,
    buckets: HashMap<Address, Bucket>,
}

impl RateLimit {
    pub fn new(per_sec: f64, burst: f64) -> Self {
        RateLimit { per_sec, burst, buckets: HashMap::new() }
    }
    /// Wall-clock admission control only (never part of consensus: floats are fine here).
    pub fn allow(&mut self, k: &Address, now_ms: u64) -> bool {
        if self.per_sec <= 0.0 {
            return true; // disabled (dev load tests only)
        }
        let b = self.buckets.entry(*k).or_insert(Bucket { tokens: self.burst, last_ms: now_ms });
        let dt = now_ms.saturating_sub(b.last_ms) as f64 / 1000.0;
        b.tokens = (b.tokens + dt * self.per_sec).min(self.burst);
        b.last_ms = now_ms;
        if b.tokens >= 1.0 {
            b.tokens -= 1.0;
            true
        } else {
            false
        }
    }
}

pub struct BlockInfo {
    pub block: Arc<Block>,
    pub outcome: BlockOutcome,
    /// API `BookDelta` objects of this block, one per market whose book changed
    pub book_deltas: Vec<serde_json::Value>,
}

/// Per market: (seq, bids, asks) of the last published book, engine units.
pub type BookState = (u64, Vec<(u64, u64)>, Vec<(u64, u64)>);

/// Engine units (1e-8) to a decimal string at the market's display decimals (exact: prices are on the tick).
pub fn disp(v: u64, decimals: u32) -> String {
    let d = decimals.min(8);
    crate::util::scaled_to_decimal((v / 10u64.pow(8 - d)) as u128, d)
}

pub fn levels_json(levels: &[(u64, u64)], pd: u32, sd: u32) -> Vec<serde_json::Value> {
    levels.iter().map(|(p, q)| serde_json::json!([disp(*p, pd), disp(*q, sd)])).collect()
}

pub struct Sequencer {
    pub g: Arc<Genesis>,
    key: SigningKey,
    pub engine: Box<dyn Engine>,
    pub log: InputLog,
    pub data_dir: PathBuf,
    mempool: VecDeque<(SignedAction, H256)>,
    pending_nonce: HashMap<Address, std::collections::BTreeSet<u64>>,
    pending_oracle: BTreeMap<u64, OracleUpdate>,
    pub acks: HashMap<H256, Ack>,
    ack_order: VecDeque<H256>,
    pub aggregator: Aggregator,
    pub rate: RateLimit,
    pub view: SharedView,
    pub snapshot_every: u64,
    pub recent: VecDeque<Arc<BlockInfo>>,
    pub recent_keep: usize,
    /// withdrawal id -> height it was created
    pub withdrawal_heights: BTreeMap<u64, u64>,
    /// deposit index -> height credited
    pub deposit_heights: BTreeMap<u64, u64>,
    pub forced_heights: BTreeMap<u64, u64>,
    pub recovered: RecoveryReport,
    checkpoints_file: PathBuf,
    pub halted: Option<String>,
    pub books: BTreeMap<u32, BookState>,
}

#[derive(Clone, Debug, Default)]
pub struct RecoveryReport {
    pub snapshot_height: Option<u64>,
    pub replayed_blocks: u64,
    pub torn_tail_bytes: u64,
    pub checkpoints_loaded: u64,
}

impl Sequencer {
    /// Opens (or creates) the node's data directory and recovers: newest usable snapshot, verified against the
    /// log, then replay of the rest of the log, checking every block's recorded state root. Any mismatch refuses
    /// to start (fail closed).
    pub fn open(g: Arc<Genesis>, key: SigningKey, mut engine: Box<dyn Engine>, data_dir: PathBuf, view: SharedView) -> Result<Sequencer, String> {
        if key_address(&key) != g.sequencer {
            return Err("sequencer key does not match the genesis sequencer address".into());
        }
        if engine.genesis_log_hash() != g.genesis_log_hash() {
            return Err("engine genesis logHash(0) differs from the node genesis: refusing to start".into());
        }
        let (log, rep) = InputLog::open(&data_dir, &g)?;
        let mut recovered = RecoveryReport { torn_tail_bytes: rep.torn_tail_bytes, ..Default::default() };
        // choose the newest snapshot consistent with the log
        let mut snaps = list_snapshots(&data_dir);
        snaps.sort();
        // a snapshot above the log means durable blocks were lost from the log (a damaged record taken for a
        // torn tail, or a truncated file): refuse to start rather than re-produce those heights
        if let Some((h, _)) = snaps.last() {
            if *h > log.last_height {
                return Err(format!("snapshot at height {h} is above the log's last block {}: refusing to start", log.last_height));
            }
        }
        let mut start = 0u64;
        for (h, p) in snaps.iter().rev() {
            if *h > log.last_height || *h == 0 {
                continue;
            }
            let Ok((meta, bytes)) = read_snapshot(p, engine.name()) else { continue };
            let Some(b) = log.read(*h)? else { continue };
            if meta.log_hash != b.header.log_hash || meta.state_root != b.header.state_root || meta.height != *h {
                continue;
            }
            let mut e2 = engine.clone_box();
            if e2.restore(&bytes).is_err() || e2.state_root() != meta.state_root || e2.height() != *h {
                continue;
            }
            engine = e2;
            start = *h;
            recovered.snapshot_height = Some(*h);
            break;
        }
        let mut s = Sequencer {
            g: g.clone(),
            key,
            engine,
            log,
            checkpoints_file: data_dir.join("checkpoints.jsonl"),
            data_dir,
            mempool: VecDeque::new(),
            pending_nonce: HashMap::new(),
            pending_oracle: BTreeMap::new(),
            acks: HashMap::new(),
            ack_order: VecDeque::new(),
            aggregator: Aggregator::default(),
            rate: RateLimit::new(20.0, 40.0),
            view,
            snapshot_every: 1000,
            recent: VecDeque::new(),
            recent_keep: 2000,
            withdrawal_heights: BTreeMap::new(),
            deposit_heights: BTreeMap::new(),
            forced_heights: BTreeMap::new(),
            recovered,
            halted: None,
            books: BTreeMap::new(),
        };
        for h in (start + 1)..=s.log.last_height {
            let b = s.log.read(h)?.ok_or("log read")?;
            let inputs = decode_inputs(&g.types, &b.inputs_encoded)?;
            let out = s.engine.apply_block(h, b.header.time, &inputs).map_err(|e| format!("replay of block {h} refused: {e}"))?;
            if s.engine.state_root() != b.header.state_root {
                return Err(format!("replay of block {h}: state root differs from the logged one: refusing to start"));
            }
            s.after_apply(&b, &inputs, &out);
            s.recovered.replayed_blocks += 1;
        }
        // reload final checkpoints, each re-verified and matched to the log
        if let Ok(txt) = std::fs::read_to_string(&s.checkpoints_file) {
            for line in txt.lines().filter(|l| !l.trim().is_empty()) {
                let j: serde_json::Value = match serde_json::from_str(line) {
                    Ok(j) => j,
                    Err(_) => continue, // a torn last line
                };
                let body = verify_checkpoint_json(&g, &j).map_err(|e| format!("checkpoints.jsonl: {e}"))?;
                let b = s.log.read(body.height)?.ok_or("checkpoint above the log")?;
                if b.header.log_hash != body.log_hash || b.header.state_root != body.state_root() {
                    return Err("checkpoints.jsonl does not match the log: refusing to start".into());
                }
                s.aggregator.by_height.remove(&body.height);
                s.aggregator.expect(&g, body.clone());
                for sj in j["signatures"].as_array().unwrap() {
                    let sig = crate::util::parse_hex_fixed::<65>(sj["signature"].as_str().unwrap())?;
                    s.aggregator.add(&g, body.height, sig)?;
                }
                s.recovered.checkpoints_loaded += 1;
            }
        }
        Ok(s)
    }

    fn after_apply(&mut self, b: &Block, inputs: &[Input], out: &BlockOutcome) {
        let h = b.header.height;
        for w in &out.withdrawals {
            self.withdrawal_heights.insert(*w, h);
        }
        for inp in inputs {
            match inp {
                Input::Deposit(d) => {
                    self.deposit_heights.insert(d.index, h);
                }
                Input::Forced(f) => {
                    self.forced_heights.insert(f.index, h);
                }
                _ => {}
            }
        }
        if is_checkpoint_height(&self.g, h, inputs, !out.withdrawals.is_empty()) {
            let v = self.view.lock().unwrap();
            match checkpoint_body(&self.g, self.engine.as_ref(), b, &v) {
                Ok(body) => {
                    drop(v);
                    self.aggregator.expect(&self.g, body);
                }
                Err(e) => eprintln!("checkpoint body at {h} not built: {e}"),
            }
        }
        for (i, (inp, o)) in inputs.iter().zip(&out.outcomes).enumerate() {
            if let (Input::Action(_), Some(ah)) = (inp, o.action_hash) {
                let mut result = serde_json::json!({"ok": o.ok, "error": o.error});
                for k in ["orderId", "orderStatus", "filledSize", "withdrawalId"] {
                    if let Some(v) = o.result.get(k) {
                        result[k] = v.clone();
                    }
                }
                if let Some(a) = self.acks.get_mut(&ah) {
                    a.status = "included";
                    a.height = Some(h);
                    a.index = Some(i);
                    a.result = Some(result);
                }
            }
        }
        let mut book_deltas = Vec::new();
        for m in self.g.markets.clone() {
            let Some((bids, asks)) = self.engine.book_levels(m.market_id) else { continue };
            let prev = self.books.get(&m.market_id).cloned().unwrap_or((0, Vec::new(), Vec::new()));
            if prev.1 == bids && prev.2 == asks {
                continue;
            }
            let diff = |old: &[(u64, u64)], new: &[(u64, u64)]| -> Vec<(u64, u64)> {
                let o: BTreeMap<u64, u64> = old.iter().copied().collect();
                let n: BTreeMap<u64, u64> = new.iter().copied().collect();
                let mut out: Vec<(u64, u64)> = n.iter().filter(|(p, q)| o.get(p) != Some(q)).map(|(p, q)| (*p, *q)).collect();
                out.extend(o.keys().filter(|p| !n.contains_key(p)).map(|p| (*p, 0)));
                out
            };
            let (db, da) = (diff(&prev.1, &bids), diff(&prev.2, &asks));
            let seq = prev.0 + 1;
            book_deltas.push(serde_json::json!({"symbol": m.symbol, "seq": seq, "prevSeq": prev.0, "height": h, "time": b.header.time,
                "bids": levels_json(&db, m.price_decimals, m.size_decimals), "asks": levels_json(&da, m.price_decimals, m.size_decimals)}));
            self.books.insert(m.market_id, (seq, bids, asks));
        }
        let info = Arc::new(BlockInfo { block: Arc::new(b.clone()), outcome: out.clone(), book_deltas });
        self.recent.push_back(info);
        while self.recent.len() > self.recent_keep {
            self.recent.pop_front();
        }
    }

    /// Admission (a pre-filter: the engine re-checks everything deterministically). Geoblocking is decided by the
    /// API layer before this call.
    pub fn admit(&mut self, req: &serde_json::Value, now_ms: u64) -> Result<Ack, AdmissionError> {
        if let Some(h) = &self.halted {
            return Err(adm(503, "SEQUENCER_UNAVAILABLE", format!("halted: {h}")));
        }
        let a = parse_request(&self.g, req).map_err(|e| adm(400, "MALFORMED", e))?;
        self.admit_signed(a, now_ms)
    }

    pub fn admit_signed(&mut self, a: SignedAction, now_ms: u64) -> Result<Ack, AdmissionError> {
        let v = verify_signed_action(&self.g, &a).map_err(|e| match e {
            ActionError::BadSignature(s) => adm(400, "BAD_SIGNATURE", s.to_string()),
            ActionError::Malformed(s) => adm(400, "MALFORMED", s),
        })?;
        if self.acks.contains_key(&v.action_hash) {
            return Err(adm(400, "NONCE_USED", "action already received"));
        }
        let t = self.engine.time().max(now_ms);
        self.engine.authority(&v.account, &v.signer, &v.primary, t).map_err(|e| match e {
            AuthorityError::UnknownSigner => adm(403, "UNKNOWN_SIGNER", "signer is neither the account nor an approved, unexpired agent"),
            AuthorityError::AgentNotAllowed => adm(403, "AGENT_NOT_ALLOWED", format!("{} requires the main key", v.primary)),
        })?;
        // SPEC S-2.5 against the applied state, plus nonces admitted but not yet applied (single use)
        if self.pending_nonce.get(&v.signer).is_some_and(|p| p.contains(&v.nonce)) {
            return Err(adm(400, "NONCE_USED", "nonce already pending for this signer"));
        }
        self.engine.nonce_ok(&v.signer, v.nonce, now_ms.max(self.engine.time())).map_err(|c| {
            adm(400, c, "nonce = milliseconds clock in now-120 s..now+30 s, single use, above the 64th highest used (SPEC 2.5)")
        })?;
        if !self.rate.allow(&v.account, now_ms) {
            return Err(adm(429, "RATE_LIMITED", "per-account action rate"));
        }
        if self.mempool.len() >= MAX_MEMPOOL {
            return Err(adm(503, "SEQUENCER_UNAVAILABLE", "mempool full"));
        }
        self.pending_nonce.entry(v.signer).or_default().insert(v.nonce);
        self.mempool.push_back((a, v.action_hash));
        let ack = Ack { action_hash: v.action_hash, received_at: now_ms, status: "accepted", height: None, index: None, result: None, signer: v.signer, account: v.account };
        self.acks.insert(v.action_hash, ack.clone());
        self.ack_order.push_back(v.action_hash);
        while self.ack_order.len() > ACK_RETENTION {
            if let Some(old) = self.ack_order.pop_front() {
                self.acks.remove(&old);
            }
        }
        Ok(ack)
    }

    /// Queues an oracle update (signatures are checked by the engine; the sequencer only keeps the newest rounds).
    pub fn submit_oracle(&mut self, u: OracleUpdate) -> Result<(), String> {
        if u.round <= self.engine.last_oracle_round() {
            return Err("ORACLE_ROUND_NOT_NEW".into());
        }
        self.pending_oracle.insert(u.round, u);
        while self.pending_oracle.len() > 16 {
            let k = *self.pending_oracle.keys().next().unwrap();
            self.pending_oracle.remove(&k);
        }
        Ok(())
    }

    /// Builds, applies, logs (fsync) and signs the next block; `None` when there is nothing to include.
    pub fn produce(&mut self, now_ms: u64) -> Result<Option<Arc<BlockInfo>>, String> {
        if let Some(h) = &self.halted {
            return Err(format!("halted: {h}"));
        }
        let mut inputs = Vec::new();
        let last_round = self.engine.last_oracle_round();
        self.pending_oracle.retain(|r, _| *r > last_round);
        if let Some((_, u)) = self.pending_oracle.iter().next_back() {
            inputs.push(Input::Oracle(u.clone()));
        }
        {
            let v = self.view.lock().unwrap();
            let mut nd = self.engine.next_deposit_index();
            while let Some(d) = v.deposits.get(&nd) {
                inputs.push(Input::Deposit(d.clone()));
                nd += 1;
            }
            let mut nf = self.engine.next_forced_index();
            while let Some(f) = v.forced.get(&nf) {
                inputs.push(Input::Forced(f.clone()));
                nf += 1;
            }
        }
        let n_actions = self.mempool.len().min(MAX_ACTIONS_PER_BLOCK);
        let actions: Vec<(SignedAction, H256)> = self.mempool.drain(..n_actions).collect();
        for (a, _) in &actions {
            inputs.push(Input::Action(a.clone()));
        }
        if inputs.is_empty() {
            return Ok(None);
        }
        let height = self.log.last_height + 1;
        let time = now_ms.max(self.log.last_time + 1);
        let enc = encode_inputs(&self.g.types, &inputs)?;
        // apply on a copy first: the live engine moves only once the block is durable
        let mut next = self.engine.clone_box();
        let out = match next.apply_block(height, time, &inputs) {
            Ok(o) => o,
            Err(e) => {
                // a structurally invalid block built by this sequencer is a bug: stop producing (fail closed)
                self.halted = Some(format!("own block {height} refused by the engine: {e}"));
                return Err(self.halted.clone().unwrap());
            }
        };
        let lh = log_hash(&self.log.last_log_hash, height, time, &enc);
        let mut b = Block {
            header: BlockHeader { height, time, prev_log_hash: self.log.last_log_hash, log_hash: lh, state_root: next.state_root() },
            inputs_encoded: enc,
            sequencer_signature: [0u8; 65],
        };
        b.sequencer_signature = sign(&self.key, &block_digest(&self.g, &b));
        if let Err(e) = self.log.append(&self.g, &b) {
            self.halted = Some(format!("log append failed: {e}"));
            return Err(self.halted.clone().unwrap());
        }
        self.engine = next;
        // pending nonces that are now in the engine can be forgotten
        for (a, _) in &actions {
            if let Ok(v) = verify_signed_action(&self.g, a) {
                if let Some(set) = self.pending_nonce.get_mut(&v.signer) {
                    set.remove(&v.nonce);
                    if set.is_empty() {
                        self.pending_nonce.remove(&v.signer);
                    }
                }
            }
        }
        self.after_apply(&b, &inputs, &out);
        if self.snapshot_every > 0 && height % self.snapshot_every == 0 && !self.engine.snapshot().is_empty() {
            let meta = SnapshotMeta { height, time, log_hash: lh, state_root: b.header.state_root };
            if let Err(e) = write_snapshot(&self.data_dir, &meta, self.engine.name(), &self.engine.snapshot(), 2) {
                eprintln!("snapshot at {height} failed (log is intact, recovery replays further): {e}");
            }
        }
        Ok(self.recent.back().cloned())
    }

    /// A validator's checkpoint signature (from in-process validators or `POST /v1/finality/signatures`).
    pub fn add_signature(&mut self, height: u64, sig: [u8; 65]) -> Result<bool, String> {
        let r = self.aggregator.add(&self.g, height, sig)?;
        if let crate::checkpoint::AddResult::Added { now_final: true } = r {
            let j = self.aggregator.checkpoint_json(&self.g, height, None).ok_or("checkpoint vanished")?;
            let line = serde_json::to_string(&j).map_err(|e| e.to_string())?;
            let mut f = std::fs::OpenOptions::new().create(true).append(true).open(&self.checkpoints_file).map_err(|e| e.to_string())?;
            writeln!(f, "{line}").map_err(|e| e.to_string())?;
            f.sync_data().map_err(|e| e.to_string())?;
            return Ok(true);
        }
        Ok(false)
    }

    pub fn latest_final(&self) -> Option<u64> {
        self.aggregator.latest_final
    }

    pub fn block_info(&self, height: u64) -> Option<Arc<BlockInfo>> {
        let first = self.recent.front()?.block.header.height;
        if height < first {
            return None;
        }
        self.recent.get((height - first) as usize).cloned()
    }

    pub fn block(&self, height: u64) -> Result<Option<Block>, String> {
        if let Some(i) = self.block_info(height) {
            return Ok(Some((*i.block).clone()));
        }
        self.log.read(height)
    }

    /// The withdrawal certificate (VAULT.md 0.2 `withdraw`): the record, its depth-32 SPEC 11 proof against the newest final
    /// checkpoint that contains it, and that checkpoint with exactly `threshold` ascending signatures.
    pub fn certificate(&self, id: u64) -> Option<serde_json::Value> {
        let rec = self.engine.withdrawal(id)?;
        let cp_h = self.aggregator.latest_final?;
        let c = self.aggregator.by_height.get(&cp_h)?;
        if id >= c.body.withdrawal_count {
            return None;
        }
        let siblings = self.engine.withdrawal_proof(id, c.body.withdrawal_count)?;
        let cp = self.aggregator.checkpoint_json(&self.g, cp_h, None)?;
        Some(serde_json::json!({
            "digest": to_hex(&c.digest),
            "record": {"withdrawalId": id.to_string(), "account": to_hex(&rec.account), "recipient": to_hex(&rec.recipient), "amount": rec.amount.to_string(), "height": rec.height.to_string()},
            "proof": {"index": id.to_string(), "siblings": siblings.iter().map(|p| to_hex(p)).collect::<Vec<_>>()},
            "leaf": to_hex(&rec.leaf()),
            "checkpoint": cp,
            "signatures": cp["signatures"].clone(),
        }))
    }

    pub fn checkpoint_for(&self, height: u64) -> Option<CheckpointBody> {
        self.aggregator.by_height.get(&height).map(|c| c.body.clone())
    }

    /// API `Book` snapshot (depth levels per side) with the seq the deltas continue from.
    pub fn book_json(&self, symbol: &str, depth: usize) -> Option<serde_json::Value> {
        let m = self.g.market_by_symbol(symbol)?;
        let (seq, bids, asks) = self.books.get(&m.market_id).cloned().unwrap_or((0, Vec::new(), Vec::new()));
        Some(serde_json::json!({"symbol": symbol, "seq": seq, "height": self.engine.height(), "time": self.engine.time(),
            "bids": levels_json(&bids[..bids.len().min(depth)], m.price_decimals, m.size_decimals),
            "asks": levels_json(&asks[..asks.len().min(depth)], m.price_decimals, m.size_decimals)}))
    }

    pub fn mempool_len(&self) -> usize {
        self.mempool.len()
    }
}

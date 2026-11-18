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
use crate::index::{IndexDelta, Indexer, PreInfo};
use crate::oracle_pool::OraclePool;
use crate::log::{block_digest, list_snapshots, read_snapshot, write_snapshot, InputLog, SnapshotMeta};
use crate::sig::{key_address, sign};
use crate::types::{decode_inputs, encode_inputs, log_hash, Block, BlockHeader, Input, OracleUpdate, SignedAction};
use crate::util::{to_hex, Address, H256};
use crate::validator::{checkpoint_body, is_checkpoint_height};
use crate::watcher::SharedView;
use k256::ecdsa::SigningKey;
use std::collections::{BTreeMap, BTreeSet, HashMap, VecDeque};
use std::io::Write;
use std::path::PathBuf;
use std::sync::Arc;

pub const MAX_ACTIONS_PER_BLOCK: usize = 5_000;
/// R436-3: a block takes at most this many pending L2 inputs (each a consecutive prefix in vault index order); the
/// rest follow in the next blocks. 1 + 2,000 + 1,000 + 5,000 stays below the engine's MAX_INPUTS (10,000), and
/// `produce` also keeps the block inside the engine's own input and byte bounds (`Engine::block_limits`).
pub const MAX_DEPOSITS_PER_BLOCK: usize = 2_000;
pub const MAX_FORCED_PER_BLOCK: usize = 1_000;
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
    /// What the block changed in the market-data index (`None` with the stub engine)
    pub delta: Option<IndexDelta>,
}

/// Checkpoint heights whose engine state is kept for account proofs: at most this many FINAL ones (the newest),
/// every pinned one (checkpoints the relayer submitted to the vault), and at most `FRAMES_UNFINAL_KEEP` not yet
/// final ones above the latest final.
pub const FRAMES_FINAL_KEEP: usize = 8;
pub const FRAMES_UNFINAL_KEEP: usize = 32;

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
    /// The market-data index (real engine only): orders, fills, trades, candles, funding. Not consensus.
    pub index: Option<Indexer>,
    /// Oracle observations gathered per round (`crate::oracle_pool`)
    pub oracle_pool: OraclePool,
    /// checkpoint height -> the real engine's state after that block (account proofs, `?finalized=true` reads)
    pub frames: BTreeMap<u64, Arc<postern_perps_engine::Engine>>,
    /// checkpoint heights never pruned from `frames` (submitted to the vault)
    pub pinned_frames: BTreeSet<u64>,
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
            index: None,
            oracle_pool: OraclePool::default(),
            frames: BTreeMap::new(),
            pinned_frames: BTreeSet::new(),
        };
        s.pinned_frames = read_pinned(&s.data_dir);
        if let Some(e) = s.engine.real() {
            let mut ix = Indexer::new(&g);
            if start > 0 {
                // recovered from a snapshot: the index covers the blocks after it (history before it is not served)
                ix.from_height = start + 1;
                ix.resync_shadow(e);
            }
            s.index = Some(ix);
        }
        for h in (start + 1)..=s.log.last_height {
            let b = s.log.read(h)?.ok_or("log read")?;
            let inputs = decode_inputs(&g.types, &b.inputs_encoded)?;
            let pre = s.engine.real().map(PreInfo::of);
            let out = s.engine.apply_block(h, b.header.time, &inputs).map_err(|e| format!("replay of block {h} refused: {e}"))?;
            if s.engine.state_root() != b.header.state_root {
                return Err(format!("replay of block {h}: state root differs from the logged one: refusing to start"));
            }
            s.after_apply(&b, &inputs, &out, pre.as_ref());
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
        s.prune_frames();
        Ok(s)
    }

    fn after_apply(&mut self, b: &Block, inputs: &[Input], out: &BlockOutcome, pre: Option<&PreInfo>) {
        let h = b.header.height;
        let delta = match (self.index.as_mut(), pre, self.engine.real()) {
            (Some(ix), Some(pre), Some(post)) => Some(ix.apply(&self.g, pre, post, h, b.header.time, inputs, &out.outcomes, &out.typed)),
            _ => None,
        };
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
                    if let Some(e) = self.engine.real() {
                        self.frames.insert(h, Arc::new(e.clone()));
                        self.prune_frames();
                    }
                }
                Err(e) => eprintln!("checkpoint body at {h} not built: {e}"),
            }
        }
        for (i, (inp, o)) in inputs.iter().zip(&out.outcomes).enumerate() {
            if let (Input::Action(_), Some(ah)) = (inp, o.action_hash) {
                let mut result = serde_json::json!({"ok": o.ok, "error": o.error});
                for k in ["orderId", "orderStatus", "filledSize", "withdrawalId"] {
                    if let Some(v) = o.result.get(k).or_else(|| delta.as_ref().and_then(|d| d.results.get(&i)).and_then(|r| r.get(k))) {
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
        let info = Arc::new(BlockInfo { block: Arc::new(b.clone()), outcome: out.clone(), book_deltas, delta });
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
        // R436-4: the ENGINE's own decoder must accept this action on its own. An action it refuses would make it
        // refuse the whole block that carries it; it never enters the mempool.
        self.engine.precheck_input(&Input::Action(a.clone())).map_err(|e| adm(400, "MALFORMED", format!("the engine's decoder refuses this action: {e}")))?;
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

    /// DEV / TEST ONLY (behind the dev-oracle feature, which a release build refuses to compile): puts an action
    /// into the mempool WITHOUT admission, to test what `produce` does with an input that should never get there.
    #[cfg(feature = "dev-oracle")]
    pub fn dev_inject_unchecked(&mut self, a: SignedAction, now_ms: u64) -> H256 {
        let v = verify_signed_action(&self.g, &a).expect("a signed action");
        self.mempool.push_back((a, v.action_hash));
        self.acks.insert(v.action_hash, Ack { action_hash: v.action_hash, received_at: now_ms, status: "accepted", height: None, index: None, result: None, signer: v.signer, account: v.account });
        self.ack_order.push_back(v.action_hash);
        v.action_hash
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

    /// Marks an action that left the mempool without entering a block.
    fn reject_ack(&mut self, hash: &H256, a: &SignedAction, error: String) {
        if let Some(k) = self.acks.get_mut(hash) {
            k.status = "rejected";
            k.result = Some(serde_json::json!({"ok": false, "error": error}));
        }
        if let Ok(v) = verify_signed_action(&self.g, a) {
            if let Some(set) = self.pending_nonce.get_mut(&v.signer) {
                set.remove(&v.nonce);
                if set.is_empty() {
                    self.pending_nonce.remove(&v.signer);
                }
            }
        }
    }

    /// Builds, applies, logs (fsync) and signs the next block; `None` when there is nothing to include.
    ///
    /// Bounds (R436-3): at most one oracle update, `MAX_DEPOSITS_PER_BLOCK` deposits, `MAX_FORCED_PER_BLOCK` forced
    /// requests and `MAX_ACTIONS_PER_BLOCK` actions, and never more inputs or encoded bytes than the engine accepts
    /// in a block; what does not fit waits for the next block (L2 inputs as a consecutive prefix, actions in
    /// arrival order).
    ///
    /// A refusal of the built block by the engine (R436-4): each input is asked of the engine's decoder on its
    /// own; actions (and an oracle update) it refuses are dropped and acknowledged as rejected, and the block is
    /// rebuilt once from the rest. Only if nothing explains the refusal, or an L2 input (whose order is the
    /// vault's and cannot be skipped) is the cause, the sequencer halts (fail closed).
    pub fn produce(&mut self, now_ms: u64) -> Result<Option<Arc<BlockInfo>>, String> {
        if let Some(h) = &self.halted {
            return Err(format!("halted: {h}"));
        }
        let (max_inputs, max_bytes) = self.engine.block_limits();
        let max_inputs = max_inputs.min(crate::types::MAX_INPUTS_PER_BLOCK);
        let size_of = |types: &crate::types::ActionTypes, i: &Input| -> Result<usize, String> { Ok(5 + crate::types::encode_payload(types, i)?.len()) };
        let mut size = 4usize;
        let mut inputs = Vec::new();
        let last_round = self.engine.last_oracle_round();
        self.pending_oracle.retain(|r, _| *r > last_round);
        // the newest round on offer: a whole update queued by `submit_oracle` (dev oracle, tests) or the newest
        // READY round of the ingest pool (every observation held for it)
        let height = self.log.last_height + 1;
        let time = now_ms.max(self.log.last_time + 1);
        let (max_future, max_age) = self.engine.oracle_time_bounds();
        let pooled = self.oracle_pool.take_ready_at(&self.g, last_round, now_ms, time, max_future, max_age);
        let queued = self.pending_oracle.iter().next_back().map(|(_, u)| u.clone());
        let oracle = match (pooled, queued) {
            (Some(p), Some(q)) => Some(if p.round >= q.round { p } else { q }),
            (Some(u), None) | (None, Some(u)) => Some(u),
            (None, None) => None,
        };
        if let Some(u) = oracle {
            let i = Input::Oracle(u);
            match size_of(&self.g.types, &i) {
                Ok(sz) if size + sz <= max_bytes => {
                    size += sz;
                    inputs.push(i);
                }
                // an update that cannot be encoded or does not fit a block is not an input (the round is skipped)
                _ => {
                    if let Input::Oracle(u) = &i {
                        self.pending_oracle.remove(&u.round);
                    }
                }
            }
        }
        {
            let v = self.view.lock().unwrap();
            let mut nd = self.engine.next_deposit_index();
            let mut taken = 0;
            while let Some(d) = v.deposits.get(&nd) {
                let i = Input::Deposit(d.clone());
                let sz = size_of(&self.g.types, &i)?;
                if taken >= MAX_DEPOSITS_PER_BLOCK || inputs.len() >= max_inputs || size + sz > max_bytes {
                    break;
                }
                size += sz;
                inputs.push(i);
                nd += 1;
                taken += 1;
            }
            let mut nf = self.engine.next_forced_index();
            let mut taken = 0;
            while let Some(f) = v.forced.get(&nf) {
                let i = Input::Forced(f.clone());
                let sz = size_of(&self.g.types, &i)?;
                if taken >= MAX_FORCED_PER_BLOCK || inputs.len() >= max_inputs || size + sz > max_bytes {
                    break;
                }
                size += sz;
                inputs.push(i);
                nf += 1;
                taken += 1;
            }
        }
        let fixed = inputs.len();
        let mut actions: Vec<(SignedAction, H256)> = Vec::new();
        while actions.len() < MAX_ACTIONS_PER_BLOCK && fixed + actions.len() < max_inputs {
            let Some((a, _)) = self.mempool.front() else { break };
            let sz = match size_of(&self.g.types, &Input::Action(a.clone())) {
                Ok(sz) => sz,
                Err(e) => {
                    // cannot even be encoded: never an input
                    let (a, h) = self.mempool.pop_front().unwrap();
                    self.reject_ack(&h, &a, format!("MALFORMED: {e}"));
                    continue;
                }
            };
            if size + sz > max_bytes {
                break;
            }
            size += sz;
            actions.push(self.mempool.pop_front().unwrap());
        }
        for (a, _) in &actions {
            inputs.push(Input::Action(a.clone()));
        }
        if inputs.is_empty() {
            return Ok(None);
        }
        // apply on a copy first: the live engine moves only once the block is durable
        let mut next = self.engine.clone_box();
        let mut out = next.apply_block(height, time, &inputs);
        if let Err(first) = &out {
            let first = first.clone();
            // R436-4: find the inputs the engine's decoder refuses on their own
            let mut l2_cause: Option<String> = None;
            let mut dropped = 0usize;
            let mut kept_fixed = Vec::new();
            for i in inputs.drain(..fixed) {
                match self.engine.precheck_input(&i) {
                    Ok(()) => kept_fixed.push(i),
                    Err(why) => match &i {
                        Input::Oracle(u) => {
                            eprintln!("produce: oracle round {} dropped, the engine's decoder refuses it: {why}", u.round);
                            self.pending_oracle.remove(&u.round);
                            dropped += 1;
                        }
                        _ => {
                            l2_cause.get_or_insert(why);
                            kept_fixed.push(i);
                        }
                    },
                }
            }
            let mut kept_actions = Vec::new();
            for (a, h) in actions.drain(..) {
                match self.engine.precheck_input(&Input::Action(a.clone())) {
                    Ok(()) => kept_actions.push((a, h)),
                    Err(why) => {
                        eprintln!("produce: action {} dropped, the engine's decoder refuses it: {why}", to_hex(&h));
                        self.reject_ack(&h, &a, format!("MALFORMED: the engine's decoder refuses this action: {why}"));
                        dropped += 1;
                    }
                }
            }
            if dropped == 0 || l2_cause.is_some() {
                // nothing droppable explains it (a bug), or an L2 input is refused (its order cannot be skipped)
                self.halted = Some(match l2_cause {
                    Some(why) => format!("own block {height} refused by the engine: {first}; an L2 input is refused by the engine's decoder: {why}"),
                    None => format!("own block {height} refused by the engine: {first}"),
                });
                return Err(self.halted.clone().unwrap());
            }
            actions = kept_actions;
            inputs = kept_fixed;
            for (a, _) in &actions {
                inputs.push(Input::Action(a.clone()));
            }
            if inputs.is_empty() {
                return Ok(None);
            }
            next = self.engine.clone_box();
            out = next.apply_block(height, time, &inputs);
            if let Err(e) = &out {
                self.halted = Some(format!("own block {height} refused by the engine after dropping {dropped} refused input(s): {e}"));
                return Err(self.halted.clone().unwrap());
            }
        }
        let out = out.expect("checked above");
        let enc = encode_inputs(&self.g.types, &inputs)?;
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
        let pre = self.engine.real().map(PreInfo::of);
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
        self.after_apply(&b, &inputs, &out, pre.as_ref());
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
            self.prune_frames();
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

    fn is_final(&self, h: u64) -> bool {
        self.aggregator.by_height.get(&h).is_some_and(|c| c.sigs.len() >= self.g.threshold)
    }

    /// Frame retention (see `FRAMES_FINAL_KEEP`): pinned heights, the newest final ones, and the not yet final
    /// ones above the latest final.
    fn prune_frames(&mut self) {
        let lf = self.aggregator.latest_final.unwrap_or(0);
        let finals: Vec<u64> = self.frames.keys().copied().filter(|h| *h <= lf && self.is_final(*h)).collect();
        let keep_final: BTreeSet<u64> = finals.iter().rev().take(FRAMES_FINAL_KEEP).copied().collect();
        let unfinal: Vec<u64> = self.frames.keys().copied().filter(|h| *h > lf).collect();
        let keep_unfinal: BTreeSet<u64> = unfinal.iter().rev().take(FRAMES_UNFINAL_KEEP).copied().collect();
        let pinned = self.pinned_frames.clone();
        self.frames.retain(|h, _| keep_final.contains(h) || keep_unfinal.contains(h) || pinned.contains(h));
    }

    /// Pins the frame of a checkpoint height (the relayer calls this for every checkpoint the vault accepted); the
    /// newest `keep` pins stay. Persisted, so a restart rebuilds exactly these frames during replay.
    pub fn pin_frame(&mut self, height: u64, keep: usize) {
        self.pinned_frames.insert(height);
        while self.pinned_frames.len() > keep.max(1) {
            let k = *self.pinned_frames.iter().next().unwrap();
            self.pinned_frames.remove(&k);
        }
        let v: Vec<u64> = self.pinned_frames.iter().copied().collect();
        let _ = std::fs::write(self.data_dir.join("pinned-frames.json"), serde_json::json!({"heights": v}).to_string());
        self.prune_frames();
    }

    /// The real engine's state at the latest final checkpoint, if its frame is held.
    pub fn final_frame(&self) -> Option<(u64, Arc<postern_perps_engine::Engine>)> {
        let h = self.aggregator.latest_final?;
        self.frames.get(&h).map(|e| (h, e.clone()))
    }

    /// VAULT.md `claimEscape` input for `addr` against the FINAL checkpoint at `height` (default: the latest
    /// final): the account leaf fields, its 32 siblings, and the checkpoint with its signatures. Checked here
    /// before it is served: the leaf and the siblings must reproduce the checkpoint's `accountsRoot`.
    pub fn account_proof(&self, addr: &Address, height: Option<u64>) -> Result<serde_json::Value, String> {
        use postern_perps_engine as pe;
        let h = match height {
            Some(h) => h,
            None => self.aggregator.latest_final.ok_or("no final checkpoint yet")?,
        };
        if !self.is_final(h) {
            return Err(format!("no final checkpoint at height {h}"));
        }
        let e = self.frames.get(&h).ok_or_else(|| format!("the state at checkpoint {h} is no longer held by this node (held: {:?})", self.frames.keys().collect::<Vec<_>>()))?;
        let c = self.aggregator.by_height.get(&h).ok_or("checkpoint vanished")?;
        let (data, index, siblings) = e.account_proof(addr).ok_or("the account does not exist at this checkpoint")?;
        let leaf = pe::merkle::leaf_hash(&data);
        // recompute the root from the leaf and the siblings exactly as the vault does (VAULT.md 5.5)
        let mut node = leaf;
        for (i, s) in siblings.iter().enumerate() {
            node = if (index >> i) & 1 == 1 { crate::util::keccak_parts(&[&[0x01], s, &node]) } else { crate::util::keccak_parts(&[&[0x01], &node, s]) };
        }
        if siblings.len() != pe::engine::ACCOUNT_DEPTH || node != c.body.accounts_root {
            return Err("internal: the proof does not reproduce the checkpoint's accountsRoot (not served)".into());
        }
        let a = &e.accounts[index as usize];
        let escape = e.escape_value(index);
        // data = "PPv1acct" || account:20 || be32 index || be128 balance || be64 escapeValue || positionsHash:32
        let positions_hash = &data[data.len() - 32..];
        let bh = self.block(h).ok().flatten().map(|b| b.header.log_hash);
        Ok(serde_json::json!({
            "address": to_hex(addr), "checkpoint": self.aggregator.checkpoint_json(&self.g, h, bh),
            "leaf": to_hex(&leaf),
            "leafData": {"account": to_hex(&a.address), "index": index, "balance": a.balance.to_string(), "escapeValue": escape.to_string(),
                "positionsHash": to_hex(positions_hash), "encoded": to_hex(&data)},
            "proof": siblings.iter().map(|s| to_hex(s)).collect::<Vec<_>>(), "index": index.to_string(),
        }))
    }

    /// VAULT.md `withdraw(seq, w, siblings, want)` input for withdrawal `id` against a tree of the first `count`
    /// records (`withdrawalCountAt(seq)` of the accepted checkpoint the caller will name), checked against the
    /// root of that prefix before it is served.
    pub fn withdrawal_proof(&self, id: u64, count: Option<u64>, height: Option<u64>) -> Result<serde_json::Value, String> {
        let rec = self.engine.withdrawal(id).ok_or("unknown withdrawal")?;
        let (count, cp_height) = match (count, height) {
            (Some(c), _) => (c, None),
            (None, Some(h)) => {
                if !self.is_final(h) {
                    return Err(format!("no final checkpoint at height {h}"));
                }
                (self.aggregator.by_height.get(&h).map(|c| c.body.withdrawal_count).ok_or("checkpoint vanished")?, Some(h))
            }
            (None, None) => {
                let h = self.aggregator.latest_final.ok_or("no final checkpoint yet")?;
                (self.aggregator.by_height.get(&h).map(|c| c.body.withdrawal_count).ok_or("checkpoint vanished")?, Some(h))
            }
        };
        if id >= count {
            return Err(format!("withdrawal {id} is not in a tree of {count} records (not yet in that checkpoint)"));
        }
        let siblings = self.engine.withdrawal_proof(id, count).ok_or("count is above the number of withdrawals")?;
        let leaf = rec.leaf();
        let mut node = leaf;
        for (i, s) in siblings.iter().enumerate() {
            node = if (id >> i) & 1 == 1 { crate::util::keccak_parts(&[&[0x01], s, &node]) } else { crate::util::keccak_parts(&[&[0x01], &node, s]) };
        }
        if let Some(h) = cp_height {
            if self.aggregator.by_height.get(&h).map(|c| c.body.withdrawals_root) != Some(node) {
                return Err("internal: the proof does not reproduce the checkpoint's withdrawalsRoot (not served)".into());
            }
        }
        Ok(serde_json::json!({
            "withdrawalId": id.to_string(), "count": count.to_string(), "checkpointHeight": cp_height, "withdrawalsRoot": to_hex(&node),
            "record": {"withdrawalId": id.to_string(), "account": to_hex(&rec.account), "recipient": to_hex(&rec.recipient), "amount": rec.amount.to_string(), "height": rec.height.to_string()},
            "leaf": to_hex(&leaf), "siblings": siblings.iter().map(|s| to_hex(s)).collect::<Vec<_>>(),
        }))
    }

    /// API `Ticker` of a market from the engine and the index (`None` with the stub engine).
    pub fn ticker_json(&self, market_id: u32) -> Option<serde_json::Value> {
        use crate::engine::view;
        let e = self.engine.real()?;
        let ix = self.index.as_ref()?;
        let m = e.markets.get(&market_id)?;
        let (rate, next) = view::predicted_funding(e, market_id);
        let (vol, hi, lo, ago) = ix.stats_24h(market_id, e.time);
        let last = ix.last_trade(market_id).map(|x| x.0);
        // change24h = (last - price24hAgo) / price24hAgo, truncated to 6 decimals
        let change = match (last, ago) {
            (Some(l), Some(a)) if a > 0 => crate::engine::real::trim(crate::util::scaled_to_decimal_signed((l as i128 - a as i128) * 1_000_000 / a as i128, 6)),
            _ => "0".to_string(),
        };
        let z = || "0".to_string();
        Some(serde_json::json!({
            "symbol": m.p.symbol, "markPrice": view::exact8(m.mark), "indexPrice": view::exact8(m.oracle.index),
            "lastPrice": last.map(view::exact8).unwrap_or_else(z), "fundingRate": view::rate9(rate), "nextFundingTime": next,
            "openInterest": view::exact8(m.long_oi), "volume24h": view::usdc((vol / postern_perps_engine::num::N_PER_USDC as u128) as i128),
            "high24h": hi.map(view::exact8).unwrap_or_else(z), "low24h": lo.map(view::exact8).unwrap_or_else(z),
            "price24hAgo": ago.map(view::exact8).unwrap_or_else(z), "change24h": change, "height": e.height, "time": e.time,
        }))
    }

    /// API `Account` from the real engine (`finalized`: at the latest final checkpoint), else the stub's object.
    pub fn account_view(&self, a: &Address, finalized: bool) -> Result<serde_json::Value, String> {
        use crate::engine::view;
        if finalized {
            let (_, e) = self.final_frame().ok_or("no final checkpoint state is held yet")?;
            // realised PnL and funding since open are index values of the LATEST state: zero in a finalized read
            return Ok(view::account_json(&e, a, &|_| view::PositionExtras::default()));
        }
        Ok(match (self.engine.real(), self.index.as_ref()) {
            (Some(e), Some(ix)) => {
                let ai = e.by_addr.get(a).copied();
                view::account_json(e, a, &|mid| ai.map(|ai| ix.extras(ai, mid)).unwrap_or_default())
            }
            _ => self.engine.account_json(a),
        })
    }

    /// API `Agent` objects (approvedAt / height from the index; 0 when the approval predates the index).
    pub fn agents_view(&self, a: &Address) -> serde_json::Value {
        let (Some(e), Some(ix)) = (self.engine.real(), self.index.as_ref()) else { return self.engine.agents_json(a) };
        let Some(ai) = e.by_addr.get(a) else { return serde_json::json!({"agents": []}) };
        let v: Vec<_> = e.accounts[*ai as usize]
            .agents
            .iter()
            .filter(|(_, x)| x.expires_at > e.time)
            .map(|(ag, x)| {
                let (at, h) = ix.agent_meta(a, ag).unwrap_or((0, 0));
                serde_json::json!({"agent": to_hex(ag), "name": x.name, "approvedAt": at, "expiresAt": x.expires_at, "height": h})
            })
            .collect();
        serde_json::json!({"agents": v})
    }
}

fn read_pinned(dir: &std::path::Path) -> BTreeSet<u64> {
    std::fs::read(dir.join("pinned-frames.json"))
        .ok()
        .and_then(|b| serde_json::from_slice::<serde_json::Value>(&b).ok())
        .and_then(|j| j["heights"].as_array().map(|a| a.iter().filter_map(|x| x.as_u64()).collect()))
        .unwrap_or_default()
}

//! Validator: replays every block with its OWN engine and its OWN view of the L2, and signs a checkpoint only for
//! a state it computed itself.
//!
//! Per block, in order (any refusal is reported with the reason; the ones marked LATCH stop all further signing
//! until an operator investigates, because they prove the sequencer published something invalid):
//! 1. chain link: height, prevLogHash, strictly increasing time, logHash recomputed, sequencer signature (LATCH);
//! 2. block time not more than `max_future_ms` ahead of the validator's clock (wait, not latch: clocks drift),
//!    and not more than `MAX_LAG_MS` behind it when first seen (else the checkpoint covering it is not signed:
//!    R395-29, a sequencer running engine time late would trade with oracle foresight);
//! 3. inputs decode canonically (LATCH);
//! 4. every deposit and forced input equals the event at that index in the validator's own finalized L2 view
//!    (two endpoints, one block hash). Not seen yet: WAIT. Seen with different content: LATCH (the sequencer
//!    invented or altered an L2 event: this is the check that stops a sequencer from minting collateral);
//! 5. the engine applies the block (structural refusal: LATCH) and the resulting state root equals the block's
//!    `stateRoot` (else LATCH: divergence);
//! 6. at a checkpoint height: refuse to sign (not latch) while an L2-finalized forced request whose deadline is
//!    at or before the block time is still unprocessed (the vault refuses such a checkpoint anyway; signing it
//!    would only help a censoring sequencer pretend liveness);
//! 7. equivocation guard: never sign two different digests at one height (persisted when a data dir is given).
//!
//! UNAUDITED PROTOTYPE.

use crate::checkpoint::{sign_checkpoint, CheckpointBody};
use crate::engine::Engine;
use crate::genesis::Genesis;
use crate::log::check_block_link;
use crate::sig::key_address;
use crate::types::{decode_inputs, Block, Input};
use crate::util::{parse_hex_fixed, to_hex, Address, H256};
use crate::vaultfmt::{deposit_head0, forced_head0};
use crate::watcher::{L2View, SharedView};
use k256::ecdsa::SigningKey;
use std::collections::BTreeMap;
use std::io::Write;
use std::path::PathBuf;
use std::sync::Arc;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum VResult {
    /// applied; at a checkpoint height, the checkpoint body and this validator's signature
    Applied { checkpoint: Option<(CheckpointBody, [u8; 65])> },
    /// applied, but the checkpoint at this height is not signed (reason)
    AppliedNotSigned(String),
    /// not applied yet; retry later (reason)
    Wait(String),
    /// refused; `latched` = signing stopped for good
    Refused { reason: String, latched: bool },
}

/// Deterministic checkpoint-height rule (sequencer and validators compute it from the block alone).
pub fn is_checkpoint_height(g: &Genesis, height: u64, inputs: &[Input], created_withdrawals: bool) -> bool {
    height % g.checkpoint_interval == 0 || created_withdrawals || inputs.iter().any(|i| matches!(i, Input::Forced(_)))
}

/// The checkpoint body of VAULT.md 0.2 5.2 for the state after block `b`: the engine's state-root components,
/// the vault's chain heads after the credited deposits / processed forced requests (from the L2 view, whose events
/// were each checked against the chain), and the withdrawal count and total. Refused unless the components
/// reproduce the block's state root (the equation the vault recomputes).
pub fn checkpoint_body(g: &Genesis, e: &dyn Engine, b: &Block, view: &L2View) -> Result<CheckpointBody, String> {
    let dc = e.next_deposit_index();
    let fp = e.forced_processed();
    let deposits_head = if dc == 0 {
        deposit_head0(700_770, &g.vault)
    } else {
        view.deposits.get(&(dc - 1)).map(|d| d.head).ok_or("deposit chain head not in the L2 view")?
    };
    let forced_head = if fp == 0 {
        forced_head0(700_770, &g.vault)
    } else {
        view.forced.get(&(fp - 1)).map(|f| f.head).ok_or("forced chain head not in the L2 view")?
    };
    let p = e.state_parts();
    let body = CheckpointBody {
        height: b.header.height,
        time: b.header.time,
        log_hash: b.header.log_hash,
        accounts_root: p.accounts_root,
        markets_root: p.markets_root,
        withdrawals_root: p.withdrawals_root,
        next_deposit_index: dc,
        next_forced_index: fp,
        next_order_id: p.next_order_id,
        total_escape: p.total_escape,
        dust: p.dust,
        aux_hash: p.aux_hash,
        deposits_head,
        forced_head,
        withdrawal_count: e.withdrawal_count(),
        withdrawals_total: p.withdrawals_total,
    };
    if body.state_root() != b.header.state_root {
        return Err("the engine's state-root components do not reproduce the block's state root".into());
    }
    Ok(body)
}

/// R395-29: a block first seen by this validator more than this after its own `time` taints the checkpoint that
/// covers it (the sequencer ran engine time behind real time). Honest lag is milliseconds (block time = the
/// sequencer's clock at production); residual foresight bound = MAX_LAG_MS + the oracle's maxFutureMs.
pub const MAX_LAG_MS: u64 = 2_000;

/// Margin between "now on the L2" and the time a signed checkpoint reaches the vault (seconds).
pub const FORCED_SUBMIT_MARGIN_S: u64 = 120;

pub struct Validator {
    pub g: Arc<Genesis>,
    key: SigningKey,
    pub address: Address,
    pub engine: Box<dyn Engine>,
    pub height: u64,
    pub log_hash: H256,
    pub time: u64,
    pub view: SharedView,
    pub latched: Option<String>,
    pub max_future_ms: u64,
    signed: BTreeMap<u64, H256>,
    signed_file: Option<PathBuf>,
    pub signed_count: u64,
    /// first height since the last checkpoint height whose block was first seen more than MAX_LAG_MS after its time
    pub late_since_checkpoint: Option<u64>,
    /// R395-37/38: when this validator FIRST received each not-yet-applied block (before any wait on its own L2
    /// view), keyed by (height, logHash): another block at the same height has its own first-seen time
    first_seen: BTreeMap<(u64, H256), u64>,
}

impl Validator {
    pub fn new(g: Arc<Genesis>, key: SigningKey, engine: Box<dyn Engine>, view: SharedView, data_dir: Option<PathBuf>) -> Result<Validator, String> {
        let address = key_address(&key);
        if g.committee.binary_search(&address).is_err() {
            return Err(format!("validator key {} is not in the genesis committee", to_hex(&address)));
        }
        let mut signed = BTreeMap::new();
        let mut signed_file = None;
        if let Some(d) = data_dir {
            std::fs::create_dir_all(&d).map_err(|e| e.to_string())?;
            let p = d.join("signed.log");
            if let Ok(s) = std::fs::read_to_string(&p) {
                for line in s.lines() {
                    let mut it = line.split(' ');
                    let h: u64 = it.next().and_then(|x| x.parse().ok()).ok_or("signed.log line")?;
                    let dg = parse_hex_fixed::<32>(it.next().ok_or("signed.log line")?)?;
                    signed.insert(h, dg);
                }
            }
            signed_file = Some(p);
        }
        Ok(Validator {
            log_hash: g.genesis_log_hash(),
            time: g.genesis_time,
            g,
            key,
            address,
            engine,
            height: 0,
            view,
            latched: None,
            max_future_ms: 5_000,
            signed,
            signed_file,
            signed_count: 0,
            late_since_checkpoint: None,
            first_seen: BTreeMap::new(),
        })
    }

    fn latch(&mut self, reason: String) -> VResult {
        self.latched = Some(reason.clone());
        VResult::Refused { reason, latched: true }
    }

    pub fn process(&mut self, b: &Block, now_ms: u64) -> VResult {
        if let Some(r) = &self.latched {
            return VResult::Refused { reason: format!("latched: {r}"), latched: true };
        }
        if b.header.height <= self.height {
            return VResult::Refused { reason: format!("block {} already applied", b.header.height), latched: false };
        }
        // R395-37: lateness is judged at RECEIPT (the first call for THIS block); waiting for this validator's own
        // L2 view never counts. R395-38: keyed by the block's logHash, so an early placeholder at the same height
        // cannot lend its first-seen time to a late block that replaces it.
        let seen = {
            let k = (b.header.height, b.header.log_hash);
            let lo = self.height;
            self.first_seen.retain(|(h, _), _| *h > lo);
            if self.first_seen.len() >= 10_000 && !self.first_seen.contains_key(&k) {
                return VResult::Wait("first-seen table full".into());
            }
            *self.first_seen.entry(k).or_insert(now_ms)
        };
        if let Err(e) = check_block_link(&self.g, self.height, &self.log_hash, self.time, b) {
            if b.header.height == self.height + 1 {
                return self.latch(e);
            }
            return VResult::Wait(format!("gap: have {}, got {}", self.height, b.header.height));
        }
        if b.header.time > now_ms.saturating_add(self.max_future_ms) {
            return VResult::Wait("block time too far ahead of this validator's clock".into());
        }
        // R395-29: lower bound, judged at the first time this validator sees the block (now)
        let late = seen.saturating_sub(b.header.time) > MAX_LAG_MS;
        let inputs = match decode_inputs(&self.g.types, &b.inputs_encoded) {
            Ok(i) => i,
            Err(e) => return self.latch(format!("block {}: inputs do not decode: {e}", b.header.height)),
        };
        {
            let v = self.view.lock().unwrap();
            for inp in &inputs {
                match inp {
                    Input::Deposit(d) => match v.deposits.get(&d.index) {
                        None => return VResult::Wait(format!("deposit {} not yet seen finalized by this validator", d.index)),
                        Some(mine) if !mine.consensus_eq(d) => {
                            let r = format!("block {}: deposit {} differs from the L2-finalized event", b.header.height, d.index);
                            drop(v);
                            return self.latch(r);
                        }
                        _ => {}
                    },
                    Input::Forced(f) => match v.forced.get(&f.index) {
                        None => return VResult::Wait(format!("forced request {} not yet seen finalized", f.index)),
                        Some(mine) if !mine.consensus_eq(f) => {
                            let r = format!("block {}: forced request {} differs from the L2 event", b.header.height, f.index);
                            drop(v);
                            return self.latch(r);
                        }
                        _ => {}
                    },
                    _ => {}
                }
            }
        }
        let out = match self.engine.apply_block(b.header.height, b.header.time, &inputs) {
            Ok(o) => o,
            Err(e) => return self.latch(format!("block {}: engine refused the block: {e}", b.header.height)),
        };
        let root = self.engine.state_root();
        if root != b.header.state_root {
            return self.latch(format!(
                "block {}: DIVERGENCE: my state root {} != block's {}",
                b.header.height,
                to_hex(&root),
                to_hex(&b.header.state_root)
            ));
        }
        self.height = b.header.height;
        self.log_hash = b.header.log_hash;
        self.time = b.header.time;
        let applied_h = b.header.height;
        self.first_seen.retain(|(h, _), _| *h > applied_h);
        if late && self.late_since_checkpoint.is_none() {
            self.late_since_checkpoint = Some(b.header.height);
        }
        if !is_checkpoint_height(&self.g, b.header.height, &inputs, !out.withdrawals.is_empty()) {
            return VResult::Applied { checkpoint: None };
        }
        if let Some(h) = self.late_since_checkpoint.take() {
            return VResult::AppliedNotSigned(format!(
                "block {h} (covered by this checkpoint) was first seen more than {MAX_LAG_MS} ms after its time: not signing (R395-29; also after a restart or pause of this validator)"
            ));
        }
        // forced-inclusion rule = the vault's acceptance rule 4, in L2 block SECONDS: the newest L2 block time
        // this validator's endpoints report, plus a submission margin
        let body = {
            let v = self.view.lock().unwrap();
            let next = self.engine.forced_processed();
            if let Some((i, f)) = v.forced.range(next..).next() {
                if f.deadline <= v.l2_time_s.saturating_add(FORCED_SUBMIT_MARGIN_S) {
                    return VResult::AppliedNotSigned(format!("forced request {i} is past its deadline and not processed: not signing"));
                }
            }
            match checkpoint_body(&self.g, self.engine.as_ref(), b, &v) {
                Ok(body) => body,
                Err(e) => return VResult::AppliedNotSigned(format!("cannot build the checkpoint body: {e}")),
            }
        };
        let digest = body.digest(&self.g.vault_domain);
        if let Some(prev) = self.signed.get(&body.height) {
            if *prev != digest {
                return VResult::AppliedNotSigned(format!("EQUIVOCATION GUARD: already signed a different checkpoint at height {}", body.height));
            }
        } else {
            if let Some(p) = &self.signed_file {
                let r = std::fs::OpenOptions::new().create(true).append(true).open(p).and_then(|mut f| {
                    writeln!(f, "{} {}", body.height, to_hex(&digest))?;
                    f.sync_data()
                });
                if let Err(e) = r {
                    return VResult::AppliedNotSigned(format!("cannot persist the equivocation guard: {e}"));
                }
            }
            self.signed.insert(body.height, digest);
        }
        let sig = sign_checkpoint(&self.key, &body, &self.g.vault_domain);
        self.signed_count += 1;
        VResult::Applied { checkpoint: Some((body, sig)) }
    }
}

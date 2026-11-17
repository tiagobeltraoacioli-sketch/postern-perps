//! Validator: replays every block with its OWN engine and its OWN view of the L2, and signs a checkpoint only for
//! a state it computed itself.
//!
//! Per block, in order (any refusal is reported with the reason; the ones marked LATCH stop all further signing
//! until an operator investigates, because they prove the sequencer published something invalid):
//! 1. chain link: height, prevLogHash, strictly increasing time, logHash recomputed, sequencer signature (LATCH);
//! 2. block time not more than `max_future_ms` ahead of the validator's clock (wait, not latch: clocks drift);
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
use crate::vaultfmt::{deposit_head0, escape_leaf, forced_head0, sum_root};
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

/// The checkpoint body of VAULT.md 5.2 for the state after block `b`: engine counters and roots, the vault's
/// chain heads after the credited deposits / processed forced requests (from the L2 view, whose events were
/// checked against their own chain), and the withdrawal and escape sum trees (VAULT.md 5.5).
pub fn checkpoint_body(g: &Genesis, e: &dyn Engine, b: &Block, view: &L2View) -> Result<CheckpointBody, String> {
    let dc = e.next_deposit_index();
    let fp = e.next_forced_index();
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
    let n = e.withdrawal_count();
    let mut wl = Vec::with_capacity(n as usize);
    for i in 0..n {
        let w = e.withdrawal(i).ok_or("withdrawal record missing")?;
        wl.push((w.leaf(), w.amount));
    }
    let (withdrawals_root, withdrawals_total) = sum_root(&wl).ok_or("withdrawals total above u64")?;
    let el: Vec<(H256, u64)> = e.escape_entries().into_iter().filter(|(_, v)| *v > 0).map(|(a, v)| (escape_leaf(&a, v), v)).collect();
    let (escape_root, escape_total) = sum_root(&el).ok_or("escape total above u64")?;
    Ok(CheckpointBody {
        height: b.header.height,
        time: b.header.time,
        state_root: b.header.state_root,
        log_hash: b.header.log_hash,
        deposits_credited: dc,
        deposits_head,
        forced_processed: fp,
        forced_head,
        withdrawal_count: n,
        withdrawals_total,
        withdrawals_root,
        escape_total,
        escape_root,
    })
}

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
        if let Err(e) = check_block_link(&self.g, self.height, &self.log_hash, self.time, b) {
            if b.header.height == self.height + 1 {
                return self.latch(e);
            }
            return VResult::Wait(format!("gap: have {}, got {}", self.height, b.header.height));
        }
        if b.header.time > now_ms.saturating_add(self.max_future_ms) {
            return VResult::Wait("block time too far ahead of this validator's clock".into());
        }
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
        if !is_checkpoint_height(&self.g, b.header.height, &inputs, !out.withdrawals.is_empty()) {
            return VResult::Applied { checkpoint: None };
        }
        // forced-inclusion rule (the vault's acceptance rule 4, judged at the block time; the vault uses its own
        // L2 timestamp, so this is the validator's best local view)
        let body = {
            let v = self.view.lock().unwrap();
            let next = self.engine.next_forced_index();
            if let Some((i, f)) = v.forced.range(next..).next() {
                if f.deadline_ms() <= b.header.time {
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

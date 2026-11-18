//! The interface between the node and the exchange engine.
//!
//! The real engine is agent 388's crate in `engine/` (deterministic matching, margin, funding, liquidation,
//! Merkle state root). It did not exist when this node was written, so the node is built against this trait and
//! ships `StubEngine`, a deliberately small, deterministic LEDGER (collateral, nonces, agents, resting orders
//! without matching, withdrawals) that exercises every node path: admission, ordering, the log, snapshots,
//! recovery, validator replay, checkpoints, withdrawal certificates, deposits and forced actions. It is NOT the
//! exchange. When `engine/` lands, an adapter implements this trait over it; nothing else in the node changes.
//!
//! Contract every implementation must keep (the validators' agreement rests on it):
//! - `apply_block` is a pure function of (state, height, time, inputs): no clock, no randomness, no floats, no
//!   iteration over unordered containers;
//! - on `Err` (a structurally invalid block) the state is unchanged; a per-input business failure is an
//!   `InputOutcome { ok: false }` and never an `Err`;
//! - `restore(snapshot())` yields a state with the same `state_root()`.
//!
//! UNAUDITED PROTOTYPE.

pub mod real;
pub mod stub;

use crate::types::Input;
use crate::util::{keccak_parts, Address, H256};

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct InputOutcome {
    pub ok: bool,
    pub error: Option<String>,
    pub action_hash: Option<H256>,
    pub result: serde_json::Value,
}

#[derive(Clone, Debug, Default)]
pub struct BlockOutcome {
    pub outcomes: Vec<InputOutcome>,
    pub events: Vec<serde_json::Value>,
    /// Withdrawal ids created by this block.
    pub withdrawals: Vec<u64>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct WithdrawalRecord {
    pub id: u64,
    pub account: Address,
    pub recipient: Address,
    /// net amount paid by the vault (SPEC S-9.2: gross minus the flat fee)
    pub amount: u64,
    pub fee: u64,
    pub height: u64,
}

impl WithdrawalRecord {
    /// SPEC 11 / VAULT.md 0.2 5.5 withdrawal leaf, computed by the engine crate's own function:
    /// `keccak256(0x00 || "PPv1wdrl" || be64 id || account || recipient || be64 amount || be64 height)`.
    pub fn leaf(&self) -> H256 {
        let r = postern_perps_engine::engine::WithdrawalRecord { id: self.id, account: self.account, recipient: self.recipient, amount: self.amount, height: self.height };
        postern_perps_engine::merkle::leaf_hash(&postern_perps_engine::Engine::withdrawal_leaf_data(&r))
    }

    pub fn to_json(&self) -> serde_json::Value {
        use crate::util::{scaled_to_decimal, to_hex};
        serde_json::json!({
            "index": self.id.to_string(), "account": to_hex(&self.account), "recipient": to_hex(&self.recipient),
            "amount": scaled_to_decimal(self.amount as u128, 6), "fee": scaled_to_decimal(self.fee as u128, 6),
            "amountRaw": self.amount.to_string(), "height": self.height, "leaf": to_hex(&self.leaf()),
        })
    }
}

/// Positional binary Merkle tree over leaves: interior `keccak256(0x01 || left || right)`. An odd last node is
/// promoted unchanged (never paired with itself: duplication would let two different leaf lists share a root);
/// the root of an empty list is `keccak256(0x02)`. The verifier takes the leaf count, which the checkpoint binds
/// (`withdrawalCount`), so a promoted node cannot be passed off as a leaf of a smaller tree.
pub fn merkle_root(leaves: &[H256]) -> H256 {
    if leaves.is_empty() {
        return keccak_parts(&[&[0x02]]);
    }
    let mut level: Vec<H256> = leaves.to_vec();
    while level.len() > 1 {
        let mut next = Vec::with_capacity(level.len().div_ceil(2));
        for pair in level.chunks(2) {
            if pair.len() == 2 {
                next.push(keccak_parts(&[&[0x01], &pair[0], &pair[1]]));
            } else {
                next.push(pair[0]);
            }
        }
        level = next;
    }
    level[0]
}

/// Proof for leaf `index`: sibling hashes bottom-up; a level where the node was promoted contributes nothing.
pub fn merkle_proof(leaves: &[H256], index: usize) -> Option<Vec<H256>> {
    if index >= leaves.len() {
        return None;
    }
    let mut proof = Vec::new();
    let mut level: Vec<H256> = leaves.to_vec();
    let mut i = index;
    while level.len() > 1 {
        let sib = i ^ 1;
        if sib < level.len() {
            proof.push(level[sib]);
        }
        let mut next = Vec::with_capacity(level.len().div_ceil(2));
        for pair in level.chunks(2) {
            if pair.len() == 2 {
                next.push(keccak_parts(&[&[0x01], &pair[0], &pair[1]]));
            } else {
                next.push(pair[0]);
            }
        }
        level = next;
        i /= 2;
    }
    Some(proof)
}

/// Verifies a proof for leaf `index` of a tree of `count` leaves (the verifier the vault would run).
pub fn merkle_verify(root: &H256, leaf: &H256, index: u64, count: u64, proof: &[H256]) -> bool {
    if index >= count {
        return false;
    }
    let mut h = *leaf;
    let mut i = index;
    let mut n = count;
    let mut p = proof.iter();
    while n > 1 {
        let sib = i ^ 1;
        if sib < n {
            let s = match p.next() {
                Some(s) => s,
                None => return false,
            };
            h = if i % 2 == 0 { keccak_parts(&[&[0x01], &h, s]) } else { keccak_parts(&[&[0x01], s, &h]) };
        }
        i /= 2;
        n = n.div_ceil(2);
    }
    p.next().is_none() && &h == root
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct StateParts {
    pub accounts_root: H256,
    pub markets_root: H256,
    pub withdrawals_root: H256,
    pub next_order_id: u64,
    pub total_escape: u128,
    pub dust: u128,
    pub aux_hash: H256,
    pub withdrawal_count: u64,
    pub withdrawals_total: u64,
}

/// SPEC 0.1.5 section 11 state root from its components (the vault's `stateRootOf`, VAULT 64072cb).
pub fn state_root_of(height: u64, time: u64, next_deposit: u64, next_forced: u64, p: &StateParts) -> H256 {
    let sys = keccak_parts(&[
        b"PPv1sys",
        &next_deposit.to_be_bytes(),
        &next_forced.to_be_bytes(),
        &p.next_order_id.to_be_bytes(),
        &p.total_escape.to_be_bytes(),
        &p.dust.to_be_bytes(),
        &p.aux_hash,
        &p.withdrawal_count.to_be_bytes(),
        &p.withdrawals_total.to_be_bytes(),
    ]);
    keccak_parts(&[b"PosternPerps/state/v1", &height.to_be_bytes(), &time.to_be_bytes(), &p.accounts_root, &p.markets_root, &p.withdrawals_root, &sys])
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AuthorityError {
    UnknownSigner,
    AgentNotAllowed,
}

pub trait Engine: Send + Sync {
    fn apply_block(&mut self, height: u64, time: u64, inputs: &[Input]) -> Result<BlockOutcome, String>;
    fn state_root(&self) -> H256;
    fn height(&self) -> u64;
    fn time(&self) -> u64;
    fn snapshot(&self) -> Vec<u8>;
    fn restore(&mut self, bytes: &[u8]) -> Result<(), String>;
    fn next_deposit_index(&self) -> u64;
    /// Forced requests RECEIVED (the next input index the engine accepts).
    fn next_forced_index(&self) -> u64;
    /// Forced requests PROCESSED (SPEC 0.1.5 / VAULT 5.2 `nextForcedIndex`; a request refused for a halt-caused
    /// reason is received but not processed, R395-5).
    fn forced_processed(&self) -> u64 {
        self.next_forced_index()
    }
    fn last_oracle_round(&self) -> u64;
    /// Admission pre-filter: may `signer` act for `account` with action `primary` at engine time `time`?
    fn authority(&self, account: &Address, signer: &Address, primary: &str, time: u64) -> Result<(), AuthorityError>;
    fn highest_nonce(&self, signer: &Address) -> Option<u64>;
    /// The genesis `logHash(0)` this engine starts from.
    fn genesis_log_hash(&self) -> H256;
    fn withdrawal_count(&self) -> u64;
    fn withdrawal(&self, id: u64) -> Option<WithdrawalRecord>;
    /// The components of SPEC 11's state root that the vault's checkpoint carries (VAULT.md 0.2 5.2).
    fn state_parts(&self) -> StateParts;
    /// Depth-32 siblings (bottom-up) of withdrawal `id` in the tree of the first `count` records.
    fn withdrawal_proof(&self, id: u64, count: u64) -> Option<Vec<H256>>;
    /// SPEC S-2.5 nonce rule against the applied state (admission adds its own pending set).
    fn nonce_ok(&self, signer: &Address, nonce: u64, time: u64) -> Result<(), &'static str>;
    fn account_json(&self, a: &Address) -> serde_json::Value;
    fn orders_json(&self, a: &Address) -> serde_json::Value;
    fn agents_json(&self, a: &Address) -> serde_json::Value;
    fn name(&self) -> &'static str;
    fn clone_box(&self) -> Box<dyn Engine>;
    /// The market table the engine ENFORCES, as API `Market` objects (R393-N1: the API publishes exactly this).
    fn market_table(&self, _g: &crate::genesis::Genesis) -> Option<Vec<serde_json::Value>> {
        None
    }
    /// Aggregated resting book of a market in engine units: (bids descending, asks ascending) of (price, size).
    fn book_levels(&self, _market_id: u32) -> Option<(Vec<(u64, u64)>, Vec<(u64, u64)>)> {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::util::keccak;

    #[test]
    fn merkle_every_size_every_index() {
        for n in 1..=33usize {
            let leaves: Vec<H256> = (0..n).map(|i| keccak(&(i as u64).to_be_bytes())).collect();
            let root = merkle_root(&leaves);
            for i in 0..n {
                let p = merkle_proof(&leaves, i).unwrap();
                assert!(merkle_verify(&root, &leaves[i], i as u64, n as u64, &p), "n={n} i={i}");
                // wrong index, wrong count, wrong leaf, extra element: refused
                assert!(!merkle_verify(&root, &leaves[i], ((i + 1) % n) as u64, n as u64, &p) || n == 1 || leaves[i] == leaves[(i + 1) % n]);
                assert!(!merkle_verify(&root, &keccak(b"x"), i as u64, n as u64, &p));
                let mut longer = p.clone();
                longer.push(root);
                assert!(!merkle_verify(&root, &leaves[i], i as u64, n as u64, &longer));
                assert!(!merkle_verify(&root, &leaves[i], n as u64, n as u64, &p));
            }
        }
        assert_ne!(merkle_root(&[]), merkle_root(&[keccak(b"")]));
    }
}

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
    pub amount: u128,
    pub fee: u128,
    pub height: u64,
}

impl WithdrawalRecord {
    /// PROPOSED leaf (for agent 390's `docs/VAULT.md`):
    /// `keccak256(0x00 || abi.encode(uint64 id, address account, address recipient, uint256 amount, uint256 fee, uint64 height))`.
    /// The 0x00 / 0x01 prefixes separate leaves from interior nodes (no second-preimage across levels).
    pub fn leaf(&self) -> H256 {
        use crate::util::{word_address, word_u128};
        keccak_parts(&[
            &[0x00],
            &word_u128(self.id as u128),
            &word_address(&self.account),
            &word_address(&self.recipient),
            &word_u128(self.amount),
            &word_u128(self.fee),
            &word_u128(self.height as u128),
        ])
    }

    pub fn to_json(&self) -> serde_json::Value {
        use crate::util::{scaled_to_decimal, to_hex};
        serde_json::json!({
            "id": self.id.to_string(), "account": to_hex(&self.account), "recipient": to_hex(&self.recipient),
            "amount": scaled_to_decimal(self.amount, 6), "fee": scaled_to_decimal(self.fee, 6),
            "amountRaw": self.amount.to_string(), "feeRaw": self.fee.to_string(), "height": self.height,
            "leaf": to_hex(&self.leaf()),
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
    fn next_forced_index(&self) -> u64;
    fn last_oracle_round(&self) -> u64;
    /// Admission pre-filter: may `signer` act for `account` with action `primary` at engine time `time`?
    fn authority(&self, account: &Address, signer: &Address, primary: &str, time: u64) -> Result<(), AuthorityError>;
    fn highest_nonce(&self, signer: &Address) -> Option<u64>;
    fn withdrawal_count(&self) -> u64;
    fn withdrawals_root(&self) -> H256;
    /// Root over the first `count` withdrawals (the root a past checkpoint committed to; the list is append-only).
    fn withdrawals_root_at(&self, count: u64) -> Option<H256>;
    fn withdrawal(&self, id: u64) -> Option<WithdrawalRecord>;
    /// Proof of withdrawal `id` in the tree of the first `count` withdrawals.
    fn withdrawal_proof(&self, id: u64, count: u64) -> Option<Vec<H256>>;
    fn account_json(&self, a: &Address) -> serde_json::Value;
    fn orders_json(&self, a: &Address) -> serde_json::Value;
    fn agents_json(&self, a: &Address) -> serde_json::Value;
    fn name(&self) -> &'static str;
    fn clone_box(&self) -> Box<dyn Engine>;
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

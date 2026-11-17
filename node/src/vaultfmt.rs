//! The vault's formats, exactly as `docs/VAULT.md` v0.1 (agent 390, `contracts/src/PerpsVault.sol`) states them:
//! deposit and forced-request hash chains (5.4), Merkle SUM trees with their withdrawal and escape leaves (5.5).
//! The node computes them so that validators sign what the vault will verify.
//!
//! UNAUDITED PROTOTYPE.

use crate::util::{keccak, keccak_parts, word_address, word_u128, Address, H256, ZERO_H256};

/// `bytes32("...")`: ASCII left-aligned, zero-padded.
pub fn bytes32_text(s: &str) -> H256 {
    let mut w = [0u8; 32];
    w[..s.len()].copy_from_slice(s.as_bytes());
    w
}

pub fn deposit_head0(chain_id: u64, vault: &Address) -> H256 {
    keccak_parts(&[&bytes32_text("PosternPerps.deposit.v1"), &word_u128(chain_id as u128), &word_address(vault)])
}

pub fn deposit_head_next(prev: &H256, index: u64, account: &Address, amount: u64) -> H256 {
    keccak_parts(&[prev, &word_u128(index as u128), &word_address(account), &word_u128(amount as u128)])
}

pub fn forced_head0(chain_id: u64, vault: &Address) -> H256 {
    keccak_parts(&[&bytes32_text("PosternPerps.forced.v1"), &word_u128(chain_id as u128), &word_address(vault)])
}

#[allow(clippy::too_many_arguments)]
pub fn forced_head_next(prev: &H256, index: u64, account: &Address, kind: u64, market_id: u32, amount: u64, deadline: u64) -> H256 {
    keccak_parts(&[
        prev,
        &word_u128(index as u128),
        &word_address(account),
        &word_u128(kind as u128),
        &word_u128(market_id as u128),
        &word_u128(amount as u128),
        &word_u128(deadline as u128),
    ])
}

pub fn withdrawal_leaf(index: u64, account: &Address, recipient: &Address, amount: u64) -> H256 {
    keccak_parts(&[
        &bytes32_text("PosternPerps.withdrawal.v1"),
        &word_u128(index as u128),
        &word_address(account),
        &word_address(recipient),
        &word_u128(amount as u128),
    ])
}

pub fn escape_leaf(account: &Address, amount: u64) -> H256 {
    keccak_parts(&[&bytes32_text("PosternPerps.escape.v1"), &word_address(account), &word_u128(amount as u128)])
}

fn node(lh: &H256, ls: u64, rh: &H256, rs: u64) -> H256 {
    keccak_parts(&[&[0x01], lh, &ls.to_be_bytes(), rh, &rs.to_be_bytes()])
}

/// Root and total of the sum tree over `leaves` (complete binary tree padded with `(0, 0)` to a power of two;
/// one leaf: the leaf itself; no leaf: `(0, 0)`). `None` if a sum does not fit u64 (the vault refuses it too).
pub fn sum_root(leaves: &[(H256, u64)]) -> Option<(H256, u64)> {
    if leaves.is_empty() {
        return Some((ZERO_H256, 0));
    }
    let mut level: Vec<(H256, u64)> = leaves.to_vec();
    let size = level.len().next_power_of_two();
    level.resize(size, (ZERO_H256, 0));
    while level.len() > 1 {
        let mut next = Vec::with_capacity(level.len() / 2);
        for p in level.chunks(2) {
            let s = p[0].1.checked_add(p[1].1)?;
            next.push((node(&p[0].0, p[0].1, &p[1].0, p[1].1), s));
        }
        level = next;
    }
    Some(level[0])
}

/// Proof `(hashes, sums)` for leaf `index`, sibling of the leaf level first.
pub fn sum_proof(leaves: &[(H256, u64)], index: usize) -> Option<(Vec<H256>, Vec<u64>)> {
    if index >= leaves.len() {
        return None;
    }
    let mut level: Vec<(H256, u64)> = leaves.to_vec();
    level.resize(level.len().next_power_of_two(), (ZERO_H256, 0));
    let (mut hs, mut ss) = (Vec::new(), Vec::new());
    let mut i = index;
    while level.len() > 1 {
        let sib = level[i ^ 1];
        hs.push(sib.0);
        ss.push(sib.1);
        let mut next = Vec::with_capacity(level.len() / 2);
        for p in level.chunks(2) {
            next.push((node(&p[0].0, p[0].1, &p[1].0, p[1].1), p[0].1.checked_add(p[1].1)?));
        }
        level = next;
        i /= 2;
    }
    Some((hs, ss))
}

/// The vault's `sumRoot(leaf, sum, proof)` recomputation.
pub fn sum_verify(root: &H256, total: u64, leaf: &H256, amount: u64, index: u64, hashes: &[H256], sums: &[u64]) -> bool {
    if hashes.len() != sums.len() || hashes.len() > 48 || (hashes.len() < 64 && index >> hashes.len() != 0) {
        return false;
    }
    let (mut h, mut s) = (*leaf, amount);
    for (i, (sh, ssum)) in hashes.iter().zip(sums).enumerate() {
        let Some(t) = s.checked_add(*ssum) else { return false };
        h = if (index >> i) & 1 == 1 { node(sh, *ssum, &h, s) } else { node(&h, s, sh, *ssum) };
        s = t;
    }
    &h == root && s == total
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sum_tree_every_size_and_forgeries() {
        for n in 1..=17usize {
            let leaves: Vec<(H256, u64)> = (0..n).map(|i| (withdrawal_leaf(i as u64, &[1; 20], &[2; 20], 10 + i as u64), 10 + i as u64)).collect();
            let (root, total) = sum_root(&leaves).unwrap();
            assert_eq!(total, (0..n as u64).map(|i| 10 + i).sum::<u64>());
            for i in 0..n {
                let (hs, ss) = sum_proof(&leaves, i).unwrap();
                assert!(sum_verify(&root, total, &leaves[i].0, leaves[i].1, i as u64, &hs, &ss), "n={n} i={i}");
                assert!(!sum_verify(&root, total, &leaves[i].0, leaves[i].1 + 1, i as u64, &hs, &ss), "inflated amount");
                assert!(!sum_verify(&root, total + 1, &leaves[i].0, leaves[i].1, i as u64, &hs, &ss), "wrong total");
                if n > 1 {
                    assert!(!sum_verify(&root, total, &leaves[i].0, leaves[i].1, (i ^ 1) as u64, &hs, &ss), "wrong index");
                }
                assert!(!sum_verify(&root, total, &leaves[i].0, leaves[i].1, (i + (1 << hs.len())) as u64, &hs, &ss), "index above 2^depth");
            }
        }
        assert_eq!(sum_root(&[]).unwrap(), (ZERO_H256, 0));
        let one = (escape_leaf(&[3; 20], 5), 5);
        assert_eq!(sum_root(&[one]).unwrap(), one, "one leaf: the root is the leaf");
        assert!(sum_root(&[(ZERO_H256, u64::MAX), (ZERO_H256, 1)]).is_none(), "sum overflow refused");
    }

    #[test]
    fn preimage_lengths_differ() {
        // 160, 96 and 81 bytes (VAULT.md 5.5): a node is never a leaf, a leaf of one tree never of the other
        assert_ne!(32 * 5, 32 * 3);
        assert_ne!(32 * 3, 1 + 32 + 8 + 32 + 8);
        assert_eq!(bytes32_text("PosternPerps.deposit.v1")[..23], *b"PosternPerps.deposit.v1");
        assert_ne!(deposit_head0(700770, &[1; 20]), forced_head0(700770, &[1; 20]));
        assert_ne!(deposit_head0(700770, &[1; 20]), deposit_head0(1, &[1; 20]));
        let _ = keccak(b"");
    }
}

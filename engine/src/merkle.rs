//! Fixed-depth binary Merkle trees over a dense prefix of leaf positions (SPEC section 11).
//! Leaves `H(0x00 || data)`, nodes `H(0x01 || left || right)`, empty subtree of height 0 = `bytes32(0)`,
//! `Z(i+1) = H(0x01 || Z(i) || Z(i))`.
use crate::hash::{keccak_parts, H256};

pub fn leaf_hash(data: &[u8]) -> H256 {
    keccak_parts(&[&[0x00], data])
}

pub fn node_hash(l: &H256, r: &H256) -> H256 {
    keccak_parts(&[&[0x01], l, r])
}

pub fn zeros(depth: usize) -> Vec<H256> {
    let mut z = vec![[0u8; 32]];
    for i in 0..depth {
        let n = node_hash(&z[i], &z[i]);
        z.push(n);
    }
    z
}

/// Root of a depth-`depth` tree whose positions `0..leaves.len()` hold `leaves` (already leaf-hashed) and the
/// rest are empty.
pub fn root(leaves: &[H256], depth: usize) -> H256 {
    assert!(depth < 64 && (leaves.len() as u128) <= (1u128 << depth));
    let z = zeros(depth);
    let mut level: Vec<H256> = leaves.to_vec();
    for d in 0..depth {
        if level.is_empty() {
            return z[depth];
        }
        let mut next = Vec::with_capacity(level.len().div_ceil(2));
        for pair in level.chunks(2) {
            let r = if pair.len() == 2 { pair[1] } else { z[d] };
            next.push(node_hash(&pair[0], &r));
        }
        level = next;
    }
    if level.is_empty() {
        z[depth]
    } else {
        level[0]
    }
}

/// Bottom-up siblings of position `idx`.
pub fn proof(leaves: &[H256], depth: usize, idx: usize) -> Vec<H256> {
    assert!(idx < leaves.len());
    let z = zeros(depth);
    let mut level: Vec<H256> = leaves.to_vec();
    let mut i = idx;
    let mut out = Vec::with_capacity(depth);
    for d in 0..depth {
        let sib = i ^ 1;
        out.push(if sib < level.len() { level[sib] } else { z[d] });
        let mut next = Vec::with_capacity(level.len().div_ceil(2));
        for pair in level.chunks(2) {
            let r = if pair.len() == 2 { pair[1] } else { z[d] };
            next.push(node_hash(&pair[0], &r));
        }
        level = next;
        i >>= 1;
    }
    out
}

/// What the vault does: bit `i` of `idx` = 1 means the node is a right child at level `i`.
pub fn verify(leaf: &H256, idx: u64, siblings: &[H256], expected_root: &H256) -> bool {
    if siblings.len() < 64 && (idx >> siblings.len()) != 0 {
        return false;
    }
    let mut h = *leaf;
    for (d, s) in siblings.iter().enumerate() {
        h = if (idx >> d) & 1 == 1 { node_hash(s, &h) } else { node_hash(&h, s) };
    }
    &h == expected_root
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn proofs_verify_for_every_position() {
        for n in 1..20usize {
            let leaves: Vec<H256> = (0..n).map(|i| leaf_hash(&[i as u8])).collect();
            let r = root(&leaves, 8);
            for i in 0..n {
                let p = proof(&leaves, 8, i);
                assert!(verify(&leaves[i], i as u64, &p, &r));
                assert!(!verify(&leaves[i], (i ^ 1) as u64, &p, &r), "wrong position must fail");
            }
        }
    }
    #[test]
    fn empty_root_is_z_depth() {
        assert_eq!(root(&[], 4), zeros(4)[4]);
    }
    #[test]
    fn index_out_of_range_refused() {
        let leaves = vec![leaf_hash(b"a")];
        let r = root(&leaves, 4);
        let p = proof(&leaves, 4, 0);
        assert!(!verify(&leaves[0], 16, &p, &r));
    }
}

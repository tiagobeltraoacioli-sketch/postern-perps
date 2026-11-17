//! The vault's chain formats, exactly as `docs/VAULT.md` v0.2 5.4 (agent 390, contracts 27c1969) states them.
//! (VAULT 0.1's sum trees were dropped by 390 in 0.2 with a written argument; the trees are SPEC 11's, computed by
//! the engine crate.)
//!
//! UNAUDITED PROTOTYPE.

use crate::util::{keccak_parts, word_address, word_u128, Address, H256};

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

/// VAULT.md 0.2: `keccak256(abi.encode(prev, uint256 i, address account, uint256 kind, uint256 deadline, uint256 arg))`.
pub fn forced_head_next(prev: &H256, index: u64, account: &Address, kind: u64, deadline: u64, arg: u64) -> H256 {
    keccak_parts(&[prev, &word_u128(index as u128), &word_address(account), &word_u128(kind as u128), &word_u128(deadline as u128), &word_u128(arg as u128)])
}


/// Shared vectors of agent 390: `contracts/test/vectors/vault-vectors.json` (schema 0.2, contracts 27c1969,
/// produced by `forge test --mc PerpsVaultVectors`). The node must reproduce every output byte for byte.
#[cfg(test)]
mod vault_vectors {
    use super::*;
    use crate::checkpoint::{CheckpointBody, CHECKPOINT_TYPE};
    use crate::eip712::Domain;
    use crate::engine::WithdrawalRecord;
    use crate::util::{keccak, parse_address, to_hex};

    fn k(s: &str) -> H256 {
        keccak(s.as_bytes())
    }

    #[test]
    fn heads_domain_checkpoint_digest_state_root_leaf_and_roots() {
        let vault = parse_address("0xF62849F9A0B5Bf2913b396098F7c7019b51A820a").unwrap();
        assert_eq!(to_hex(&deposit_head0(700770, &vault)), "0x14ac17057b39ed17e5d150480e7c42180a7fa1d604e22282eaa0a49379e5f875");
        assert_eq!(to_hex(&forced_head0(700770, &vault)), "0x76fabf4cf7ebd9898533505e4a5dbcef50ed361022328cd954e05c41f9e95696");
        let dom = Domain { name: "PosternPerpsVault".into(), version: "1".into(), chain_id: 700770, verifying_contract: Some(vault) };
        assert_eq!(to_hex(&dom.separator()), "0xd8612207db80974d15f1485a3a41ac5d3ea80eecc91cde21caaa2d6287031cd5");
        assert_eq!(to_hex(&keccak(CHECKPOINT_TYPE.as_bytes())), "0xa11fd1f0c03526baa4a08e1e667b8972d6ed98015080bcd9c7e4e7c4cbf7bcb6");
        let cp = CheckpointBody {
            height: 10,
            time: 1_800_000_000_000,
            log_hash: k("log"),
            accounts_root: k("accounts"),
            markets_root: k("markets"),
            withdrawals_root: k("withdrawals"),
            next_deposit_index: 2,
            next_forced_index: 1,
            next_order_id: 77,
            total_escape: 5_000_000,
            dust: 9,
            aux_hash: k("aux"),
            deposits_head: k("dh"),
            forced_head: k("fh"),
            withdrawal_count: 4,
            withdrawals_total: 1_000_000,
        };
        assert_eq!(to_hex(&cp.state_root()), "0xe72601eaf3f450e42a4936525576728e24a4c75301e50dc379caca768cbfec22");
        assert_eq!(to_hex(&cp.digest(&dom)), "0xedbdd9a9ebd4e2e3767ec12a7c2e2fa499fd328953513ff27eb7ad385e3163f9");
        let w = WithdrawalRecord {
            id: 3,
            account: parse_address("0x1111111111111111111111111111111111111111").unwrap(),
            recipient: parse_address("0x2222222222222222222222222222222222222222").unwrap(),
            amount: 1_234_567,
            fee: 0,
            height: 99,
        };
        assert_eq!(to_hex(&w.leaf()), "0xe9166baba646062a6c6d061b7bd43516cf74f9893efb8dc38fe32d02a0b77bd9");
        let abc = [k("a"), k("b"), k("c")];
        assert_eq!(to_hex(&postern_perps_engine::merkle::root(&abc, 32)), "0x8fbe559c6533a8a7b143ff5aeb5a68f790da1a04469c65f6b7a3a8895fa97d89");
        assert_eq!(to_hex(&postern_perps_engine::merkle::root(&[], 32)), "0x7c1d0e8a93ea9c09cc13b91ead8f72de66a33cb695c30934dc2d75bffac1248e");
        // a body whose stateRoot field disagrees with its components is refused
        let mut j = cp.to_json();
        j["stateRoot"] = to_hex(&k("x")).into();
        assert!(CheckpointBody::from_json(&j).is_err());
        assert_eq!(CheckpointBody::from_json(&cp.to_json()).unwrap(), cp);
    }
}

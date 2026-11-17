//! Agent 395 demonstrations against node `c9bb54b` (copy into `node/tests/` next to `common/` and run
//! `cargo test --test r395_node`). Each test PASSES while the defect is present. THROWAWAY keys only.
mod common;
use common::*;
use perps_node::types::{ForcedEvent, ForcedKind};
use perps_node::util::keccak;
use perps_node::validator::VResult;
use perps_node::sig::key_address;

/// R395-29: a block whose time lies 60 s BEHIND every validator's clock is applied and its checkpoint signed by all
/// seven validators (validator.rs checks only `time > now + max_future_ms`). A sequencer can run engine time behind
/// real time (every nonce/oracle/age rule of SPEC is judged in engine time) while it already holds the newer oracle
/// observations.
#[test]
fn r395_29_lagging_block_time_is_signed() {
    let mut n = net("r395-29", 1);
    let a = key_address(&user(1));
    n.deposit_everywhere(deposit(0, a, 1_000_000_000));
    let lagging = now() - 60_000;
    let info = n.seq.produce(lagging).unwrap().expect("a block");
    let b = info.block.as_ref().clone();
    assert!(b.header.time <= lagging + 1);
    let mut signed = 0;
    for v in n.vals.iter_mut() {
        match v.process(&b, now()) {
            VResult::Applied { checkpoint: Some(_) } => signed += 1,
            other => panic!("expected a signed checkpoint, got {other:?}"),
        }
    }
    assert_eq!(signed, 7, "every validator signed a block 60 s behind its clock");
}

/// R395-30 (units): the vault's ForcedRequest deadline is `block.timestamp + forcedDelay` in SECONDS
/// (PerpsVault.sol requestForced); validator.rs compares it with the block time in MILLISECONDS. A request whose
/// deadline is one hour in the future is treated as overdue at once and no validator signs.
#[test]
fn r395_30_forced_deadline_seconds_vs_block_ms() {
    let mut n = net("r395-30", 1);
    let a = key_address(&user(2));
    n.deposit_everywhere(deposit(0, a, 1_000_000_000));
    let deadline_seconds = now() / 1000 + 3600; // one hour ahead, as the vault writes it
    let f = ForcedEvent { index: 0, account: a, kind: ForcedKind::CancelAll, deadline: deadline_seconds, l2_block: 200,
        l2_block_hash: keccak(b"f"), l2_tx_hash: keccak(b"t"), log_index: 1 };
    // validators have seen it finalized; the sequencer has not yet (normal lag between views)
    for v in &n.val_views { v.lock().unwrap().forced.insert(0, f.clone()); }
    let rs = n.step();
    assert_eq!(rs.len(), 7);
    for r in rs {
        match r {
            VResult::AppliedNotSigned(why) => assert!(why.contains("past its deadline"), "{why}"),
            other => panic!("expected refusal to sign, got {other:?}"),
        }
    }
}

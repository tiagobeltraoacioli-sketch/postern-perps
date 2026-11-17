//! Agent 395 re-verification of R395-29 at node `dc2c235` (copy into `node/tests/`, run
//! `cargo test --test r395_node_v2 -- --nocapture`). THROWAWAY keys only.
mod common;
use common::*;
use perps_node::sig::key_address;
use perps_node::validator::{VResult, Validator};

fn signed_count(rs: &[VResult]) -> usize {
    rs.iter().filter(|r| matches!(r, VResult::Applied { checkpoint: Some(_) })).count()
}

fn one_block_lagging(name: &str, lag: u64) -> usize {
    let mut n = net(name, 1);
    let a = key_address(&user(1));
    n.deposit_everywhere(deposit(0, a, 1_000_000_000));
    let t = now();
    let info = n.seq.produce(t - lag).unwrap().expect("block");
    let b = info.block.as_ref().clone();
    let rs: Vec<VResult> = n.vals.iter_mut().map(|v| v.process(&b, t)).collect();
    signed_count(&rs)
}

/// Bound: 1.9 s late -> signed by all; 2.1 s late -> signed by none.
#[test]
fn r395_29_bound_inside_and_outside() {
    let inside = one_block_lagging("r29-in", 1_900);
    let outside = one_block_lagging("r29-out", 2_100);
    println!("inside 1.9 s: {inside}/7 signed; outside 2.1 s: {outside}/7 signed");
    assert_eq!(inside, 7);
    assert_eq!(outside, 0);
}

/// Liveness probe: an honest, fresh block carries a deposit the validators' own L2 view has not yet finalized.
/// They WAIT; when their view catches up 3 s later the block is processed with `now` 3 s after its time and the
/// checkpoint is refused, although the sequencer was honest. Prints the outcome (the assertion records today's
/// behaviour).
#[test]
fn r395_37_honest_block_waiting_for_l2_view_is_refused() {
    let mut n = net("r37", 1);
    let a = key_address(&user(2));
    let d = deposit(0, a, 1_000_000_000);
    n.seq_view.lock().unwrap().deposits.insert(0, d.clone());
    let t = now();
    let info = n.seq.produce(t).unwrap().expect("block");
    let b = info.block.as_ref().clone();
    let first: Vec<VResult> = n.vals.iter_mut().map(|v| v.process(&b, t)).collect();
    assert!(first.iter().all(|r| matches!(r, VResult::Wait(_))), "{first:?}");
    for v in &n.val_views { v.lock().unwrap().deposits.insert(0, d.clone()); }
    let later: Vec<VResult> = n.vals.iter_mut().map(|v| v.process(&b, t + 3_000)).collect();
    let s = signed_count(&later);
    println!("honest block, validators' L2 view 3 s behind: {s}/7 signed; first result {:?}", later[0]);
    assert_eq!(s, 0, "R395-37: honest checkpoint refused because the validator's own L2 view lagged");
}

/// Restart: a validator that starts from genesis and replays blocks produced earlier refuses the checkpoints
/// covering those blocks, then signs again once it sees fresh blocks.
#[test]
fn r395_29_restarted_validator_resumes_on_fresh_blocks() {
    let mut n = net("r29-restart", 1);
    let a = key_address(&user(3));
    n.deposit_everywhere(deposit(0, a, 1_000_000_000));
    let t = now();
    let b1 = n.seq.produce(t).unwrap().expect("b1").block.as_ref().clone();
    let k = n.keys.validators[0].clone();
    let v = n.val_views[0].clone();
    let mut fresh = Validator::new(n.g.clone(), k, fresh_engine(&n.g), v, None).unwrap();
    let r1 = fresh.process(&b1, t + 10_000); // replayed 10 s later
    n.deposit_everywhere(deposit(1, a, 1_000_000_000));
    let b2 = n.seq.produce(t + 10_000).unwrap().expect("b2").block.as_ref().clone();
    let r2 = fresh.process(&b2, t + 10_050);
    println!("replayed: {r1:?}\nfresh: {:?}", matches!(r2, VResult::Applied { checkpoint: Some(_) }));
    assert!(matches!(r1, VResult::AppliedNotSigned(_)));
    assert!(matches!(r2, VResult::Applied { checkpoint: Some(_) }));
}

//! Agent 395 re-verification at node `f1578be` (copy into `node/tests/`, `cargo test --test r395_node_v3 -- --nocapture`).
//! THROWAWAY keys only.
mod common;
use common::*;
use perps_node::sig::key_address;
use perps_node::validator::VResult;

fn signed(rs: &[VResult]) -> usize { rs.iter().filter(|r| matches!(r, VResult::Applied { checkpoint: Some(_) })).count() }

/// R395-37 as fixed: an honest block whose deposit reaches the validators' L2 view 3 s later is signed 7/7.
#[test]
fn r395_37_honest_wait_signed() {
    let mut n = net("v3-37", 1);
    let a = key_address(&user(1));
    let d = deposit(0, a, 1_000_000_000);
    n.seq_view.lock().unwrap().deposits.insert(0, d.clone());
    let t = now();
    let b = n.seq.produce(t).unwrap().unwrap().block.as_ref().clone();
    let first: Vec<VResult> = n.vals.iter_mut().map(|v| v.process(&b, t)).collect();
    assert!(first.iter().all(|r| matches!(r, VResult::Wait(_))));
    for v in &n.val_views { v.lock().unwrap().deposits.insert(0, d.clone()); }
    let later: Vec<VResult> = n.vals.iter_mut().map(|v| v.process(&b, t + 3_000)).collect();
    println!("honest wait: {}/7", signed(&later));
    assert_eq!(signed(&later), 7);
}

/// R395-38: first receipt is keyed by HEIGHT, not by the block. The sequencer sends, on time, a placeholder for
/// height 1 that waits (its deposit is not finalized in the validators' view), then 60 s later delivers a DIFFERENT,
/// valid block for height 1 whose time is 60 s behind the validators' clock. It is judged by the placeholder's
/// receipt time and signed: the R395-29 lag bound is bypassed. PASSES while the defect is present.
#[test]
fn r395_38_placeholder_height_bypasses_lag_bound() {
    let mut n = net("v3-38", 1);
    let mut other = net("v3-38", 1); // same seed: same genesis and sequencer key, separate log
    let a = key_address(&user(2));
    let t = now();
    // placeholder from `n`'s sequencer: carries deposit 0, which the validators have not seen
    n.seq_view.lock().unwrap().deposits.insert(0, deposit(0, a, 1_000_000_000));
    let placeholder = n.seq.produce(t).unwrap().unwrap().block.as_ref().clone();
    let first: Vec<VResult> = n.vals.iter_mut().map(|v| v.process(&placeholder, t)).collect();
    assert!(first.iter().all(|r| matches!(r, VResult::Wait(_))), "{first:?}");
    // the real block for height 1, produced with time t (engine time), delivered at t + 60 s
    let d1 = deposit(0, a, 2_000_000_000);
    other.seq_view.lock().unwrap().deposits.insert(0, d1.clone());
    for v in &n.val_views { v.lock().unwrap().deposits.insert(0, d1.clone()); }
    let real = other.seq.produce(t).unwrap().unwrap().block.as_ref().clone();
    assert_eq!(real.header.height, placeholder.header.height);
    assert_ne!(real.header.log_hash, placeholder.header.log_hash);
    let late: Vec<VResult> = n.vals.iter_mut().map(|v| v.process(&real, t + 60_000)).collect();
    println!("block 60 s behind, height pre-registered by a placeholder: {}/7 signed; {:?}", signed(&late), late[0]);
    assert_eq!(signed(&late), 7, "R395-38: lag bound bypassed");
}

//! Agent 436 (round 2 review) demonstrations against the node on `main` `f0e3967`. Copy into `node/tests/` of an
//! export and run `cargo test --release --test r436_node -- --nocapture`. A test PASSES when the DEFECT IS PRESENT.
//! THROWAWAY keys only. UNAUDITED review tooling, never merged into `main`.
mod common;
use common::*;
use perps_node::sig::key_address;

/// R436-3 (Medium, liveness). `Sequencer::produce` (sequencer.rs:404-419) puts EVERY pending finalized deposit and
/// forced request of its L2 view into the next block, with no cap, then up to 5,000 actions. The engine refuses a
/// block of more than `MAX_INPUTS = 10_000` inputs (`BlockError::TooLarge`), and the sequencer treats a refusal of
/// its own block as a bug and HALTS (sequencer.rs:430-434). The L2 finalizes in batches (a block per proof, minutes
/// apart), so more than 10,000 deposits can become final between two engine blocks: anyone can do it with
/// minimum-size deposits (gas only; the deposits stay theirs). After a restart the same block is rebuilt and
/// refused again: the exchange stays down until the code changes, and users are left with the escape hatch.
#[test]
fn r436_3_a_batch_of_finalized_deposits_halts_the_sequencer_for_good() {
    let mut n = net_real("r436-3", 10);
    {
        let mut v = n.seq_view.lock().unwrap();
        for i in 0..10_001u64 {
            let a = key_address(&user(1_000 + (i % 50)));
            v.deposits.insert(i, deposit(i, a, 10_000_000));
        }
    }
    let r = n.seq.produce(now());
    println!("produce with 10,001 pending deposits: {:?}", r.as_ref().map(|_| ()).map_err(|e| e.clone()));
    assert!(r.is_err(), "the sequencer's own block is refused by the engine");
    assert!(n.seq.halted.is_some(), "and the sequencer halts");
    println!("halted: {:?}", n.seq.halted);
    // no later call recovers: admission and production both refuse
    assert!(n.seq.produce(now()).is_err());
    // a restart over the same data directory rebuilds the same block and halts again
    let (g, keys, checked) = perps_node::dev::dev_genesis_real("test-r436-3", 7, 5, 10, 0);
    let eng = Box::new(perps_node::engine::real::RealEngine::new(g.clone(), checked).unwrap());
    let dir = n.dir.clone();
    let view = n.seq_view.clone();
    drop(n);
    let mut again = perps_node::sequencer::Sequencer::open(g, keys.sequencer.clone(), eng, dir, view).unwrap();
    assert!(again.halted.is_none(), "a restarted sequencer starts un-halted");
    assert!(again.produce(now()).is_err(), "and halts on its first block");
    assert!(again.halted.is_some());
    assert_eq!(again.log.last_height, 0, "no block was ever produced");
}

/// R436-4 (High, liveness). One signed `ApproveAgent` whose `name` is 33 bytes halts the sequencer. The node's
/// admission accepts it (eip712.rs caps strings at 256 bytes; the signature is valid; `authority` returns Ok for
/// `account == signer` without asking whether the account exists, so NO deposit is needed). `produce` puts it in
/// the next block; the engine's DECODER refuses the whole input list ("agent name > 32 bytes", engine
/// codec.rs:163-165) instead of failing that one action (SPEC 3.2: an invalid individual input never rejects the
/// block; `apply_action` already has the per-action MALFORMED check); the sequencer treats the refusal of its own
/// block as a bug and halts (sequencer.rs:430-434). Every other action drained into that block is lost. After a
/// restart the same request halts it again: a kill switch for anyone who can reach POST /v1/actions.
#[test]
fn r436_4_one_action_with_a_33_byte_agent_name_halts_the_sequencer() {
    let mut n = net_real("r436-4", 10);
    let g = n.g.clone();
    let k = user(7); // never deposited: no account in the engine
    let a = key_address(&k);
    let t = now();
    let honest = user(8);
    let ha = key_address(&honest);
    n.deposit_everywhere(deposit(0, ha, 1_000_000_000));
    n.seq.produce(t).unwrap().unwrap();
    let good = withdraw(&g, &honest, ha, t + 1, 2_000_000, ha);
    n.seq.admit_signed(good, t).expect("an honest action is admitted");
    let poison = signed(
        &g,
        &k,
        "ApproveAgent",
        serde_json::json!({"account": perps_node::util::to_hex(&a), "nonce": (t + 2).to_string(), "agent": perps_node::util::to_hex(&[9u8; 20]),
            "name": "x".repeat(33), "expiresAt": (t + 3_600_000).to_string()}),
    );
    let ack = n.seq.admit(&request(&g, &poison), t);
    println!("admission of the 33-byte-name ApproveAgent from an address with no account: {:?}", ack.as_ref().map(|a| a.status).map_err(|e| e.code));
    assert!(ack.is_ok(), "admitted");
    let r = n.seq.produce(t + 5);
    println!("produce: {:?}", r.as_ref().map(|_| ()).map_err(|e| e.clone()));
    assert!(r.is_err());
    assert!(n.seq.halted.is_some(), "the sequencer halted");
    assert_eq!(n.seq.mempool_len(), 0, "the honest withdrawal drained into the refused block is gone");
    let later = n.seq.admit(&request(&g, &withdraw(&g, &honest, ha, t + 9, 2_000_000, ha)), t + 9);
    assert_eq!(later.err().map(|e| e.http), Some(503), "nothing more is admitted");
    assert!(n.seq.produce(t + 10).is_err(), "and nothing more is produced");
}

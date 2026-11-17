//! Sequencer / validator / checkpoint / certificate / recovery tests (no network).

mod common;
use common::*;
use perps_node::checkpoint::{verify_checkpoint_json, verify_signatures, AddResult, CertError, CheckpointBody};
#[allow(unused_imports)]
use perps_node::engine::Engine;
use perps_node::log::block_digest;
use perps_node::sequencer::Sequencer;
use perps_node::sig::{key_address, sign};
use perps_node::types::{decode_inputs, encode_inputs, ForcedEvent, ForcedKind, Input};
use perps_node::util::{keccak, parse_hex_fixed, to_hex};
use perps_node::validator::VResult;

fn all_applied(rs: &[VResult]) -> bool {
    rs.iter().all(|r| matches!(r, VResult::Applied { .. }))
}

#[test]
fn deposit_withdraw_checkpoint_certificate_end_to_end() {
    let mut n = net("e2e", 4);
    let u = user(1);
    let ua = key_address(&u);
    n.deposit_everywhere(deposit(0, ua, 5_000_000_000));
    let rs = n.step();
    assert!(all_applied(&rs), "{rs:?}");
    assert_eq!(n.seq.engine.account_json(&ua)["collateral"], "5000.000000");
    // a withdrawal creates a checkpoint at its block
    let t = now();
    let w = withdraw(&n.g, &u, ua, t, 1_000_000_000, ua);
    let ack = n.seq.admit(&request(&n.g, &w), t).unwrap();
    assert_eq!(ack.status, "accepted");
    let rs = n.step();
    let signed = rs.iter().filter(|r| matches!(r, VResult::Applied { checkpoint: Some(_) })).count();
    assert_eq!(signed, 7, "{rs:?}");
    let h = n.seq.log.last_height;
    assert_eq!(n.seq.latest_final(), Some(h));
    assert_eq!(n.seq.acks[&ack.action_hash].status, "included");
    // the certificate verifies with the vault's rules from public data only
    let cert = n.seq.certificate(0).expect("certificate");
    let cp = &cert["checkpoint"];
    let body = verify_checkpoint_json(&n.g, cp).expect("checkpoint verifies");
    assert_eq!(cp["signatures"].as_array().unwrap().len(), 5, "exactly threshold signatures");
    let rec = n.seq.engine.withdrawal(0).unwrap();
    assert_eq!(rec.amount, 999_000_000, "SPEC S-9.2: the record carries gross minus the flat fee");
    assert_eq!(rec.fee, 1_000_000, "flat withdrawal fee from markets.json defaults");
    let sib: Vec<[u8; 32]> = cert["proof"]["siblings"].as_array().unwrap().iter().map(|p| parse_hex_fixed::<32>(p.as_str().unwrap()).unwrap()).collect();
    assert_eq!(sib.len(), 32, "SPEC 11: exactly 32 siblings");
    assert!(postern_perps_engine::merkle::verify(&rec.leaf(), 0, &sib, &body.withdrawals_root));
    assert!(!postern_perps_engine::merkle::verify(&rec.leaf(), 1, &sib, &body.withdrawals_root), "wrong position refused");
    assert_eq!(body.state_root(), n.seq.block(body.height).unwrap().unwrap().header.state_root, "components reproduce the replayed root");
    assert_eq!(body.withdrawals_total, 999_000_000);
    // chain heads in the checkpoint are the vault's, taken from the (checked) L2 events
    assert_eq!(body.next_deposit_index, 1);
    assert_eq!(body.deposits_head, n.seq_view.lock().unwrap().deposits[&0].head);
    assert_eq!(body.forced_head, perps_node::vaultfmt::forced_head0(700770, &n.g.vault));
    // escape tree total = what accounts are owed (stub: balances), here the user and the fee account
    assert_eq!(body.total_escape, 5_000_000_000 - 1_000_000_000 + 1_000_000);
    // the fee went to the owner's address account
    let fee_acc = perps_node::util::parse_address(perps_node::dev::OWNER_FEE_ADDRESS).unwrap();
    assert_eq!(n.seq.engine.account_json(&fee_acc)["collateral"], "1.000000");
    assert_eq!(n.seq.engine.account_json(&ua)["collateral"], "4000.000000");
    // every validator computed the same root as the sequencer
    for v in &n.vals {
        assert_eq!(v.engine.state_root(), n.seq.engine.state_root());
    }
}

#[test]
fn admission_and_engine_refuse_forgery_replay_window_and_authority() {
    let mut n = net("adm", 10);
    let u = user(2);
    let ua = key_address(&u);
    let t = now();
    // forged: signed by another key for this account
    let other = user(3);
    let forged = place_order(&n.g, &other, ua, t, 1, 672505, 10, false);
    let e = n.seq.admit(&request(&n.g, &forged), t).unwrap_err();
    assert_eq!(e.code, "UNKNOWN_SIGNER");
    // tampered message after signing
    let mut tampered = request(&n.g, &place_order(&n.g, &u, ua, t, 1, 672505, 10, false));
    tampered["message"]["size"] = "1000".into();
    let e = n.seq.admit(&tampered, t).unwrap_err();
    assert_eq!(e.code, "UNKNOWN_SIGNER", "a changed message recovers another address");
    // malformed signature
    let mut bad = request(&n.g, &place_order(&n.g, &u, ua, t + 1, 1, 672505, 10, false));
    bad["signature"] = to_hex(&[0u8; 65]).into();
    assert_eq!(n.seq.admit(&bad, t).unwrap_err().code, "BAD_SIGNATURE");
    // good, then replay, then lower nonce
    let ok = place_order(&n.g, &u, ua, t + 2, 1, 672505, 10, false);
    n.seq.admit(&request(&n.g, &ok), t).unwrap();
    assert_eq!(n.seq.admit(&request(&n.g, &ok), t).unwrap_err().code, "NONCE_USED");
    // SPEC 2.5 (replaces the strict-increase rule of the pre-SPEC proposal): a lower, unused nonce inside the
    // window is valid (out-of-order arrival); the same nonce twice never is, pending or applied
    let lower = place_order(&n.g, &u, ua, t + 1, 1, 672505, 11, false);
    n.seq.admit(&request(&n.g, &lower), t).unwrap();
    let lower_again = place_order(&n.g, &u, ua, t + 1, 1, 672505, 12, false);
    assert_eq!(n.seq.admit(&request(&n.g, &lower_again), t).unwrap_err().code, "NONCE_USED", "pending nonce reused");
    // window
    let old = place_order(&n.g, &u, ua, t - 121_000, 1, 672505, 10, false);
    assert_eq!(n.seq.admit(&request(&n.g, &old), t).unwrap_err().code, "NONCE_WINDOW");
    let future = place_order(&n.g, &u, ua, t + 31_000, 1, 672505, 10, false);
    assert_eq!(n.seq.admit(&request(&n.g, &future), t).unwrap_err().code, "NONCE_WINDOW");
    assert!(all_applied(&n.step()));
    assert_eq!(n.seq.admit(&request(&n.g, &lower_again), t).unwrap_err().code, "NONCE_USED", "applied nonce reused");
    // agent: approved by the main key, may trade, may not withdraw or approve
    let ag = user(4);
    let aga = key_address(&ag);
    let appr = signed(&n.g, &u, "ApproveAgent", serde_json::json!({"account": to_hex(&ua), "nonce": (t + 3).to_string(), "agent": to_hex(&aga), "name": "bot", "expiresAt": (t + 3_600_000).to_string()}));
    n.seq.admit(&request(&n.g, &appr), t).unwrap();
    assert!(all_applied(&n.step()));
    let by_agent = place_order(&n.g, &ag, ua, t + 4, 1, 672505, 10, false);
    n.seq.admit(&request(&n.g, &by_agent), t).unwrap();
    let wd = withdraw(&n.g, &ag, ua, t + 5, 1, aga);
    assert_eq!(n.seq.admit(&request(&n.g, &wd), t).unwrap_err().code, "AGENT_NOT_ALLOWED");
    assert!(all_applied(&n.step()));
    let orders = n.seq.engine.orders_json(&ua);
    assert_eq!(orders["orders"].as_array().unwrap().len(), 3, "t+2, t+1 and the agent's order");
}

/// The engine itself refuses (deterministically, in replay) what admission would have refused: a malicious
/// sequencer that skips admission gains nothing.
#[test]
fn engine_rechecks_every_action_in_replay() {
    let (g, _k) = perps_node::dev::dev_genesis("engine-recheck", 7, 5, 10, 0);
    let mut e = fresh_engine(&g);
    let u = user(5);
    let ua = key_address(&u);
    let t = 1_800_000_000_000u64;
    let a1 = place_order(&g, &u, ua, t, 1, 10, 1, false);
    let forged = place_order(&g, &user(6), ua, t + 1, 1, 10, 1, false);
    let replay = a1.clone();
    let stale = place_order(&g, &u, ua, t - 200_000, 1, 10, 1, false);
    let zero_price = place_order(&g, &u, ua, t + 2, 1, 0, 1, false);
    let inputs: Vec<Input> = [a1, forged, replay, stale, zero_price].into_iter().map(Input::Action).collect();
    let out = e.apply_block(1, t, &inputs).unwrap();
    let codes: Vec<Option<String>> = out.outcomes.iter().map(|o| o.error.clone()).collect();
    assert_eq!(
        codes,
        vec![None, Some("UNKNOWN_SIGNER".into()), Some("NONCE_USED".into()), Some("NONCE_WINDOW".into()), Some("PRICE_REQUIRED".into())]
    );
    // deposits out of order refuse the whole block, state unchanged
    let root = e.state_root();
    let r = e.apply_block(2, t + 1, &[Input::Deposit(deposit(1, ua, 5))]);
    assert!(r.is_err());
    assert_eq!(e.state_root(), root);
    assert!(e.apply_block(2, t, &[Input::Deposit(deposit(0, ua, 5))]).is_err(), "time must increase");
    assert!(e.apply_block(3, t + 1, &[Input::Deposit(deposit(0, ua, 5))]).is_err(), "height must follow");
    assert_eq!(e.state_root(), root);
}

#[test]
fn validators_never_sign_an_invented_or_altered_deposit() {
    let mut n = net("dep-lie", 1);
    let ua = key_address(&user(7));
    // only the sequencer "sees" this deposit: validators wait, nobody signs, nothing is final
    n.seq_view.lock().unwrap().deposits.insert(0, deposit(0, ua, 1_000_000));
    let rs = n.step();
    assert!(rs.iter().all(|r| matches!(r, VResult::Wait(_))), "{rs:?}");
    assert_eq!(n.seq.latest_final(), None);
    // validators' views now hold a DIFFERENT event at index 0 (the sequencer altered the amount): latch
    for v in &n.val_views {
        v.lock().unwrap().deposits.insert(0, deposit(0, ua, 999));
    }
    let b = n.seq.block(1).unwrap().unwrap();
    for v in n.vals.iter_mut() {
        match v.process(&b, now()) {
            VResult::Refused { latched: true, reason } => assert!(reason.contains("differs"), "{reason}"),
            r => panic!("expected latch, got {r:?}"),
        }
        // a latched validator refuses everything afterwards
        assert!(matches!(v.process(&b, now()), VResult::Refused { latched: true, .. }));
    }
    assert_eq!(n.seq.latest_final(), None);
}

#[test]
fn validators_latch_on_a_state_root_lie_or_a_bad_sequencer_signature() {
    let mut n = net("root-lie", 1);
    let ua = key_address(&user(8));
    n.deposit_everywhere(deposit(0, ua, 1_000_000));
    n.seq.produce(now()).unwrap();
    let good = n.seq.block(1).unwrap().unwrap();
    // 1: state root changed and re-signed by the (malicious) sequencer key
    let mut lie = good.clone();
    lie.header.state_root = keccak(b"another state");
    lie.sequencer_signature = sign(&n.keys.sequencer, &block_digest(&n.g, &lie));
    match n.vals[0].process(&lie, now()) {
        VResult::Refused { latched: true, reason } => assert!(reason.contains("DIVERGENCE"), "{reason}"),
        r => panic!("{r:?}"),
    }
    // 2: inputs changed, signature not redone
    let mut forged = good.clone();
    forged.header.time += 1;
    match n.vals[1].process(&forged, now()) {
        VResult::Refused { latched: true, .. } => {}
        r => panic!("{r:?}"),
    }
    // 3: signed by a key that is not the genesis sequencer
    let mut imp = good.clone();
    imp.sequencer_signature = sign(&user(9), &block_digest(&n.g, &imp));
    match n.vals[2].process(&imp, now()) {
        VResult::Refused { latched: true, reason } => assert!(reason.contains("sequencer"), "{reason}"),
        r => panic!("{r:?}"),
    }
    // the honest block is still accepted by an untouched validator
    assert!(matches!(n.vals[3].process(&good, now()), VResult::Applied { .. }));
}

#[test]
fn forced_request_past_deadline_blocks_signing_until_processed() {
    let mut n = net("forced", 1);
    let u = user(10);
    let ua = key_address(&u);
    n.deposit_everywhere(deposit(0, ua, 50_000_000));
    assert!(all_applied(&n.step()));
    // validators see a forced withdraw whose deadline has passed; the sequencer "censors" it (its view lacks it)
    let f = ForcedEvent { index: 0, account: ua, kind: ForcedKind::Withdraw, deadline: 1, arg: 0, head: keccak(b"fh0"), l2_block: 200, l2_block_hash: keccak(b"b200"), l2_tx_hash: keccak(b"t200"), log_index: 0 };
    for v in &n.val_views {
        v.lock().unwrap().forced.insert(0, f.clone());
    }
    let t = now();
    n.seq.admit(&request(&n.g, &place_order(&n.g, &u, ua, t, 1, 10, 1, false)), t).unwrap();
    let rs = n.step();
    assert!(rs.iter().all(|r| matches!(r, VResult::AppliedNotSigned(s) if s.contains("deadline"))), "{rs:?}");
    let fin_before = n.seq.latest_final();
    // once the sequencer includes it, signing resumes and the forced withdraw is in the checkpoint
    n.seq_view.lock().unwrap().forced.insert(0, f);
    let rs = n.step();
    assert!(rs.iter().all(|r| matches!(r, VResult::Applied { checkpoint: Some(_) })), "{rs:?}");
    assert!(n.seq.latest_final() > fin_before);
    let body = n.seq.checkpoint_for(n.seq.log.last_height).unwrap();
    assert_eq!(body.next_forced_index, 1);
    assert_eq!(body.withdrawal_count, 1);
    let rec = n.seq.engine.withdrawal(0).unwrap();
    assert_eq!((rec.recipient, rec.amount), (ua, 49_000_000), "forced withdraw: everything minus the flat fee, to the account itself");
    assert_eq!(body.forced_head, keccak(b"fh0"), "the forced chain head after 1 request is the event's");
}

#[test]
fn checkpoint_signature_rules_match_the_vault() {
    let mut n = net("cp-rules", 1);
    n.deposit_everywhere(deposit(0, key_address(&user(11)), 1));
    assert!(all_applied(&n.step()));
    let body = n.seq.checkpoint_for(1).unwrap();
    let d = body.digest(&n.g.vault_domain);
    let sigs: Vec<[u8; 65]> = n.keys.validators.iter().map(|k| sign(k, &d)).collect();
    let c = &n.g.committee;
    assert!(verify_signatures(c, 5, &d, &sigs[..5]).is_ok());
    assert_eq!(verify_signatures(c, 5, &d, &sigs[..4]), Err(CertError::Count { got: 4, need: 5 }));
    assert!(verify_signatures(c, 5, &d, &sigs[..6]).is_ok(), "the vault accepts more than threshold");
    assert!(verify_signatures(c, 5, &d, &sigs).is_ok());
    let mut rev = sigs[..5].to_vec();
    rev.swap(0, 1);
    assert_eq!(verify_signatures(c, 5, &d, &rev), Err(CertError::NotAscending));
    let mut dup = sigs[..5].to_vec();
    dup[1] = dup[0];
    assert_eq!(verify_signatures(c, 5, &d, &dup), Err(CertError::NotAscending));
    let mut stranger = sigs[..5].to_vec();
    stranger[4] = sign(&user(12), &d);
    assert!(verify_signatures(c, 5, &d, &stranger).is_err());
    // a signature over another body (a validator with another root) is refused by the aggregator
    let mut other = body.clone();
    other.accounts_root = keccak(b"x");
    let s_other = sign(&n.keys.validators[0], &other.digest(&n.g.vault_domain));
    assert!(n.seq.add_signature(1, s_other).is_err());
    // duplicates are idempotent
    assert_eq!(n.seq.aggregator.add(&n.g, 1, sigs[0]).unwrap(), AddResult::Duplicate);
    // a user-action signature can never be a checkpoint signature: different domain separator
    assert_ne!(n.g.engine_domain.separator(), n.g.vault_domain.separator());
    let _ = CheckpointBody::from_json(&body.to_json()).unwrap();
}

#[test]
fn recovery_snapshot_log_torn_tail_and_corruption() {
    let mut n = net("recovery", 2);
    n.seq.snapshot_every = 3;
    let u = user(13);
    let ua = key_address(&u);
    for i in 0..4u64 {
        n.deposit_everywhere(deposit(i, ua, 1_000_000 + i));
        assert!(all_applied(&n.step()));
    }
    let t = now();
    for i in 0..3u64 {
        n.seq.admit(&request(&n.g, &place_order(&n.g, &u, ua, t + i, 1, 10 + i, 1, false)), t).unwrap();
        assert!(all_applied(&n.step()));
    }
    let (h, root, lh, fin) = (n.seq.log.last_height, n.seq.engine.state_root(), n.seq.log.last_log_hash, n.seq.latest_final());
    assert_eq!(h, 7);
    let dir = n.dir.clone();
    let g = n.g.clone();
    let key = n.keys.sequencer.clone();
    drop(n);
    let s = Sequencer::open(g.clone(), key.clone(), fresh_engine(&g), dir.clone(), view()).unwrap();
    assert_eq!((s.log.last_height, s.engine.state_root(), s.log.last_log_hash, s.latest_final()), (h, root, lh, fin));
    assert_eq!(s.recovered.snapshot_height, Some(6));
    assert_eq!(s.recovered.replayed_blocks, 1);
    assert!(s.recovered.checkpoints_loaded >= 3);
    drop(s);
    // a torn tail (half a record) is cut off; the chain before it is intact
    let logp = dir.join("blocks.log");
    let full = std::fs::read(&logp).unwrap();
    let mut torn = full.clone();
    torn.extend_from_slice(b"PPLB\x00\x00\x10\x00partial");
    std::fs::write(&logp, &torn).unwrap();
    let s = Sequencer::open(g.clone(), key.clone(), fresh_engine(&g), dir.clone(), view()).unwrap();
    assert_eq!(s.recovered.torn_tail_bytes, 15);
    assert_eq!(s.log.last_height, h);
    drop(s);
    assert_eq!(std::fs::read(&logp).unwrap(), full);
    // a flipped byte in the middle is corruption: refuse to start
    let mut bad = full.clone();
    let mid = bad.len() / 2;
    bad[mid] ^= 0x01;
    std::fs::write(&logp, &bad).unwrap();
    assert!(Sequencer::open(g.clone(), key.clone(), fresh_engine(&g), dir.clone(), view()).is_err());
    // the wrong sequencer key refuses to start
    std::fs::write(&logp, &full).unwrap();
    assert!(Sequencer::open(g.clone(), user(14), fresh_engine(&g), dir.clone(), view()).is_err());
    // a snapshot whose content was altered is skipped (checksum), recovery replays from genesis instead
    for (_, p) in perps_node::log::list_snapshots(&dir) {
        let mut b = std::fs::read(&p).unwrap();
        let k = b.len() - 40;
        b[k] ^= 0xff;
        std::fs::write(&p, &b).unwrap();
    }
    let s = Sequencer::open(g.clone(), key, fresh_engine(&g), dir, view()).unwrap();
    assert_eq!(s.recovered.snapshot_height, None);
    assert_eq!(s.recovered.replayed_blocks, h);
    assert_eq!(s.engine.state_root(), root);
}

#[test]
fn canonical_encoding_is_unique_and_roundtrips() {
    let (g, _k) = perps_node::dev::dev_genesis("codec", 7, 5, 10, 0);
    let u = user(15);
    let ua = key_address(&u);
    let mut d0 = deposit(0, ua, 7);
    (d0.sender, d0.head, d0.l2_block, d0.l2_block_hash, d0.l2_tx_hash) = ([0; 20], [0; 32], 0, [0; 32], [0; 32]);
    let inputs = vec![
        Input::Deposit(d0),
        Input::Forced(ForcedEvent { index: 0, account: ua, kind: ForcedKind::Close, deadline: 9, arg: 2, head: [0; 32], l2_block: 0, l2_block_hash: [0; 32], l2_tx_hash: [0; 32], log_index: 0 }),
        Input::Action(place_order(&g, &u, ua, 5, 2, 7, 8, true)),
    ];
    let enc = encode_inputs(&g.types, &inputs).unwrap();
    assert_eq!(decode_inputs(&g.types, &enc).unwrap(), inputs);
    let mut trailing = enc.clone();
    trailing.push(0);
    assert!(decode_inputs(&g.types, &trailing).is_err());
    let mut wrong_order = inputs.clone();
    wrong_order.swap(0, 2);
    assert!(encode_inputs(&g.types, &wrong_order).is_err());
    // count field larger than the content
    let mut c = enc.clone();
    c[3] += 1;
    assert!(decode_inputs(&g.types, &c).is_err());
}

#[test]
fn snapshot_restore_same_root_and_non_canonical_refused() {
    let mut n = net("snap", 5);
    let u = user(16);
    let ua = key_address(&u);
    n.deposit_everywhere(deposit(0, ua, 77));
    n.step();
    let snap = n.seq.engine.snapshot();
    let mut e = fresh_engine(&n.g);
    e.restore(&snap).unwrap();
    assert_eq!(e.state_root(), n.seq.engine.state_root());
    let mut bad = snap.clone();
    bad.push(0);
    assert!(fresh_engine(&n.g).restore(&bad).is_err());
}

/// The same end-to-end path over agent 388's real engine (SPEC 0.1.1): the node's bytes are decoded by the
/// engine's own decoder, every validator's engine reaches the sequencer's root, the checkpoint certifies a real
/// engine withdrawal record, and recovery (no engine snapshots: replay from genesis) reproduces the root.
#[test]
fn real_engine_deposit_order_withdraw_certificate_and_recovery() {
    let mut n = net_real("real", 4);
    let u = user(40);
    let ua = key_address(&u);
    n.deposit_everywhere(deposit(0, ua, 5_000_000_000));
    assert!(all_applied(&n.step()));
    let t = now();
    // an order (the market has no oracle price yet: the engine's outcome is whatever SPEC says; all agree)
    let po = place_order(&n.g, &u, ua, t, 1, 6_000_000_000_000, 100_000, false);
    n.seq.admit(&request(&n.g, &po), t).unwrap();
    let wd = withdraw(&n.g, &u, ua, t + 1, 1_000_000_000, ua);
    let ack = n.seq.admit(&request(&n.g, &wd), t).unwrap();
    let rs = n.step();
    assert!(rs.iter().all(|r| matches!(r, VResult::Applied { checkpoint: Some(_) })), "{rs:?}");
    let res = &n.seq.acks[&ack.action_hash];
    assert_eq!(res.status, "included");
    assert_eq!(res.result.as_ref().unwrap()["ok"], true, "{:?}", res.result);
    let rec = n.seq.engine.withdrawal(0).expect("engine withdrawal record");
    assert_eq!(rec.amount, 999_000_000);
    let cert = n.seq.certificate(0).expect("certificate");
    let body = verify_checkpoint_json(&n.g, &cert["checkpoint"]).unwrap();
    let sib: Vec<[u8; 32]> = cert["proof"]["siblings"].as_array().unwrap().iter().map(|p| parse_hex_fixed::<32>(p.as_str().unwrap()).unwrap()).collect();
    assert_eq!(sib.len(), 32, "SPEC 11: exactly 32 siblings");
    assert!(postern_perps_engine::merkle::verify(&rec.leaf(), 0, &sib, &body.withdrawals_root));
    assert!(!postern_perps_engine::merkle::verify(&rec.leaf(), 1, &sib, &body.withdrawals_root), "wrong position refused");
    assert_eq!(body.state_root(), n.seq.block(body.height).unwrap().unwrap().header.state_root, "components reproduce the replayed root");
    // engine definition (SPEC 11 systemHash): every account, the protocol-fee account (1 USDC fee) included
    assert_eq!(body.total_escape, 4_001_000_000, "user 4000 USDC + fee account 1 USDC");
    for v in &n.vals {
        assert_eq!(v.engine.state_root(), n.seq.engine.state_root());
    }
    let (root, h) = (n.seq.engine.state_root(), n.seq.log.last_height);
    let (g, key, dir) = (n.g.clone(), n.keys.sequencer.clone(), n.dir.clone());
    drop(n);
    let (_, _, checked) = perps_node::dev::dev_genesis_real("test-real", 7, 5, 4, 0);
    let s = Sequencer::open(g.clone(), key, Box::new(perps_node::engine::real::RealEngine::new(g.clone(), checked).unwrap()), dir, view()).unwrap();
    assert_eq!((s.engine.state_root(), s.log.last_height, s.recovered.replayed_blocks), (root, h, h));
    // a stub engine cannot open the real engine's log (different logHash(0))
}

/// R395-29 (395, Medium): a block timed far behind the validators' clocks must not be certified. Without the
/// lower bound (node c9bb54b..b170e74) every validator returned `Applied { checkpoint: Some(_) }` here (395's
/// demonstration `r395_29_lagging_block_time_is_signed` passed on c9bb54b).
#[test]
fn r395_29_block_timed_behind_the_validators_clock_is_not_signed() {
    let mut n = net("r395-29", 1);
    let ua = key_address(&user(50));
    n.deposit_everywhere(deposit(0, ua, 1_000_000));
    // the sequencer runs engine time 60 s late
    let info = n.seq.produce(now() - 60_000).unwrap().unwrap();
    let b = info.block.as_ref().clone();
    for v in n.vals.iter_mut() {
        match v.process(&b, now()) {
            VResult::AppliedNotSigned(r) => assert!(r.contains("R395-29"), "{r}"),
            r => panic!("a late block was certified: {r:?}"),
        }
    }
    assert_eq!(n.seq.latest_final(), None);
    // the next timely block's checkpoint covers only timely blocks again: signed
    n.deposit_everywhere(deposit(1, ua, 1_000_000));
    let rs = n.step();
    assert!(rs.iter().all(|r| matches!(r, VResult::Applied { checkpoint: Some(_) })), "{rs:?}");
    // a late block inside an interval taints that interval's checkpoint even if the checkpoint block is timely
    let mut m = net("r395-29b", 3);
    m.deposit_everywhere(deposit(0, ua, 1));
    let late = m.seq.produce(now() - 10_000).unwrap().unwrap().block.as_ref().clone();
    for v in m.vals.iter_mut() {
        assert!(matches!(v.process(&late, now()), VResult::Applied { checkpoint: None }));
    }
    m.deposit_everywhere(deposit(1, ua, 1));
    assert!(m.step().iter().all(|r| matches!(r, VResult::Applied { checkpoint: None })));
    m.deposit_everywhere(deposit(2, ua, 1));
    assert!(m.step().iter().all(|r| matches!(r, VResult::AppliedNotSigned(s) if s.contains("block 1"))));
}

/// R395-30: the pre-SPEC user-action domain (chain 7007701, no verifyingContract) is refused at genesis.
#[test]
fn r395_30_old_7007701_domain_refused() {
    let mut td: serde_json::Value = serde_json::from_slice(&perps_node::dev::fixture("eip712.json")).unwrap();
    td["domain"]["chainId"] = 7007701.into();
    let r = perps_node::genesis::Genesis::new(
        td.to_string().as_bytes(),
        &perps_node::dev::fixture("markets.json"),
        vec![[1u8; 20]],
        1,
        vec![[1u8; 20]],
        1,
        [2u8; 20],
        [3u8; 20],
        [4u8; 20],
        0,
        1,
    );
    assert!(r.unwrap_err().contains("700770"));
}

/// R395-30: forced deadlines are L2 SECONDS (VAULT 0.2) and are compared with L2 time in seconds, never with the
/// engine's millisecond block time (with ms vs s every pending request looked "past its deadline" at once).
#[test]
fn r395_30_forced_deadline_is_judged_in_l2_seconds() {
    let mut n = net("r395-30", 1);
    let ua = key_address(&user(51));
    n.deposit_everywhere(deposit(0, ua, 10_000_000));
    assert!(all_applied(&n.step()));
    let now_s = now() / 1000;
    let f = ForcedEvent { index: 0, account: ua, kind: ForcedKind::CancelAll, deadline: now_s + 3600, arg: 0, head: keccak(b"fh"), l2_block: 9, l2_block_hash: keccak(b"b9"), l2_tx_hash: keccak(b"t9"), log_index: 0 };
    for v in &n.val_views {
        let mut x = v.lock().unwrap();
        x.forced.insert(0, f.clone());
        x.l2_time_s = now_s;
    }
    // pending, deadline an hour ahead in L2 seconds: validators keep signing (block time in ms is ~1000x larger)
    n.deposit_everywhere(deposit(1, ua, 1));
    assert!(n.step().iter().all(|r| matches!(r, VResult::Applied { checkpoint: Some(_) })));
    // L2 time reaches the deadline minus the 120 s margin: not signing until the sequencer includes it
    for v in &n.val_views {
        v.lock().unwrap().l2_time_s = now_s + 3600 - 120;
    }
    n.deposit_everywhere(deposit(2, ua, 1));
    assert!(n.step().iter().all(|r| matches!(r, VResult::AppliedNotSigned(s) if s.contains("deadline"))));
}

//! Fixes of agent 436's round-2 findings on the node, each written failing-first against `b897344` (the property
//! the fix must give; 436's own demonstrations in `review/node/r436_node*.rs` pass when the DEFECT is present).
//! THROWAWAY keys only. UNAUDITED PROTOTYPE.

mod common;

use common::*;
use perps_node::sig::key_address;
use perps_node::util::to_hex;
use perps_node::validator::VResult;

fn poison(g: &perps_node::genesis::Genesis, k: &k256::ecdsa::SigningKey, nonce: u64) -> perps_node::types::SignedAction {
    signed(
        g,
        k,
        "ApproveAgent",
        serde_json::json!({"account": to_hex(&key_address(k)), "nonce": nonce.to_string(), "agent": to_hex(&[9u8; 20]),
            "name": "x".repeat(33), "expiresAt": (nonce + 3_600_000).to_string()}),
    )
}

/// An engine whose decoder refuses a WHOLE block for one input, as engine 0.1.6 did for an agent name above 32
/// bytes (R436-4). Since engine 0.2.3 the real decoder has no such per-input rule (the action reaches
/// `apply_action` and is MALFORMED there: test `r436_4_with_the_fixed_engine_...` below), so the node's two lines
/// of defence are tested against this stand-in: the real engine underneath, plus the old refusal.
struct Refusing(Box<dyn perps_node::engine::Engine>, std::sync::Arc<perps_node::genesis::Genesis>);

impl Refusing {
    fn refuses(&self, i: &perps_node::types::Input) -> bool {
        let perps_node::types::Input::Action(a) = i else { return false };
        if self.1.types.name(a.type_index) != Some("ApproveAgent") {
            return false;
        }
        matches!(perps_node::actions::field(&self.1, "ApproveAgent", &a.message, "name"), Some(perps_node::eip712::Val::Str(s)) if s.len() > 32)
    }
}

impl perps_node::engine::Engine for Refusing {
    fn apply_block(&mut self, height: u64, time: u64, inputs: &[perps_node::types::Input]) -> Result<perps_node::engine::BlockOutcome, String> {
        if inputs.iter().any(|i| self.refuses(i)) {
            return Err("engine decoder refused the node's encoding: agent name > 32 bytes".into());
        }
        self.0.apply_block(height, time, inputs)
    }
    fn precheck_input(&self, input: &perps_node::types::Input) -> Result<(), String> {
        if self.refuses(input) {
            return Err("agent name > 32 bytes".into());
        }
        self.0.precheck_input(input)
    }
    fn clone_box(&self) -> Box<dyn perps_node::engine::Engine> {
        Box::new(Refusing(self.0.clone_box(), self.1.clone()))
    }
    fn state_root(&self) -> perps_node::util::H256 {
        self.0.state_root()
    }
    fn height(&self) -> u64 {
        self.0.height()
    }
    fn time(&self) -> u64 {
        self.0.time()
    }
    fn snapshot(&self) -> Vec<u8> {
        self.0.snapshot()
    }
    fn restore(&mut self, bytes: &[u8]) -> Result<(), String> {
        self.0.restore(bytes)
    }
    fn next_deposit_index(&self) -> u64 {
        self.0.next_deposit_index()
    }
    fn next_forced_index(&self) -> u64 {
        self.0.next_forced_index()
    }
    fn forced_processed(&self) -> u64 {
        self.0.forced_processed()
    }
    fn last_oracle_round(&self) -> u64 {
        self.0.last_oracle_round()
    }
    fn authority(&self, account: &perps_node::util::Address, signer: &perps_node::util::Address, primary: &str, time: u64) -> Result<(), perps_node::engine::AuthorityError> {
        self.0.authority(account, signer, primary, time)
    }
    fn highest_nonce(&self, signer: &perps_node::util::Address) -> Option<u64> {
        self.0.highest_nonce(signer)
    }
    fn genesis_log_hash(&self) -> perps_node::util::H256 {
        self.0.genesis_log_hash()
    }
    fn withdrawal_count(&self) -> u64 {
        self.0.withdrawal_count()
    }
    fn withdrawal(&self, id: u64) -> Option<perps_node::engine::WithdrawalRecord> {
        self.0.withdrawal(id)
    }
    fn state_parts(&self) -> perps_node::engine::StateParts {
        self.0.state_parts()
    }
    fn withdrawal_proof(&self, id: u64, count: u64) -> Option<Vec<perps_node::util::H256>> {
        self.0.withdrawal_proof(id, count)
    }
    fn nonce_ok(&self, signer: &perps_node::util::Address, nonce: u64, time: u64) -> Result<(), &'static str> {
        self.0.nonce_ok(signer, nonce, time)
    }
    fn account_json(&self, a: &perps_node::util::Address) -> serde_json::Value {
        self.0.account_json(a)
    }
    fn orders_json(&self, a: &perps_node::util::Address) -> serde_json::Value {
        self.0.orders_json(a)
    }
    fn agents_json(&self, a: &perps_node::util::Address) -> serde_json::Value {
        self.0.agents_json(a)
    }
    fn name(&self) -> &'static str {
        self.0.name()
    }
    fn real(&self) -> Option<&postern_perps_engine::Engine> {
        self.0.real()
    }
    fn block_limits(&self) -> (usize, usize) {
        self.0.block_limits()
    }
}

/// The network of `net_real`, every engine wrapped in `Refusing`.
fn net_refusing(name: &str, ci: u64) -> Net {
    let (g, keys, checked) = perps_node::dev::dev_genesis_real(&format!("test-{name}"), 7, 5, ci, 0);
    net_with(name, g, keys, &move |g| Box::new(Refusing(Box::new(perps_node::engine::real::RealEngine::new(g.clone(), checked.clone()).unwrap()), g.clone())))
}

/// R436-4 (High): one signed `ApproveAgent` with a 33-byte name was admitted, drained into a block the engine's
/// decoder refuses as a whole, and the sequencer halted for good (and again after every restart). Property:
/// admission asks the ENGINE (its decoder) about every action, so an action the engine would refuse a block for
/// never enters the mempool, the honest actions around it are included, and the sequencer keeps producing.
/// RESTATED after the engine's own fix (0.2.3) removed the refusal from the real decoder: the same assertions now
/// run against `Refusing` (the real engine with 0.1.6's refusal), because the real engine no longer offers a
/// refusable action; what the real engine does with the action is the third test below.
#[test]
fn r436_4_an_action_the_engine_decoder_refuses_is_refused_at_admission() {
    let mut n = net_refusing("r436-4", 10);
    let g = n.g.clone();
    let (k, honest) = (user(7), user(8));
    let ha = key_address(&honest);
    let t = now();
    n.deposit_everywhere(deposit(0, ha, 1_000_000_000));
    n.seq.produce(t).unwrap().unwrap();
    let good = n.seq.admit_signed(withdraw(&g, &honest, ha, t + 1, 2_000_000, ha), t).expect("an honest action is admitted");
    let e = n.seq.admit(&request(&g, &poison(&g, &k, t + 2)), t).expect_err("the engine's decoder refuses this action: not admitted");
    assert_eq!((e.http, e.code), (400, "MALFORMED"), "{e:?}");
    assert!(e.message.contains("engine"), "the refusal names the engine's decoder: {}", e.message);
    assert_eq!(n.seq.mempool_len(), 1, "only the honest action is queued");
    // a 32-byte name is the engine's limit: admitted (and judged by the engine: this key has no account)
    let mut ok = poison(&g, &k, t + 3);
    ok = signed(&g, &k, "ApproveAgent", {
        let mut j = g.types.registry.val_to_json("ApproveAgent", &ok.message);
        j["name"] = "y".repeat(32).into();
        j
    });
    n.seq.admit_signed(ok, t).expect("32 bytes is inside the engine's bound");
    let info = n.seq.produce(t + 5).expect("the sequencer produces").expect("a block");
    assert!(n.seq.halted.is_none());
    assert_eq!(n.seq.acks[&good.action_hash].status, "included");
    assert_eq!(n.seq.acks[&good.action_hash].result.as_ref().unwrap()["ok"], true);
    assert_eq!(info.outcome.outcomes.len(), 2);
    assert!(n.seq.admit_signed(withdraw(&g, &honest, ha, t + 9, 2_000_000, ha), t + 9).is_ok(), "admission stays open");
}

/// R436-4, second line of defence: if an input the engine's decoder refuses reaches the mempool anyway (a future
/// admission gap), `produce` drops THAT action (acknowledged as rejected, with the reason), includes the rest and
/// does not halt. A sequencer that halted on its own block here lost every action drained with it. (Restated on
/// `Refusing` for the same reason as the test above.)
#[cfg(feature = "dev-oracle")]
#[test]
fn r436_4_produce_drops_an_input_the_engine_refuses_instead_of_halting() {
    let mut n = net_refusing("r436-4b", 10);
    let g = n.g.clone();
    let (k, honest) = (user(7), user(8));
    let ha = key_address(&honest);
    let t = now();
    n.deposit_everywhere(deposit(0, ha, 1_000_000_000));
    n.step();
    let good1 = n.seq.admit_signed(withdraw(&g, &honest, ha, t + 1, 2_000_000, ha), t).unwrap();
    let bad = n.seq.dev_inject_unchecked(poison(&g, &k, t + 2), t);
    let good2 = n.seq.admit_signed(withdraw(&g, &honest, ha, t + 3, 3_000_000, ha), t).unwrap();
    assert_eq!(n.seq.mempool_len(), 3);
    let rs = n.step();
    assert!(n.seq.halted.is_none(), "{:?}", n.seq.halted);
    assert!(rs.iter().all(|r| matches!(r, VResult::Applied { checkpoint: Some(_) })), "every validator applies and signs the block without the refused action: {rs:?}");
    for good in [&good1, &good2] {
        let a = &n.seq.acks[&good.action_hash];
        assert_eq!((a.status, a.result.as_ref().unwrap()["ok"].as_bool()), ("included", Some(true)));
    }
    let b = &n.seq.acks[&bad];
    assert_eq!(b.status, "rejected");
    assert!(b.result.as_ref().unwrap()["error"].as_str().unwrap().starts_with("MALFORMED"), "{:?}", b.result);
    assert_eq!(n.seq.engine.withdrawal_count(), 2);
    assert_eq!(n.seq.mempool_len(), 0);
}

/// R436-4 end to end with the engine's own fix (0.2.3: no per-input rule in the block decoder): the 33-byte agent
/// name is admitted, included, and refused by the engine for THAT action only (MALFORMED); the honest action in
/// the same block is applied, every validator signs, nothing halts.
#[test]
fn r436_4_with_the_fixed_engine_the_action_is_malformed_alone() {
    let mut n = net_real("r436-4c", 10);
    let g = n.g.clone();
    let (k, honest) = (user(7), user(8));
    let (ka, ha) = (key_address(&k), key_address(&honest));
    n.deposit_everywhere(deposit(0, ha, 1_000_000_000));
    n.deposit_everywhere(deposit(1, ka, 1_000_000_000));
    n.step();
    let t = now();
    let good = n.seq.admit_signed(withdraw(&g, &honest, ha, t + 1, 2_000_000, ha), t).unwrap();
    let bad = n.seq.admit(&request(&g, &poison(&g, &k, t + 2)), t).expect("the real engine's decoder takes it");
    let rs = n.step();
    assert!(n.seq.halted.is_none());
    assert!(rs.iter().all(|r| matches!(r, VResult::Applied { checkpoint: Some(_) })), "{rs:?}");
    assert_eq!(n.seq.acks[&good.action_hash].result.as_ref().unwrap()["ok"], true);
    let b = n.seq.acks[&bad.action_hash].result.clone().unwrap();
    assert_eq!((b["ok"].as_bool(), b["error"].as_str()), (Some(false), Some("MALFORMED")), "{b}");
    assert!(n.seq.agents_view(&ka)["agents"].as_array().unwrap().is_empty(), "no agent was approved");
}

/// R436-3 (Medium): `produce` took EVERY pending finalized deposit and forced request with no cap; above the
/// engine's MAX_INPUTS the engine refused the block and the sequencer halted for good (the same block was rebuilt
/// after a restart). Property: a block takes a bounded prefix of the pending L2 inputs, the rest follow in the
/// next blocks, nothing halts, and every deposit is credited in vault index order.
#[test]
fn r436_3_a_batch_of_finalized_deposits_is_spread_over_blocks() {
    let mut n = net_real("r436-3", 10);
    let total = 10_001u64;
    {
        let mut v = n.seq_view.lock().unwrap();
        for i in 0..total {
            v.deposits.insert(i, deposit(i, key_address(&user(1_000 + (i % 50))), 10_000_000));
        }
    }
    let (max_inputs, _) = n.seq.engine.block_limits();
    let mut blocks = 0;
    let mut t = now();
    while n.seq.engine.next_deposit_index() < total {
        t += 1;
        let info = n.seq.produce(t).expect("never refused by the engine").expect("a block while deposits are pending");
        assert!(info.outcome.outcomes.len() <= max_inputs);
        assert!(info.outcome.outcomes.iter().all(|o| o.ok));
        blocks += 1;
        assert!(blocks <= 20, "bounded number of blocks");
    }
    assert!(n.seq.halted.is_none());
    assert!(blocks >= 2, "10,001 deposits do not fit one block");
    assert_eq!(n.seq.engine.next_deposit_index(), total);
    assert!(n.seq.produce(t + 1).unwrap().is_none(), "nothing left to include");
}

fn observation(g: &perps_node::genesis::Genesis, key: &k256::ecdsa::SigningKey, signer: u8, time_ms: u64, price: u64) -> perps_node::types::SignedObservation {
    let mut o = perps_node::types::SignedObservation { signer, time_ms, prices: vec![perps_node::types::OraclePrice { market_id: 1, price, sources: 5 }], signature: [0u8; 65] };
    o.signature = perps_node::sig::sign(key, &perps_node::oracle_pool::observation_digest(g, time_ms / 3000, &o));
    o
}

/// R436-12 (Medium): the engine refuses a WHOLE oracle update when one observation is outside its time bound
/// (`timeMs > T + maxFutureMs`), and the no-selection pool put every held observation into the block: ONE signer
/// of seven vetoed every round by signing at the end of the round. Property: the pool hands the sequencer only
/// observations the engine accepts at the block's time; an observation still in the engine's future DEFERS the
/// round (at most until the round's last 1,000 ms) instead of poisoning it, so every round is accepted with all
/// seven observations, and one late signer costs at most that delay.
#[test]
fn r436_12_one_late_signer_delays_a_round_but_cannot_veto_it() {
    const P: u64 = 6_725_000_000_000;
    let (g, keys, checked) = perps_node::dev::dev_genesis_real_with("r436-12", 7, 5, 10, 0, &perps_node::dev::DevOptions::default()).unwrap();
    let mut seq = perps_node::sequencer::Sequencer::open(g.clone(), keys.sequencer.clone(), Box::new(perps_node::engine::real::RealEngine::new(g.clone(), checked).unwrap()), tmpdir("r436-12"), view()).unwrap();
    let mut t = (now() / 3_600_000) * 3_600_000 + 600_000; // a round boundary
    for _ in 0..5 {
        t += 3000;
        let round = t / 3000;
        let last = seq.engine.last_oracle_round();
        for (i, k) in keys.oracle.iter().enumerate() {
            // signer 6 signs the last millisecond of the round; the others 100 ms into it
            let tm = if i == 6 { round * 3000 + 2999 } else { t + 100 };
            seq.oracle_pool.offer(&g, round, observation(&g, k, i as u8, tm, P), last, t + 100).unwrap();
        }
        // 300 ms into the round signer 6's observation is 2,699 ms in the engine's future (bound 1,000): no block
        assert!(seq.produce(t + 300).unwrap().is_none(), "the round waits; it is not built with an observation the engine refuses");
        assert!(seq.halted.is_none());
        // from 1,999 ms into the round the engine accepts all seven
        assert!(seq.produce(t + 1_998).unwrap().is_none());
        let info = seq.produce(t + 1_999).unwrap().expect("the round is built once every held observation is acceptable");
        let o = &info.outcome.outcomes[0];
        assert!(o.ok, "{:?}", o.error);
        assert_eq!(seq.engine.last_oracle_round(), round);
        assert_eq!(seq.oracle_pool.last_built.as_ref().unwrap().1, vec![0, 1, 2, 3, 4, 5, 6], "all seven, no selection");
    }
    // three agreeing rounds made the market active: the late signer did not stop the index
    assert_eq!(seq.engine.real().unwrap().markets[&1].oracle.index, P);
}

/// R436-12, the other side: when waiting for an observation in the future would let the valid ones expire, the
/// round is built from the valid ones (the engine would refuse the other at any time that keeps them), and an
/// observation the engine already refuses as too old is dropped. Unit-level with artificial bounds, because the
/// compliance bounds (future 1 s, age 6 s, round 3 s) never produce the conflict.
#[test]
fn r436_12_pool_time_rules_with_artificial_bounds() {
    const P: u64 = 6_725_000_000_000;
    let (g, keys, _) = perps_node::dev::dev_genesis_real_with("r436-12b", 7, 5, 10, 0, &perps_node::dev::DevOptions::default()).unwrap();
    let t0 = (now() / 3000) * 3000;
    let round = t0 / 3000;
    let offer_all = |pool: &mut perps_node::oracle_pool::OraclePool, times: [u64; 7]| {
        for (i, k) in keys.oracle.iter().enumerate() {
            pool.offer(&g, round, observation(&g, k, i as u8, t0 + times[i], P), 0, t0 + 10).unwrap();
        }
    };
    // (max future 1,000, max age 1,500): six at +0, signer 6 at +2,999 (valid only from T = +1,999, when the
    // others, valid until +1,500, are gone). At T = +900: waiting cannot work -> built now from the six.
    let mut pool = perps_node::oracle_pool::OraclePool::default();
    offer_all(&mut pool, [0, 0, 0, 0, 0, 0, 2_999]);
    let u = pool.take_ready_at(&g, 0, t0 + 900, t0 + 900, 1_000, 1_500).expect("built from the valid six");
    assert_eq!(u.observations.iter().map(|o| o.signer).collect::<Vec<_>>(), vec![0, 1, 2, 3, 4, 5]);
    assert_eq!(pool.left_out, 1);
    // one observation too old at T (age bound 1,500): dropped, the round is built from the other six
    let mut pool = perps_node::oracle_pool::OraclePool::default();
    offer_all(&mut pool, [0, 1_000, 1_000, 1_000, 1_000, 1_000, 1_000]);
    let u = pool.take_ready_at(&g, 0, t0 + 1_600, t0 + 1_600, 1_000, 1_500).expect("six are still inside the age bound");
    assert_eq!(u.observations.iter().map(|o| o.signer).collect::<Vec<_>>(), vec![1, 2, 3, 4, 5, 6]);
    assert_eq!(pool.left_out, 1);
    // fewer than the quorum acceptable at T: nothing is built (the engine would refuse it: fail closed, STALE)
    let mut pool = perps_node::oracle_pool::OraclePool::default();
    offer_all(&mut pool, [0, 0, 0, 1_000, 1_000, 1_000, 1_000]);
    assert!(pool.take_ready_at(&g, 0, t0 + 1_600, t0 + 1_600, 1_000, 1_500).is_none(), "only four acceptable");
}

/// R436-6 (Medium, node part): the withdrawal certificate and an account's withdrawal list no longer rebuild the
/// whole withdrawal tree per record; the incremental tree gives the same siblings as the engine's own proof for
/// every record and every accepted count, and the per-account list is exact.
#[test]
fn r436_6_withdrawal_proofs_from_the_incremental_tree_equal_the_engines() {
    let mut n = net_real("r436-6", 1);
    let users: Vec<_> = (0..3).map(|i| user(200 + i)).collect();
    for (i, u) in users.iter().enumerate() {
        n.deposit_everywhere(deposit(i as u64, key_address(u), 5_000_000_000));
    }
    n.step();
    // seven withdrawals over four blocks, interleaved between the accounts
    let mut nonce = now();
    for round in 0..4 {
        for (i, u) in users.iter().enumerate() {
            if (round + i) % 2 == 0 {
                nonce += 1;
                let a = key_address(u);
                n.seq.admit_signed(withdraw(&n.g, u, a, nonce, 10_000_000 + (round as u64) * 1_000_000, a), now()).unwrap();
            }
        }
        n.step();
    }
    let total = n.seq.engine.withdrawal_count();
    assert!(total >= 6, "{total}");
    assert_eq!(n.seq.wd_tree.len(), total);
    for count in 1..=total {
        for id in 0..count {
            let ours = n.seq.wd_tree.proof(id, count).unwrap();
            assert_eq!(Some(ours.clone()), n.seq.engine.withdrawal_proof(id, count), "id {id} count {count}");
            let p = n.seq.withdrawal_proof(id, Some(count), None).unwrap();
            assert_eq!(p["siblings"].as_array().unwrap().len(), 32);
        }
    }
    // the certificate of every record verifies against the newest final checkpoint
    let h = n.seq.latest_final().unwrap();
    let root = n.seq.checkpoint_for(h).unwrap().withdrawals_root;
    for id in 0..total {
        let cert = n.seq.certificate(id).expect("certified");
        let sib: Vec<[u8; 32]> = cert["proof"]["siblings"].as_array().unwrap().iter().map(|p| perps_node::util::parse_hex_fixed::<32>(p.as_str().unwrap()).unwrap()).collect();
        assert!(postern_perps_engine::merkle::verify(&n.seq.engine.withdrawal(id).unwrap().leaf(), id, &sib, &root));
    }
    // the per-account list is exactly the engine's records of that account, in id order
    for u in &users {
        let a = key_address(u);
        let expect: Vec<u64> = (0..total).filter(|id| n.seq.engine.withdrawal(*id).unwrap().account == a).collect();
        assert_eq!(n.seq.wd_by_account.get(&a).cloned().unwrap_or_default(), expect);
    }
    // and survives a restart (rebuilt from the replayed engine)
    let (g, key, dir) = (n.g.clone(), n.keys.sequencer.clone(), n.dir.clone());
    let lens: Vec<usize> = users.iter().map(|u| n.seq.wd_by_account[&key_address(u)].len()).collect();
    drop(n);
    let (_, _, checked) = perps_node::dev::dev_genesis_real("test-r436-6", 7, 5, 1, 0);
    let s = perps_node::sequencer::Sequencer::open(g.clone(), key, Box::new(perps_node::engine::real::RealEngine::new(g.clone(), checked).unwrap()), dir, view()).unwrap();
    assert_eq!(s.wd_tree.len(), total);
    assert_eq!(users.iter().map(|u| s.wd_by_account[&key_address(u)].len()).collect::<Vec<_>>(), lens);
    assert!(s.certificate(0).is_some(), "final checkpoints reloaded: certificates are served again");
}

/// Round-2 measurement turned into a rule: a block takes at most `block_actions` actions (default 600, never above
/// the hard cap); the rest wait in arrival order for the next blocks, and nothing is lost.
#[test]
fn a_block_takes_at_most_block_actions_and_the_rest_wait_in_order() {
    let mut n = net("block-actions", 1000);
    assert_eq!(n.seq.block_actions, perps_node::sequencer::DEFAULT_BLOCK_ACTIONS);
    n.seq.block_actions = 5;
    n.seq.rate = perps_node::sequencer::RateLimit::new(0.0, 0.0);
    let u = user(60);
    let ua = key_address(&u);
    n.deposit_everywhere(deposit(0, ua, 1_000_000_000));
    n.step();
    let t = now();
    let acks: Vec<_> = (0..13u64).map(|i| n.seq.admit(&request(&n.g, &place_order(&n.g, &u, ua, t + i, 1, 10 + i, 1, false)), t).unwrap()).collect();
    let mut sizes = Vec::new();
    while n.seq.mempool_len() > 0 {
        let info = n.seq.produce(now()).unwrap().unwrap();
        sizes.push(info.outcome.outcomes.len());
    }
    assert_eq!(sizes, vec![5, 5, 3]);
    let heights: Vec<u64> = acks.iter().map(|a| n.seq.acks[&a.action_hash].height.unwrap()).collect();
    assert!(heights.windows(2).all(|w| w[0] <= w[1]), "arrival order is kept across blocks: {heights:?}");
    assert!(acks.iter().all(|a| n.seq.acks[&a.action_hash].status == "included"));
    // a setting above the hard cap is clamped to it
    n.seq.block_actions = usize::MAX;
    assert!(n.seq.produce(now()).unwrap().is_none());
}

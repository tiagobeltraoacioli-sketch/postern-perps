//! Engine acceptance rule and halt machine: normal operation and adversarial cases (README sections 5 and 6).
//! Throwaway keys derived from fixed labels; nothing here is a real key.

use k256::ecdsa::SigningKey;
use perps_oracle::committee::*;
use perps_oracle::eip712;
use perps_oracle::encoding;
use perps_oracle::keccak256;
use perps_oracle::sig::{self, SigError};
use perps_oracle::types::*;
use perps_oracle::ROUND_MS;


fn key(i: u8) -> SigningKey {
    SigningKey::from_bytes(&keccak256(format!("postern-perps-oracle-test-signer-{i}").as_bytes()).into()).unwrap()
}

fn addr(i: u8) -> [u8; 20] {
    sig::address_of(key(i).verifying_key())
}

/// A throwaway vault address (the engine domain's verifyingContract of this test deployment).
const VAULT: [u8; 20] = [0xdb; 20];

fn market(id: u32, jump: u32) -> MarketParams {
    MarketParams {
        market_id: id,
        min_sources: 3,
        agree_bps: 100,
        jump_bps: jump,
        max_staleness_ms: 10_000,
        confirm_rounds: 3,
        exit_haircut_bps: jump,
        exit_haircut_per_min_bps: 100,
        exit_max_haircut_bps: 5000,
        max_halt_ms: 3_600_000,
    }
}

fn params() -> CommitteeParams {
    CommitteeParams {
        verifying_contract: VAULT,
        signers: (0..7).map(addr).collect(),
        quorum: 5,
        max_future_ms: 1000,
        max_age_ms: 6000,
        markets: vec![market(1, 500), market(2, 500), market(3, 800)],
    }
}

const R0: u64 = 598_296_000;

fn t_of(round: u64) -> u64 {
    round * ROUND_MS + 400
}

/// Signer `i` observes `prices` in `round` (sampling time 100 ms into the round).
fn obs(i: u8, round: u64, prices: &[(u32, u64, u8)]) -> SignedObservation {
    let prices: Vec<OraclePrice> = prices.iter().map(|&(m, p, s)| OraclePrice { market_id: m, price: p, sources: s }).collect();
    let time_ms = round * ROUND_MS + 100;
    let msg = Observation { round, time_ms, prices: prices.clone() };
    SignedObservation { signer: i, time_ms, prices, signature: sig::sign(&key(i), &eip712::digest(&eip712::domain_separator(&VAULT), &msg)) }
}

/// Every signer in `signers` observes BTC at `btc[k]`.
fn update(round: u64, signers: &[u8], btc: &[u64]) -> OracleUpdate {
    OracleUpdate {
        round,
        observations: signers.iter().zip(btc).map(|(&i, &p)| obs(i, round, &[(1, p, 5)])).collect(),
    }
}

fn honest(round: u64, p: u64) -> OracleUpdate {
    update(round, &[0, 1, 2, 3, 4, 5, 6], &[p; 7])
}

/// A state with BTC active at `p` (three confirming rounds from genesis), next free round returned.
fn active_at(p: u64) -> (CommitteeParams, OracleState, u64) {
    let prm = params();
    let mut st = OracleState::genesis(&prm).unwrap();
    for k in 0..3 {
        let r = R0 + k;
        begin_block(&prm, &mut st, t_of(r));
        apply_update(&prm, &mut st, &honest(r, p), t_of(r)).unwrap();
    }
    assert_eq!(st.safe_index(1), Some(p));
    (prm, st, R0 + 3)
}

fn assert_rejected_unchanged(prm: &CommitteeParams, st: &mut OracleState, u: &OracleUpdate, t: u64, want: OracleError) {
    let before = st.encode();
    assert_eq!(apply_update(prm, st, u, t), Err(want));
    assert_eq!(st.encode(), before, "a rejected input must not change the state");
}

#[test]
fn genesis_needs_confirmation_then_follows_bounded_steps() {
    let prm = params();
    let mut st = OracleState::genesis(&prm).unwrap();
    assert_eq!(st.safe_index(1), None);
    let out = apply_update(&prm, &mut st, &honest(R0, 648_700), t_of(R0)).unwrap();
    assert_eq!(out.outcomes[0], (1, MarketOutcome::Confirming { price: 648_700, count: 1 }));
    assert_eq!(out.outcomes[1], (2, MarketOutcome::NoQuorum { have: 0 }));
    apply_update(&prm, &mut st, &honest(R0 + 1, 648_710), t_of(R0 + 1)).unwrap();
    let out = apply_update(&prm, &mut st, &honest(R0 + 2, 648_720), t_of(R0 + 2)).unwrap();
    assert_eq!(out.outcomes[0], (1, MarketOutcome::Resumed { price: 648_720, confirmed: true }));
    assert_eq!(st.safe_index(1), Some(648_720));
    let out = apply_update(&prm, &mut st, &honest(R0 + 3, 650_000), t_of(R0 + 3)).unwrap();
    assert_eq!(out.outcomes[0], (1, MarketOutcome::Moved { price: 650_000 }));
    assert_eq!(st.market(1).unwrap().index_time_ms, (R0 + 3) * ROUND_MS);
}

#[test]
fn c1_replayed_or_old_round_is_rejected() {
    let (prm, mut st, r) = active_at(648_700);
    let u = honest(r - 1, 648_700);
    assert_rejected_unchanged(&prm, &mut st, &u, t_of(r), OracleError::RoundNotNew);
    let u = honest(r, 648_700);
    apply_update(&prm, &mut st, &u, t_of(r)).unwrap();
    assert_rejected_unchanged(&prm, &mut st, &u, t_of(r), OracleError::RoundNotNew);
}

#[test]
fn c2_structure() {
    let (prm, mut st, r) = active_at(648_700);
    let t = t_of(r);
    let m = |k| OracleError::Malformed(k);
    // Fewer than q observations.
    assert_rejected_unchanged(&prm, &mut st, &update(r, &[0, 1, 2, 3], &[648_700; 4]), t, m(Malformed::Count));
    // More than n (an 8th entry, even a duplicate).
    let mut u = honest(r, 648_700);
    u.observations.push(u.observations[6].clone());
    assert_rejected_unchanged(&prm, &mut st, &u, t, m(Malformed::Count));
    // Duplicate signer: one honest signer counted twice would let 4 signers make a quorum of 5.
    let mut u = update(r, &[0, 1, 2, 3, 4], &[648_700; 5]);
    u.observations[4] = u.observations[3].clone();
    assert_rejected_unchanged(&prm, &mut st, &u, t, m(Malformed::SignerOrder));
    // Descending signers.
    let mut u = update(r, &[0, 1, 2, 3, 4], &[648_700; 5]);
    u.observations.swap(0, 1);
    assert_rejected_unchanged(&prm, &mut st, &u, t, m(Malformed::SignerOrder));
    // Signer index outside the committee.
    let mut u = update(r, &[0, 1, 2, 3, 4], &[648_700; 5]);
    u.observations[4].signer = 7;
    assert_rejected_unchanged(&prm, &mut st, &u, t, m(Malformed::SignerOrder));
    // Empty price list.
    let mut u = honest(r, 648_700);
    u.observations[2] = SignedObservation { prices: vec![], ..u.observations[2].clone() };
    assert_rejected_unchanged(&prm, &mut st, &u, t, m(Malformed::PriceCount));
    // Duplicate market in one observation (a signer counted twice for one market).
    let mut u = honest(r, 648_700);
    u.observations[2] = obs(2, r, &[(1, 648_700, 5), (1, 648_700, 5)]);
    assert_rejected_unchanged(&prm, &mut st, &u, t, m(Malformed::PriceOrder));
    let mut u = honest(r, 648_700);
    u.observations[2] = obs(2, r, &[(2, 34_800, 5), (1, 648_700, 5)]);
    assert_rejected_unchanged(&prm, &mut st, &u, t, m(Malformed::PriceOrder));
    // Zero price, even correctly signed.
    let mut u = honest(r, 648_700);
    u.observations[2] = obs(2, r, &[(1, 0, 5)]);
    assert_rejected_unchanged(&prm, &mut st, &u, t, m(Malformed::ZeroPrice));
    // Round so large that round * 3000 overflows.
    let u = OracleUpdate { round: u64::MAX / 2, observations: update(r, &[0, 1, 2, 3, 4], &[1; 5]).observations };
    assert_rejected_unchanged(&prm, &mut st, &u, t, m(Malformed::RoundOverflow));
}

#[test]
fn c2b_signatures() {
    let (prm, mut st, r) = active_at(648_700);
    let t = t_of(r);
    // A price changed after signing (a sequencer editing an honest observation).
    let mut u = honest(r, 648_700);
    u.observations[3].prices[0].price = 700_000;
    assert_rejected_unchanged(&prm, &mut st, &u, t, OracleError::BadSignature { observation: 3, reason: None });
    // `sources` changed after signing.
    let mut u = honest(r, 648_700);
    u.observations[3].prices[0].sources = 9;
    assert_rejected_unchanged(&prm, &mut st, &u, t, OracleError::BadSignature { observation: 3, reason: None });
    // Signer 5's observation presented as signer 6's.
    let mut u = update(r, &[0, 1, 2, 3, 5], &[648_700; 5]);
    u.observations[4].signer = 6;
    assert_rejected_unchanged(&prm, &mut st, &u, t, OracleError::BadSignature { observation: 4, reason: None });
    // A key outside the committee.
    let mut u = honest(r, 648_700);
    let msg = u.observations[6].message(r);
    u.observations[6].signature = sig::sign(&key(99), &eip712::digest(&eip712::domain_separator(&VAULT), &msg));
    assert_rejected_unchanged(&prm, &mut st, &u, t, OracleError::BadSignature { observation: 6, reason: None });
    // High-s twin of a valid signature.
    let mut u = honest(r, 648_700);
    let s: [u8; 32] = u.observations[1].signature[32..64].try_into().unwrap();
    u.observations[1].signature[32..64].copy_from_slice(&sig::neg_scalar(&s));
    u.observations[1].signature[64] ^= 27 ^ 28;
    assert_rejected_unchanged(&prm, &mut st, &u, t, OracleError::BadSignature { observation: 1, reason: Some(SigError::HighS) });
    // v outside {27, 28}.
    let mut u = honest(r, 648_700);
    u.observations[1].signature[64] -= 27;
    assert_rejected_unchanged(&prm, &mut st, &u, t, OracleError::BadSignature { observation: 1, reason: Some(SigError::BadV) });
    // Signed in another domain (the vault's checkpoint domain name, the L2 chain id).
    for dom in [
        eip712::domain_separator_for("PosternPerpsVault", "1", 700_770, &VAULT),
        eip712::domain_separator_for("PosternPerps", "1", 7_007_701, &VAULT),
        eip712::domain_separator_for("PosternPerps", "2", 700_770, &VAULT),
        // The same engine domain of another deployment (a staging vault).
        eip712::domain_separator(&[0xdc; 20]),
    ] {
        let mut u = honest(r, 648_700);
        let msg = u.observations[0].message(r);
        u.observations[0].signature = sig::sign(&key(0), &eip712::digest(&dom, &msg));
        assert_rejected_unchanged(&prm, &mut st, &u, t, OracleError::BadSignature { observation: 0, reason: None });
    }
}

#[test]
fn c2c_time_binding_and_cross_round_replay() {
    let (prm, mut st, r) = active_at(648_700);
    // An observation of an older round re-used in a new round: its time is outside the round.
    let mut u = honest(r, 648_700);
    u.observations[2] = obs(2, r - 1, &[(1, 648_700, 5)]);
    assert_rejected_unchanged(&prm, &mut st, &u, t_of(r), OracleError::Time { observation: 2 });
    // ... and with its time moved into the round, its signature (which binds round and time) fails.
    let mut u = honest(r, 648_700);
    let mut old = obs(2, r - 1, &[(1, 648_700, 5)]);
    old.time_ms += ROUND_MS;
    u.observations[2] = old;
    assert_rejected_unchanged(&prm, &mut st, &u, t_of(r), OracleError::BadSignature { observation: 2, reason: None });
    // Too far in the future of the block time (block time before the round started minus maxFutureMs).
    let u = honest(r, 648_700);
    let early = r * ROUND_MS + 100 - 1001;
    assert_rejected_unchanged(&prm, &mut st, &u, early, OracleError::Time { observation: 0 });
    assert!(apply_update(&prm, &mut st.clone(), &u, early + 1).is_ok(), "exactly maxFutureMs ahead is accepted");
    // Too old at the block time.
    let late = r * ROUND_MS + 100 + 6001;
    assert_rejected_unchanged(&prm, &mut st, &u, late, OracleError::Time { observation: 0 });
    assert!(apply_update(&prm, &mut st.clone(), &u, late - 1).is_ok(), "exactly maxAgeMs old is accepted");
}

#[test]
fn c2_errors_have_a_fixed_order() {
    let (prm, mut st, r) = active_at(648_700);
    // Observation 1 has a bad time, observation 0 a bad signature: observation 0 is checked first.
    let mut u = honest(r, 648_700);
    u.observations[0].prices[0].price += 1;
    u.observations[1].time_ms = 0;
    assert_rejected_unchanged(&prm, &mut st, &u, t_of(r), OracleError::BadSignature { observation: 0, reason: None });
    // In one observation the time is checked before the signature.
    let mut u = honest(r, 648_700);
    u.observations[0].prices[0].price += 1;
    u.observations[0].time_ms = 0;
    assert_rejected_unchanged(&prm, &mut st, &u, t_of(r), OracleError::Time { observation: 0 });
    // Structure (C2) of a later observation is reported before any signature of an earlier one.
    let mut u = honest(r, 648_700);
    u.observations[0].prices[0].price += 1;
    u.observations[5].prices[0].price = 0;
    assert_rejected_unchanged(&prm, &mut st, &u, t_of(r), OracleError::Malformed(Malformed::ZeroPrice));
    // C1 before everything.
    let mut u = honest(r - 1, 648_700);
    u.observations.truncate(1);
    assert_rejected_unchanged(&prm, &mut st, &u, t_of(r), OracleError::RoundNotNew);
}

/// Every subset of size >= q that a sequencer could include, with 2 lying signers at any extreme value: the
/// committee price stays inside the range of the honest values included, or the round changes nothing.
#[test]
fn c3_median_resists_two_liars_and_any_sequencer_subset() {
    let honest_vals = [648_500u64, 648_600, 648_700, 648_800, 648_900];
    let (prm, base, r) = active_at(648_700);
    for liar_vals in [(1u64, 1u64), (u64::MAX / 4, u64::MAX / 4), (1, u64::MAX / 4), (648_000, 649_400), (648_550, 648_550)] {
        // Signers 0..5 honest, 5 and 6 lie. Each signs once; the sequencer picks any subset of the signed set.
        let vals = [honest_vals[0], honest_vals[1], honest_vals[2], honest_vals[3], honest_vals[4], liar_vals.0, liar_vals.1];
        let signed: Vec<SignedObservation> = (0..7u8).map(|i| obs(i, r, &[(1, vals[usize::from(i)], 5)])).collect();
        let mut subsets = 0;
        for mask in 0u32..128 {
            let signers: Vec<u8> = (0..7u8).filter(|i| mask & (1 << i) != 0).collect();
            if signers.len() < 5 {
                continue;
            }
            subsets += 1;
            let mut st = base.clone();
            let u = OracleUpdate { round: r, observations: signers.iter().map(|&i| signed[usize::from(i)].clone()).collect() };
            let out = apply_update(&prm, &mut st, &u, t_of(r)).unwrap();
            let inc_honest: Vec<u64> = signers.iter().filter(|&&i| i < 5).map(|&i| vals[usize::from(i)]).collect();
            let (lo, hi) = (*inc_honest.iter().min().unwrap(), *inc_honest.iter().max().unwrap());
            match out.outcomes[0].1 {
                MarketOutcome::Moved { price } => assert!(lo <= price && price <= hi, "mask {mask:#b} liars {liar_vals:?}: {price} outside [{lo}, {hi}]"),
                MarketOutcome::Disagree { .. } => assert_eq!(st.safe_index(1), Some(648_700)),
                other => panic!("mask {mask:#b} liars {liar_vals:?}: unexpected {other:?}"),
            }
        }
        assert_eq!(subsets, 29, "C(7,5) + C(7,6) + C(7,7)");
    }
}

#[test]
fn c3_sources_below_minimum_do_not_count() {
    let (prm, mut st, r) = active_at(648_700);
    let mut u = honest(r, 649_000);
    for i in [0usize, 1, 2] {
        u.observations[i] = obs(i as u8, r, &[(1, 649_000, 2)]);
    }
    let out = apply_update(&prm, &mut st, &u, t_of(r)).unwrap();
    assert_eq!(out.outcomes[0], (1, MarketOutcome::NoQuorum { have: 4 }));
    assert_eq!(st.safe_index(1), Some(648_700));
}

#[test]
fn c4_honest_disagreement_changes_nothing() {
    let (prm, mut st, r) = active_at(648_700);
    // Venues diverge: signers see 2% apart.
    let u = update(r, &[0, 1, 2, 3, 4, 5, 6], &[630_000, 640_000, 648_700, 649_000, 660_000, 665_000, 670_000]);
    let out = apply_update(&prm, &mut st, &u, t_of(r)).unwrap();
    assert_eq!(out.outcomes[0], (1, MarketOutcome::Disagree { median: 649_000, agree: 2 }));
    assert_eq!(st.safe_index(1), Some(648_700));
    // A rejected-by-C4 round still consumes the round number (C1 is about the input, not its outcome).
    assert_eq!(st.last_round, r);
}

#[test]
fn unknown_markets_are_ignored() {
    let (prm, mut st, r) = active_at(648_700);
    let u = OracleUpdate { round: r, observations: (0..7).map(|i| obs(i, r, &[(1, 648_800, 5), (77, 5, 5)])).collect() };
    let out = apply_update(&prm, &mut st, &u, t_of(r)).unwrap();
    assert_eq!(out.outcomes.len(), 3);
    assert_eq!(out.outcomes[0], (1, MarketOutcome::Moved { price: 648_800 }));
}

#[test]
fn h2_flash_move_halts_and_a_reversion_resumes_fast() {
    let (prm, mut st, r) = active_at(650_000);
    // +5% exactly is still a step; +5.0002% halts.
    let out = apply_update(&prm, &mut st, &honest(r, 682_500), t_of(r)).unwrap();
    assert_eq!(out.outcomes[0].1, MarketOutcome::Moved { price: 682_500 });
    let out = apply_update(&prm, &mut st, &honest(r + 1, 650_000 * 2), t_of(r + 1)).unwrap();
    assert_eq!(out.outcomes[0].1, MarketOutcome::HaltedJump { price: 1_300_000 });
    assert_eq!(st.safe_index(1), None, "a halted market gives the engine no price");
    assert_eq!(st.market(1).unwrap().index, 682_500, "the index did not move");
    // The flash reverts in the next round: fast return.
    let out = apply_update(&prm, &mut st, &honest(r + 2, 682_000), t_of(r + 2)).unwrap();
    assert_eq!(out.outcomes[0].1, MarketOutcome::Resumed { price: 682_000, confirmed: false });
}

#[test]
fn h3_a_genuine_move_is_followed_after_confirmation() {
    let (prm, mut st, r) = active_at(650_000);
    let out = apply_update(&prm, &mut st, &honest(r, 600_000), t_of(r)).unwrap();
    assert_eq!(out.outcomes[0].1, MarketOutcome::HaltedJump { price: 600_000 });
    let out = apply_update(&prm, &mut st, &honest(r + 1, 601_000), t_of(r + 1)).unwrap();
    assert_eq!(out.outcomes[0].1, MarketOutcome::Confirming { price: 601_000, count: 2 });
    let out = apply_update(&prm, &mut st, &honest(r + 2, 600_500), t_of(r + 2)).unwrap();
    assert_eq!(out.outcomes[0].1, MarketOutcome::Resumed { price: 600_500, confirmed: true });
    assert_eq!(st.safe_index(1), Some(600_500));
}

#[test]
fn h3_an_oscillating_manipulation_never_confirms() {
    let (prm, mut st, r) = active_at(650_000);
    // Prices alternate 10% apart every round: each round restarts the run; never resumes, never moves the index.
    for k in 0..20u64 {
        let p = if k % 2 == 0 { 590_000 } else { 520_000 };
        let out = apply_update(&prm, &mut st, &honest(r + k, p), t_of(r + k)).unwrap();
        assert!(matches!(out.outcomes[0].1, MarketOutcome::HaltedJump { .. } | MarketOutcome::Confirming { count: 1, .. }), "{k}: {:?}", out.outcomes[0].1);
        assert_eq!(st.market(1).unwrap().index, 650_000);
    }
}

#[test]
fn h3_confirmation_runs_need_freshness() {
    let (prm, mut st, r) = active_at(650_000);
    apply_update(&prm, &mut st, &honest(r, 600_000), t_of(r)).unwrap();
    // The next agreeing price comes 11 s later: the run restarts.
    let r2 = r + 4;
    let out = apply_update(&prm, &mut st, &honest(r2, 600_000), t_of(r2)).unwrap();
    assert_eq!(out.outcomes[0].1, MarketOutcome::Confirming { price: 600_000, count: 1 });
}

#[test]
fn h1_withholding_halts_stale_and_needs_confirmation_after() {
    let (prm, mut st, r) = active_at(650_000);
    let index_time = st.market(1).unwrap().index_time_ms;
    // The sequencer stops including oracle updates. 10 s after the index: still active; 10.001 s: halted.
    assert!(begin_block(&prm, &mut st, index_time + 10_000).is_empty());
    assert_eq!(begin_block(&prm, &mut st, index_time + 10_001), vec![1]);
    assert_eq!(st.market(1).unwrap().status, Status::Halted(HaltReason::Stale));
    assert_eq!(st.safe_index(1), None);
    // Updates return with the same price: no fast return from a stale index, three rounds needed.
    let r2 = r + 10;
    let out = apply_update(&prm, &mut st, &honest(r2, 650_000), t_of(r2)).unwrap();
    assert_eq!(out.outcomes[0].1, MarketOutcome::Confirming { price: 650_000, count: 1 });
    apply_update(&prm, &mut st, &honest(r2 + 1, 650_000), t_of(r2 + 1)).unwrap();
    let out = apply_update(&prm, &mut st, &honest(r2 + 2, 650_000), t_of(r2 + 2)).unwrap();
    assert_eq!(out.outcomes[0].1, MarketOutcome::Resumed { price: 650_000, confirmed: true });
}

#[test]
fn markets_are_independent() {
    let prm = params();
    let mut st = OracleState::genesis(&prm).unwrap();
    for k in 0..3 {
        let r = R0 + k;
        let u = OracleUpdate { round: r, observations: (0..7).map(|i| obs(i, r, &[(1, 650_000, 5), (2, 348_000, 5)])).collect() };
        apply_update(&prm, &mut st, &u, t_of(r)).unwrap();
    }
    let r = R0 + 3;
    // ETH jumps, BTC moves normally, SOL has no data.
    let u = OracleUpdate { round: r, observations: (0..7).map(|i| obs(i, r, &[(1, 651_000, 5), (2, 400_000, 5)])).collect() };
    let out = apply_update(&prm, &mut st, &u, t_of(r)).unwrap();
    assert_eq!(out.outcomes, vec![
        (1, MarketOutcome::Moved { price: 651_000 }),
        (2, MarketOutcome::HaltedJump { price: 400_000 }),
        (3, MarketOutcome::NoQuorum { have: 0 }),
    ]);
}

#[test]
fn decoded_bytes_apply_identically() {
    let (prm, st, r) = active_at(650_000);
    let u = update(r, &[0, 2, 3, 5, 6], &[650_100, 650_200, 650_000, 650_300, 650_050]);
    let b = encoding::encode(&u);
    let u2 = encoding::decode(&b).unwrap();
    let (mut a, mut c) = (st.clone(), st);
    assert_eq!(apply_update(&prm, &mut a, &u, t_of(r)), apply_update(&prm, &mut c, &u2, t_of(r)));
    assert_eq!(a.encode(), c.encode());
}

#[test]
fn parameter_validation() {
    let ok = params();
    assert!(ok.validate().is_ok());
    let mut p = ok.clone();
    p.quorum = 3;
    assert_eq!(p.validate(), Err(ParamError::QuorumNotMajority), "3 of 7 is not a majority");
    p.quorum = 4;
    assert!(p.validate().is_ok());
    p.quorum = 8;
    assert_eq!(p.validate(), Err(ParamError::QuorumNotMajority));
    let mut p = ok.clone();
    p.signers[3] = p.signers[1];
    assert_eq!(p.validate(), Err(ParamError::DuplicateSigner));
    let mut p = ok.clone();
    p.signers[3] = [0; 20];
    assert_eq!(p.validate(), Err(ParamError::ZeroSigner));
    let mut p = ok.clone();
    p.signers.clear();
    assert_eq!(p.validate(), Err(ParamError::CommitteeSize));
    let mut p = ok.clone();
    p.markets.swap(0, 1);
    assert_eq!(p.validate(), Err(ParamError::MarketOrder));
    let mut p = ok.clone();
    p.markets[0].confirm_rounds = 1;
    assert_eq!(p.validate(), Err(ParamError::ConfirmRounds));
    let mut p = ok.clone();
    p.markets[0].agree_bps = 600;
    assert_eq!(p.validate(), Err(ParamError::AgreeAboveJump));
    let mut p = ok.clone();
    p.markets[0].jump_bps = 0;
    assert_eq!(p.validate(), Err(ParamError::Bps));
    let mut p = ok.clone();
    p.markets[0].max_staleness_ms = 2999;
    assert_eq!(p.validate(), Err(ParamError::Staleness));
    let mut p = ok.clone();
    p.max_age_ms = 10_001;
    assert_eq!(p.validate(), Err(ParamError::AgeAboveStaleness));
    let mut p = ok.clone();
    p.markets[0].min_sources = 0;
    assert_eq!(p.validate(), Err(ParamError::MinSources));
    let mut p = ok.clone();
    p.verifying_contract = [0; 20];
    assert_eq!(p.validate(), Err(ParamError::ZeroVault));
    let mut p = ok.clone();
    p.markets[0].exit_haircut_bps = 499;
    assert_eq!(p.validate(), Err(ParamError::ExitBand), "the band starts at least one permitted step wide");
    let mut p = ok.clone();
    p.markets[0].exit_max_haircut_bps = 10_000;
    assert_eq!(p.validate(), Err(ParamError::ExitBand));
    let mut p = ok.clone();
    p.markets[0].exit_max_haircut_bps = 400;
    assert_eq!(p.validate(), Err(ParamError::ExitBand));
    let mut p = ok.clone();
    p.markets[0].max_halt_ms = 9_999;
    assert_eq!(p.validate(), Err(ParamError::ExitBand));
    let mut p = ok;
    p.markets.clear();
    assert_eq!(p.validate(), Err(ParamError::NoMarkets));
    assert!(OracleState::genesis(&p).is_err());
}

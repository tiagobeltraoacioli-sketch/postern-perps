//! Agent 392's engine requests (SPEC 0.1.2): funding clamp per market, volume fee tiers, launch gate, backstop
//! position limit, and the converter from compliance/markets.json + risk.json.
use k256::ecdsa::SigningKey;
use postern_perps_engine::engine::{system_address, Event, FillKind, ACCT_BACKSTOP, ACCT_INSURANCE};
use postern_perps_engine::genesis::{from_compliance, FeeTier, Genesis};
use postern_perps_engine::hash::Addr;
use postern_perps_engine::invariants;
use postern_perps_engine::testkit::*;
use postern_perps_engine::*;

const T0: u64 = 1_794_880_000_000;
const P: u64 = 6_725_000_000_000;

struct Rig {
    e: Engine,
    ok: Vec<SigningKey>,
    h: u64,
    t: u64,
    dep: u64,
}

impl Rig {
    fn new(g: Genesis) -> Rig {
        Rig { e: Engine::new(g.check().unwrap()), ok: oracle_keys(7), h: 0, t: T0, dep: 0 }
    }
    fn dep(&mut self, a: Addr, usdc: u64) -> Input {
        let i = Input::Deposit { index: self.dep, account: a, amount: usdc * 1_000_000 };
        self.dep += 1;
        i
    }
    fn tick(&mut self, dt: u64, p: u64, mut rest: Vec<Input>) -> BlockOutcome {
        self.h += 1;
        self.t += dt;
        let mut inputs = vec![oracle_round(&self.e, &self.ok, self.t, &[(1, p)])];
        inputs.append(&mut rest);
        let out = self.e.apply_block(&Block { height: self.h, time: self.t, inputs }).unwrap();
        invariants::check(&self.e).unwrap();
        out
    }
}

fn base() -> Genesis {
    genesis_raw(7, vec![market(1, "BTC-USD")])
}

fn trade(r: &mut Rig, kb: &SigningKey, ka: &SigningKey, size: u64) -> BlockOutcome {
    let t = r.t + 1000;
    let (a, b) = (addr(ka), addr(kb));
    let ask = sign_action(&r.e, kb, place(b, t, 1, false, ORDER_LIMIT, TIF_GTC, P, size, false));
    let buy = sign_action(&r.e, ka, place(a, t, 1, true, ORDER_MARKET, TIF_IOC, P + 1_000_000_000, size, false));
    let out = r.tick(1000, P, vec![ask, buy]);
    assert_eq!(out.outcomes[1..], [Ok(()), Ok(())]);
    out
}

#[test]
fn converter_reads_392s_files_exactly() {
    let m: serde_json::Value = serde_json::from_str(include_str!("fixtures/compliance-0.2.1/markets.json")).unwrap();
    let rk: serde_json::Value = serde_json::from_str(include_str!("fixtures/compliance-0.2.1/risk.json")).unwrap();
    let signers: Vec<String> = oracle_keys(7).iter().map(|k| postern_perps_engine::hash::hex0x(&addr(k))).collect();
    let g = from_compliance(&m, &rk, DEV_VAULT, signers.clone(), 5, "25000", 250_000).expect("0.2.1 converts");
    let btc = &g.markets[0];
    assert_eq!((btc.symbol.as_str(), btc.tick, btc.step, btc.oi_cap), ("BTC-USD", 50_000_000, 10_000, 3_000_000_000));
    assert_eq!(btc.funding_clamp, 62_500);
    assert_eq!(g.fee_tiers.len(), 5);
    assert_eq!((g.fee_tiers[4].min_volume_14d, g.fee_tiers[4].maker_fee_ppm), (500_000_000_000_000, 0));
    assert_eq!(g.launch_insurance_min, 60_000_000_000);
    assert_eq!(g.withdrawal_fee, 1_000_000);
    // owner inputs are never guessed
    assert!(from_compliance(&m, &rk, DEV_VAULT, signers.clone(), 5, "TBD-owner", 250_000).is_err());
    // a value finer than the fixed scale is refused, not rounded
    let mut bad = m.clone();
    bad["markets"][0]["tick"] = serde_json::Value::String("0.000000005".into());
    assert!(from_compliance(&bad, &rk, DEV_VAULT, signers, 5, "25000", 250_000).is_err());
}

#[test]
fn funding_clamp_is_per_market() {
    let mut g = base();
    g.markets[0].funding_clamp = 5_000; // interest 12500 clamped to 5000 when the premium is 0
    let mut r = Rig::new(g);
    let (ka, kb) = (key(1), key(2));
    let to_b = 3_600_000 - (r.t % 3_600_000) - 10_000;
    let (d1, d2) = (r.dep(addr(&ka), 100_000), r.dep(addr(&kb), 100_000));
    r.tick(to_b, P, vec![d1, d2]);
    trade(&mut r, &kb, &ka, 10_000_000);
    let mut rate = None;
    for _ in 0..6 {
        let out = r.tick(3000, P, vec![]);
        for e in out.events {
            if let Event::Funding { rate: x, .. } = e {
                rate = Some(x);
            }
        }
    }
    assert_eq!(rate, Some(5_000));
}

#[test]
fn volume_tier_applies_from_the_next_utc_day() {
    let mut g = base();
    g.fee_tiers = vec![
        FeeTier { min_volume_14d: 0, taker_fee_ppm: 450, maker_fee_ppm: 150 },
        FeeTier { min_volume_14d: 10_000_000_000, taker_fee_ppm: 300, maker_fee_ppm: 0 },
    ];
    let mut r = Rig::new(g);
    let (ka, kb) = (key(1), key(2));
    let (d1, d2) = (r.dep(addr(&ka), 100_000), r.dep(addr(&kb), 100_000));
    r.tick(1000, P, vec![d1, d2]);
    let fees = |out: &BlockOutcome| out.events.iter().find_map(|e| match e { Event::Fill { taker_fee, maker_fee, kind: FillKind::Book, .. } => Some((*taker_fee, *maker_fee)), _ => None }).unwrap();
    let out = trade(&mut r, &kb, &ka, 20_000_000); // 13,450 USDC notional each: above the tier-1 floor
    assert_eq!(fees(&out), (6_052_500, 2_017_500), "tier 0 the same day");
    let out = trade(&mut r, &kb, &ka, 20_000_000);
    assert_eq!(fees(&out), (6_052_500, 2_017_500), "still tier 0: today's volume does not count until tomorrow");
    // next UTC day
    let to_next_day = 86_400_000 - (r.t % 86_400_000) + 1000;
    r.tick(to_next_day, P, vec![]);
    let t = r.t + 1000;
    let (a, b) = (addr(&ka), addr(&kb));
    let ask = sign_action(&r.e, &kb, place(b, t, 1, false, ORDER_LIMIT, TIF_GTC, P, 20_000_000, false));
    let sell_close = sign_action(&r.e, &ka, place(a, t, 1, false, ORDER_LIMIT, TIF_GTC, P + 10_000_000, 10_000_000, false));
    let buy = sign_action(&r.e, &ka, place(a, t + 1, 1, true, ORDER_MARKET, TIF_IOC, P + 1_000_000_000, 20_000_000, false));
    let out = r.tick(3000, P, vec![ask, sell_close, buy]);
    assert!(out.outcomes.iter().all(|o| o.is_ok()), "{:?}", out.outcomes);
    assert_eq!(fees(&out), (4_035_000, 0), "tier 1 the next day for both sides");
}

#[test]
fn launch_gate_holds_orders_until_both_seeds_arrive() {
    let mut g = base();
    g.launch_insurance_min = 100_000_000;
    g.launch_backstop_min = 50_000_000;
    let mut r = Rig::new(g);
    let ka = key(1);
    let a = addr(&ka);
    let d = r.dep(a, 1000);
    r.tick(1000, P, vec![d]);
    let t = r.t + 1000;
    let o1 = sign_action(&r.e, &ka, place(a, t, 1, true, ORDER_LIMIT, TIF_GTC, P - 1_000_000_000, 100_000, false));
    let ins = r.dep(system_address(ACCT_INSURANCE), 100);
    let out = r.tick(1000, P, vec![ins, o1]);
    assert_eq!(out.outcomes[2], Err(Error::NotLaunched), "insurance alone is not enough");
    let t = r.t + 1000;
    let bs = r.dep(system_address(ACCT_BACKSTOP), 50);
    let o2 = sign_action(&r.e, &ka, place(a, t, 1, true, ORDER_LIMIT, TIF_GTC, P - 1_000_000_000, 100_000, false));
    let out = r.tick(1000, P, vec![bs, o2]);
    assert_eq!(out.outcomes[2], Ok(()));
    assert_eq!(out.events.iter().filter(|e| matches!(e, Event::Launched)).count(), 1);
    assert!(r.e.launched);
}

#[test]
fn backstop_takes_at_most_its_limit_then_adl() {
    let mut g = base();
    g.backstop_oi_ppm = 50; // oiCap 1000 BTC -> limit 0.05 BTC
    let mut r = Rig::new(g);
    let (ka, kb) = (key(1), key(2));
    let (a, b) = (addr(&ka), addr(&kb));
    let (d1, d2, d3) = (r.dep(a, 700), r.dep(b, 50_000), r.dep(system_address(ACCT_BACKSTOP), 100_000));
    r.tick(1000, P, vec![d1, d2, d3]);
    trade(&mut r, &kb, &ka, 10_000_000);
    let mut p = P;
    for _ in 0..12 {
        p = p * 985 / 1000 / 10_000_000 * 10_000_000;
        let out = r.tick(3000, p, vec![]);
        if out.events.iter().any(|e| matches!(e, Event::Liquidated { .. })) {
            break;
        }
    }
    let size = |e: &Engine, i: u32| e.accounts[i as usize].positions.get(&1).map(|x| x.size).unwrap_or(0);
    assert_eq!(size(&r.e, r.e.by_addr[&a]), 0, "a closed");
    assert_eq!(size(&r.e, ACCT_BACKSTOP), 5_000_000, "backstop took exactly its limit");
    assert_eq!(size(&r.e, r.e.by_addr[&b]), -5_000_000, "the rest was deleveraged against b");
}

#[test]
fn genesis_refuses_market_fees_that_differ_from_tier_0() {
    let mut g = base();
    g.markets[0].taker_fee_ppm = 400;
    assert!(g.check().is_err());
}

/// 392/397's user attestation is signed under the engine's domain (SPEC 2.1): the type name `Attestation` is
/// reserved and can never act. (a) no input type id decodes to it; (b) an attestation signature attached to any
/// action does not verify (different struct hash), so it is refused as BAD_SIGNATURE and consumes nothing.
#[test]
fn attestation_signature_is_never_an_action() {
    use postern_perps_engine::codec::decode_inputs;
    use postern_perps_engine::eip712::{digest, sign_digest};
    use postern_perps_engine::hash::{keccak, keccak_parts, word_addr, word_u};
    let mut r = Rig::new(base());
    let ka = key(1);
    let a = addr(&ka);
    let d = r.dep(a, 1000);
    r.tick(1000, P, vec![d]);
    // 392's type (compliance/legal/notices.json, attestation)
    let th = keccak(b"Attestation(address account,string documentId,bytes32 documentHash,string country,uint64 issuedAt)");
    let sh = keccak_parts(&[&th, &word_addr(&a), &keccak(b"attest-0.1-20261117"), &[0x11u8; 32], &keccak(b"ZZ"), &word_u((r.t / 1000) as u128)]);
    let att_sig = sign_digest(&ka, &digest(&r.e.domain_sep, &sh));
    let t = r.t + 1000;
    let order = place(a, t, 1, true, ORDER_LIMIT, TIF_GTC, P - 1_000_000_000, 100_000, false);
    let forged = Input::Action(SignedAction { action: order.clone(), signature: att_sig });
    let genuine = sign_action(&r.e, &ka, order);
    let out = r.tick(1000, P, vec![forged, genuine]);
    assert_eq!(out.outcomes[1..], [Err(Error::BadSignature), Ok(())], "the attestation signature is refused; the nonce stayed unused");
    // no type id 0 or 9..255 decodes (an `Attestation` input cannot even be expressed)
    for ty in [0u8, 9, 255] {
        let mut b = vec![0, 0, 0, 1, 0x04, 0, 0, 0, 1 + 20 + 65];
        b.push(ty);
        b.extend_from_slice(&[0u8; 20]);
        b.extend_from_slice(&[0u8; 65]);
        assert!(decode_inputs(&b).is_err());
    }
}

//! Round 2 (agent 430): the findings of the independent review 436 against the engine, each as a test of the FIXED
//! property that uses only the API of `main` f0e3967 and FAILS there (run recorded in agents/430-engine-r2.md).
//! 436's own demonstrations (`review/engine-tests/r436_engine.rs` on its branch) pass while the defect is present;
//! these are their counterparts. R436-1 (High), R436-2, R436-4 (High, engine half), R436-8, R436-10 (High), R436-12.
mod common;
use common::*;
use k256::ecdsa::SigningKey;
use postern_perps_engine::codec::{decode_inputs, encode_inputs};
use postern_perps_engine::eip712::{digest, oracle_struct_hash, sign_digest};
use postern_perps_engine::engine::{system_address, Event, FillKind, ACCT_INSURANCE};
use postern_perps_engine::hash::Addr;
use postern_perps_engine::oracle::{Status, ROUND_MS};
use postern_perps_engine::testkit::*;
use postern_perps_engine::*;

const ONE: u64 = 100_000_000;

fn ins(r: &Rig) -> i128 {
    r.e.accounts[ACCT_INSURANCE as usize].balance
}

fn lev50(r: &Rig, k: &SigningKey, n: u64) -> Input {
    sign_action(&r.e, k, Action::UpdateLeverage { account: addr(k), nonce: r.t + n, market_id: 1, leverage: 50, isolated: false })
}

fn order(r: &Rig, k: &SigningKey, n: u64, buy: bool, ty: u8, tif: u8, price: u64, size: u64) -> Input {
    sign_action(&r.e, k, place(addr(k), r.t + n, 1, buy, ty, tif, price, size, false))
}

/// R436-1, 436's scenario: A (2,000 USDC, 50x) offers 1 BTC at HALF the mark, B (10 USDC) lifts it. On f0e3967 A's
/// manufactured deficit of about 31,600 USDC is paid by INSURANCE and ends in B's balance. Fixed: the fund is
/// untouched and B holds what it deposited.
#[test]
fn r436_1_an_off_market_maker_order_cannot_move_the_insurance_fund() {
    let mut r = Rig::one();
    let (ka, kb) = (key(4361), key(4362));
    let (a, b) = (addr(&ka), addr(&kb));
    let deps = vec![r.dep(system_address(ACCT_INSURANCE), 60_000), r.dep(a, 2_000), r.dep(b, 10)];
    r.block(3000, &[(1, P)], deps);
    let low = P / 2;
    let inputs = vec![lev50(&r, &ka, 3000), order(&r, &ka, 3001, false, ORDER_LIMIT, TIF_GTC, low, ONE), order(&r, &kb, 3002, true, ORDER_LIMIT, TIF_IOC, low, ONE)];
    let out = r.block(3000, &[(1, P)], inputs);
    assert!(!out.events.iter().any(|e| matches!(e, Event::Fill { .. })), "no fill at half the mark: {:?}", out.events);
    assert_eq!(ins(&r), 60_000_000_000, "INSURANCE untouched");
    assert!(r.bal(&b) <= 10_000_000 && r.bal(&a) <= 2_000_000_000);
}

/// R436-1 inside any price band: A's ask rests AT the mark (fully reserved), the index then rises 4%, and the same
/// order is now a 2,690 USDC loss for an account that holds 2,000. On f0e3967 B lifts it and INSURANCE pays the
/// difference. Fixed (S-4.9.1): a fill that would leave the MAKER below its maintenance requirement at the mark is
/// not executed; the order is cancelled.
#[test]
fn r436_1_a_maker_fill_never_leaves_the_maker_below_maintenance() {
    let mut r = Rig::one();
    let (ka, kb) = (key(4363), key(4364));
    let (a, b) = (addr(&ka), addr(&kb));
    let deps = vec![r.dep(system_address(ACCT_INSURANCE), 60_000), r.dep(a, 2_000), r.dep(b, 10)];
    r.block(3000, &[(1, P)], deps);
    let inputs = vec![lev50(&r, &ka, 3000), lev50(&r, &kb, 3000), order(&r, &ka, 3001, false, ORDER_LIMIT, TIF_GTC, P, ONE)];
    let out = r.block(3000, &[(1, P)], inputs);
    assert!(out.outcomes.iter().all(|o| o.is_ok()), "{:?}", out.outcomes);
    let up = tick(P / 100 * 104);
    let buy = order(&r, &kb, 3000, true, ORDER_LIMIT, TIF_IOC, P, ONE);
    let out = r.block(3000, &[(1, up)], vec![buy]);
    assert_eq!(r.e.markets[&1].mark, up, "one ask below the index does not move the mark (median of bid, ask, last)");
    assert!(!out.events.iter().any(|e| matches!(e, Event::Fill { .. })), "the stale ask is not filled: {:?}", out.events);
    assert_eq!(ins(&r), 60_000_000_000, "INSURANCE untouched");
    assert!(r.e.accounts[r.ai(&a) as usize].orders.is_empty(), "the order that could not be honoured is gone");
    assert!(r.bal(&a) >= 0 && r.bal(&b) <= 10_000_000);
}

/// R436-1, the TAKER side 436 did not list: a fill that only REDUCES was never margin-checked. A (long 1 BTC, about
/// 2,070 USDC) sells into its colluder's bid 4.5% below the mark: a 3,026 USDC loss. On f0e3967 A ends flat with a
/// negative balance that INSURANCE pays as bad debt. Fixed: the taker stops (INSUFFICIENT_MARGIN).
#[test]
fn r436_1_a_reducing_taker_fill_never_leaves_the_taker_below_maintenance() {
    let mut r = Rig::one();
    let (ka, kb, km) = (key(4365), key(4366), key(4367));
    let (a, b) = (addr(&ka), addr(&kb));
    let deps = vec![r.dep(system_address(ACCT_INSURANCE), 60_000), r.dep(a, 2_100), r.dep(b, 20_000), r.dep(addr(&km), 100_000)];
    r.block(3000, &[(1, P)], deps);
    let l = lev50(&r, &ka, 3000);
    r.block(3000, &[(1, P)], vec![l]);
    r.trade(&[(1, P)], 1, &km, &ka, P, ONE); // A long 1 BTC at the mark
    let bid_px = tick(P / 1000 * 955);
    let inputs = vec![order(&r, &kb, 3000, true, ORDER_LIMIT, TIF_GTC, bid_px, ONE), order(&r, &ka, 3001, false, ORDER_LIMIT, TIF_IOC, bid_px, ONE)];
    let out = r.block(3000, &[(1, P)], inputs);
    assert!(!out.events.iter().any(|e| matches!(e, Event::Fill { .. })), "A's ruinous close is not executed: {:?}", out.events);
    assert_eq!(ins(&r), 60_000_000_000, "INSURANCE untouched");
    assert_eq!(r.size(r.ai(&a), 1), ONE as i64);
}

/// R436-1 through the forced path (found while fixing it): a forced close sold down to mark - 5% whatever the
/// account's equity. A (long 1 BTC, about 2,070 USDC) requests a forced close while its colluder's bid 4.9% below
/// the mark is the only bid: on f0e3967 A loses 3,295 USDC and INSURANCE pays the rest. Fixed: a forced close never
/// goes past the account's bankruptcy price; what the book does not take inside it goes to ADL at that price.
#[test]
fn r436_1_a_forced_close_never_goes_past_the_bankruptcy_price() {
    let mut r = Rig::one();
    let (ka, kb, km) = (key(4368), key(4369), key(4370));
    let (a, b) = (addr(&ka), addr(&kb));
    let deps = vec![r.dep(system_address(ACCT_INSURANCE), 60_000), r.dep(a, 2_100), r.dep(b, 20_000), r.dep(addr(&km), 100_000)];
    r.block(3000, &[(1, P)], deps);
    let l = lev50(&r, &ka, 3000);
    r.block(3000, &[(1, P)], vec![l]);
    r.trade(&[(1, P)], 1, &km, &ka, P, ONE);
    let bid = order(&r, &kb, 3000, true, ORDER_LIMIT, TIF_GTC, tick(P / 1000 * 951), ONE);
    r.block(3000, &[(1, P)], vec![bid]);
    let f = r.forced(a, FORCED_CLOSE, 0);
    let out = r.block(3000, &[(1, P)], vec![f]);
    assert!(out.events.iter().any(|e| matches!(e, Event::ForcedServed { index: 0 })), "{:?}", out.events);
    assert_eq!(r.size(r.ai(&a), 1), 0, "closed");
    assert_eq!(ins(&r), 60_000_000_000, "INSURANCE untouched");
    assert!(r.bal(&a) >= 0, "no deficit: {}", r.bal(&a));
    assert_eq!(r.size(r.ai(&b), 1), 0, "the bid beyond the bankruptcy price did not fill");
}

/// R436-2, 436's scenario. Returns (kind, price) of the fill that closed B.
fn r436_2_run(grief: bool) -> (FillKind, u64, i128) {
    let mut r = Rig::one();
    let (ka, kb, kd) = (key(4371), key(4372), key(4373));
    let (a, b, d) = (addr(&ka), addr(&kb), addr(&kd));
    let deps = vec![r.dep(a, 100_000), r.dep(b, 7_000), r.dep(d, 100_000)];
    r.block(3000, &[(1, P)], deps);
    let i = vec![order(&r, &ka, 3000, true, ORDER_LIMIT, TIF_GTC, P, ONE), order(&r, &kb, 3001, false, ORDER_MARKET, TIF_IOC, P - 100 * TICK, ONE)];
    let out = r.block(3000, &[(1, P)], i);
    assert!(out.outcomes.iter().all(|o| o.is_ok()));
    let p1 = tick(P / 1000 * 1048);
    let p2 = tick(p1 / 1000 * 1046);
    let old_ask = order(&r, &kd, 3000, false, ORDER_LIMIT, TIF_GTC, p2 + TICK, ONE);
    let out = r.block(3000, &[(1, p1)], vec![old_ask]);
    assert!(out.outcomes.iter().all(|o| o.is_ok()), "{:?}", out.outcomes);
    let mut inputs = vec![];
    if grief {
        inputs.push(order(&r, &ka, 3000, false, ORDER_LIMIT, TIF_GTC, p2, 100_000));
    }
    let out = r.block(3000, &[(1, p2)], inputs);
    let bi = r.ai(&b);
    assert!(out.events.iter().any(|e| matches!(e, Event::Liquidated { account } if *account == bi)), "B liquidated in the block of the move: {:?}", out.events);
    let (price, kind) = out.events.iter().find_map(|e| match e { Event::Fill { taker, price, kind, .. } if *taker == bi => Some((*price, *kind)), _ => None }).expect("a fill closed B");
    (kind, price, r.bal(&b))
}

/// R436-2: a fresh minimum-size order at the top of the book must not hide the older liquidity behind it from the
/// block's liquidations (f0e3967 stopped matching at it and sent the position to ADL at the bound price).
#[test]
fn r436_2_a_same_block_order_is_skipped_and_older_liquidity_is_used() {
    let (kind_plain, price_plain, bal_plain) = r436_2_run(false);
    let (kind_grief, price_grief, bal_grief) = r436_2_run(true);
    assert_eq!(kind_plain, FillKind::Liquidation);
    assert_eq!((kind_grief, price_grief), (FillKind::Liquidation, price_plain), "the same old ask closes B with or without the fresh order");
    assert_eq!(bal_grief, bal_plain, "B keeps the same balance");
}

/// R436-4 (engine half): an ApproveAgent whose name is 33 bytes is a well-formed input that FAILS (`MALFORMED`); it
/// must not make the block's input list undecodable (f0e3967: "agent name > 32 bytes" refused the whole list, and
/// the sequencer halted on its own block).
#[test]
fn r436_4_a_33_byte_agent_name_fails_alone_and_never_rejects_the_block() {
    let mut r = Rig::one();
    let (ka, kb) = (key(1), key(2));
    let (a, b) = (addr(&ka), addr(&kb));
    let (d1, d2) = (r.dep(a, 1_000), r.dep(b, 1_000));
    r.block(3000, &[(1, P)], vec![d1, d2]);
    let name = "x".repeat(33);
    let bad = sign_action(&r.e, &ka, Action::ApproveAgent { account: a, nonce: r.t + 1000, agent: addr(&key(77)), name, expires_at: r.t + 3_600_000 });
    let good = sign_action(&r.e, &kb, Action::CancelAll { account: b, nonce: r.t + 1000, market_id: 0 });
    let bytes = encode_inputs(&[bad, good]);
    let decoded = decode_inputs(&bytes).expect("the input list decodes: the name rule is not a block rule");
    assert_eq!(encode_inputs(&decoded), bytes);
    let out = r.block(1000, &[], decoded);
    assert_eq!(out.outcomes, [Err(Error::Malformed), Ok(())], "the long name fails by itself; the other action is applied");
    assert!(r.e.accounts[r.ai(&a) as usize].agents.is_empty());
    // the longest name the wire can carry behaves the same
    let huge = sign_action(&r.e, &ka, Action::ApproveAgent { account: a, nonce: r.t + 1001, agent: addr(&key(77)), name: "y".repeat(65_535), expires_at: r.t + 3_600_000 });
    let out = r.block(1000, &[], decode_inputs(&encode_inputs(&[huge])).expect("decodes"));
    assert_eq!(out.outcomes, [Err(Error::Malformed)]);
}

/// R436-8: a genesis with `impactNotional = 0` is refused (f0e3967 accepted it and divided by zero at the first
/// premium sample), and the compliance converter refuses an integer that does not fit its field instead of
/// narrowing it (f0e3967 read imfPpm 4295017296 as 50000 and minSources 259 as 3).
#[test]
fn r436_8_genesis_refuses_impact_notional_zero_and_out_of_range_integers() {
    let mut m = market(1, "BTC-USD");
    m.impact_notional = 0;
    assert!(genesis_raw(7, vec![m]).check().is_err(), "impactNotional 0");
    let dir = concat!(env!("CARGO_MANIFEST_DIR"), "/../compliance/");
    let markets: serde_json::Value = serde_json::from_str(&std::fs::read_to_string(format!("{dir}markets.json")).unwrap()).unwrap();
    let risk: serde_json::Value = serde_json::from_str(&std::fs::read_to_string(format!("{dir}risk.json")).unwrap()).unwrap();
    let signers: Vec<String> = oracle_keys(7).iter().map(|k| postern_perps_engine::hash::hex0x(&addr(k))).collect();
    let conv = |m: &serde_json::Value, rk: &serde_json::Value| postern_perps_engine::genesis::from_compliance(m, rk, DEV_VAULT, signers.clone(), 5, "25000", 250_000);
    let honest = conv(&markets, &risk).expect("the committed files convert");
    let cases: Vec<(&str, Box<dyn Fn(&mut serde_json::Value, &mut serde_json::Value)>)> = vec![
        ("imfPpm + 2^32", Box::new(|m, _| m["markets"][0]["tiers"][0]["imfPpm"] = serde_json::json!(50_000u64 + (1u64 << 32)))),
        ("minSources + 256", Box::new(|m, _| m["markets"][0]["oracle"]["minSources"] = serde_json::json!(3 + 256))),
        ("maxLeverage + 2^32", Box::new(|m, _| m["markets"][0]["maxLeverage"] = serde_json::json!(20u64 + (1u64 << 32)))),
        ("negative markBandBps", Box::new(|m, _| m["markets"][0]["markBandBps"] = serde_json::json!(-4294967246i64))),
        ("optional exitHaircutBps + 2^32", Box::new(|m, _| m["markets"][0]["oracle"]["exitHaircutBps"] = serde_json::json!(500u64 + (1u64 << 32)))),
        ("maxAgeMs negative", Box::new(|_, rk| rk["global"]["maxAgeMs"] = serde_json::json!(-1))),
    ];
    assert_eq!(honest.markets[0].tiers[0].imf_ppm, 50_000);
    for (name, f) in cases {
        let (mut m, mut rk) = (markets.clone(), risk.clone());
        f(&mut m, &mut rk);
        assert!(conv(&m, &rk).is_err(), "{name}: accepted");
    }
}

/// R436-10, 436's scenario. A long 1 BTC against S short 1 BTC; the oracle is withheld (stale halt, index frozen);
/// with `forced`, S's forced close is processed in the halt; the oracle returns 4% higher.
/// Returns (A's equity, S's equity, price of S's forced fill).
fn r436_10_run(forced: bool) -> (i128, i128, Option<u64>) {
    let mut r = Rig::one();
    let (ka, ks) = (key(4391), key(4392));
    let (a, s) = (addr(&ka), addr(&ks));
    let deps = vec![r.dep(a, 50_000), r.dep(s, 50_000)];
    r.block(3000, &[(1, P)], deps);
    r.trade(&[(1, P)], 1, &ks, &ka, P, ONE);
    for _ in 0..4 {
        r.block(3000, &[], vec![]);
    }
    assert_ne!(r.e.markets[&1].oracle.status, Status::Active, "halted (stale)");
    let mut fill = None;
    if forced {
        let f = r.forced(s, FORCED_CLOSE, 0);
        let out = r.block(3000, &[], vec![f]);
        assert!(out.events.iter().any(|e| matches!(e, Event::ForcedServed { index: 0 })), "served during the halt: {:?}", out.events);
        fill = out.events.iter().find_map(|e| match e { Event::Fill { kind: FillKind::Adl, price, .. } => Some(*price), _ => None });
    }
    let up = tick(P / 100 * 104);
    r.block(3000, &[(1, up)], vec![]);
    r.block(3000, &[(1, up)], vec![]);
    assert_eq!((r.e.markets[&1].oracle.status, r.e.markets[&1].oracle.index), (Status::Active, up), "resumed 4% higher");
    let eq = |x: &Addr| r.e.equity(r.ai(x)) / 10_000_000_000;
    (eq(&a), eq(&s), fill)
}

/// R436-10 (High). A forced close in a halted market was served at `settle` = the frozen index: whoever decides
/// when the index freezes (the sequencer, by withholding oracle inputs) held a free stop-loss at the last price,
/// paid by the deleveraged counterparty. Fixed (432's corrected README 6a): the close is priced at the side of the
/// exit band WORSE for the closer (a short at `high`); forcing the close in a halt is never better for the closer
/// than not forcing it, and never worse for the counterparty.
#[test]
fn r436_10_a_forced_close_in_a_halt_is_priced_at_the_band_side_worse_for_the_closer() {
    let (a_plain, s_plain, _) = r436_10_run(false);
    let (a_forced, s_forced, fill) = r436_10_run(true);
    let fill = fill.expect("an ADL fill closed S");
    assert!(fill > tick(P / 100 * 104), "S (short) bought back at the HIGH side of the exit band ({fill}), not at the frozen index {P}");
    assert!(s_forced <= s_plain, "the forced exit gave S nothing: {s_forced} vs {s_plain}");
    assert!(a_forced >= a_plain, "and took nothing from A: {a_forced} vs {a_plain}");
}

/// R436-10, X3 (432's rule (2)): an hour of SILENCE settled every position at the frozen pre-halt index on
/// f0e3967: the same free exit for the whole market, chosen by whoever withholds the oracle input. Fixed: X3
/// settles only at a price the committee signed since the halt; with none the market stays halted and positions
/// stay open (single accounts exit at the band; a lasting silence ends in the vault's escape).
#[test]
fn r436_10_silence_alone_never_settles_a_market() {
    let mut r = Rig::one();
    let (ka, ks) = (key(4393), key(4394));
    let deps = vec![r.dep(addr(&ka), 50_000), r.dep(addr(&ks), 50_000)];
    r.block(3000, &[(1, P)], deps);
    r.trade(&[(1, P)], 1, &ks, &ka, P, ONE);
    r.block(11_000, &[], vec![]);
    let out = r.block(3_600_000, &[], vec![]);
    assert!(!out.events.iter().any(|e| matches!(e, Event::MarketSettled { .. })), "no settlement at the frozen index: {:?}", out.events);
    assert_eq!(r.size(r.ai(&addr(&ka)), 1), ONE as i64);
    assert_ne!(r.e.markets[&1].oracle.status, Status::Active);
    let out = r.block(7_200_000, &[], vec![]);
    assert!(!out.events.iter().any(|e| matches!(e, Event::MarketSettled { .. })));
}

fn observation(r: &Rig, signer: usize, round: u64, time_ms: u64, price: u64, key_of: &SigningKey) -> SignedObservation {
    let prices = vec![OraclePrice { market_id: 1, price, sources: 5 }];
    let d = digest(&r.e.domain_sep, &oracle_struct_hash(round, time_ms, &prices));
    SignedObservation { signer: signer as u8, time_ms, prices, signature: sign_digest(key_of, &d) }
}

/// R436-12, rule C2d as 432 restated it (README section 5; vectors c2d.json in `r2_regression.rs`): ONE observation
/// whose `timeMs` is outside the block-time window (here at the end of the round, beyond block time +
/// oracleMaxFutureMs) refused the WHOLE update on f0e3967, so one signer of seven, or one feeder clock a second
/// fast, vetoed every round. Fixed: that observation is IGNORED and the round stands on the other six (quorum 5);
/// with fewer than the quorum left the update is refused ORACLE_TIME and the round is NOT consumed, so the same
/// update is accepted at a later block time. A bad signature or a malformed price list stays a refusal of the
/// whole input (facts about the observation alone, which an honest sequencer filters).
#[test]
fn r436_12_one_bad_observation_does_not_veto_the_round() {
    let mut r = Rig::one();
    let keys = oracle_keys(7);
    r.t += 3000;
    r.h += 1;
    let round = r.t / ROUND_MS;
    let start = round * ROUND_MS;
    let t = start + 400;
    let late = start + ROUND_MS - 1;
    let mut obs: Vec<SignedObservation> = (0..6).map(|i| observation(&r, i, round, start, P, &keys[i])).collect();
    obs.push(observation(&r, 6, round, late, P, &keys[6]));
    let out = r.e.apply_block(&Block { height: r.h, time: t, inputs: vec![Input::Oracle(OracleUpdate { round, observations: obs })] }).unwrap();
    r.t = t;
    assert_eq!(out.outcomes, [Ok(())], "late clock: the round stands on six observations");
    assert_eq!((r.e.markets[&1].oracle.index, r.e.markets[&1].oracle.status), (P, Status::Active));
    // three of seven outside the window: four left < quorum 5: refused, nothing moves, the round is not consumed
    r.t += 3000;
    r.h += 1;
    let round = r.t / ROUND_MS;
    let start = round * ROUND_MS;
    let late = start + ROUND_MS - 1;
    let mut obs: Vec<SignedObservation> = (0..4).map(|i| observation(&r, i, round, start, P + TICK, &keys[i])).collect();
    obs.extend((4..7).map(|i| observation(&r, i, round, late, P + TICK, &keys[i])));
    let update = Input::Oracle(OracleUpdate { round, observations: obs });
    let before = r.e.last_oracle_round;
    let out = r.e.apply_block(&Block { height: r.h, time: start + 400, inputs: vec![update.clone()] }).unwrap();
    assert_eq!(out.outcomes, [Err(Error::OracleTime)]);
    assert_eq!((r.e.markets[&1].oracle.index, r.e.last_oracle_round), (P, before));
    // the same update 1.7 s later: every observation is inside the window now
    r.h += 1;
    let out = r.e.apply_block(&Block { height: r.h, time: start + 2100, inputs: vec![update] }).unwrap();
    assert_eq!(out.outcomes, [Ok(())], "the round was not consumed by the refusal");
    assert_eq!(r.e.markets[&1].oracle.index, P + TICK);
}

/// R436-13: a stop-limit / take-profit-limit signed IOC was accepted and entered the book as GTC when it fired
/// (the remainder rested although the user signed immediate-or-cancel). Fixed: types 3 and 5 MUST be GTC; anything
/// else is MALFORMED at placement.
#[test]
fn r436_13_limit_triggers_must_be_gtc() {
    let mut r = Rig::one();
    let ka = key(1);
    let a = addr(&ka);
    let d = r.dep(a, 10_000);
    r.block(3000, &[(1, P)], vec![d]);
    let mk = |r: &Rig, n: u64, ty: u8, tif: u8| {
        let mut x = place(a, r.t + 3000 + n, 1, true, ty, tif, P + 110 * TICK, 100_000, false);
        if let Action::PlaceOrder(p) = &mut x {
            p.trigger_price = P + 100 * TICK;
        }
        sign_action(&r.e, &ka, x)
    };
    let inputs = vec![mk(&r, 0, ORDER_STOP_LIMIT, TIF_IOC), mk(&r, 1, ORDER_TP_LIMIT, TIF_IOC), mk(&r, 2, ORDER_STOP_LIMIT, TIF_GTC), mk(&r, 3, ORDER_STOP_MARKET, TIF_IOC)];
    let out = r.block(3000, &[(1, P)], inputs);
    assert_eq!(out.outcomes[1..], [Err(Error::Malformed), Err(Error::Malformed), Ok(()), Ok(())]);
}

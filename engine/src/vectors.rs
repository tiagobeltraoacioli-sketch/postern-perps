//! Machine-readable vectors produced BY THE ENGINE for the other areas (SPEC sections 15 to 17). UNAUDITED
//! PROTOTYPE; throwaway keys only (`testkit`).
//!
//! - `worked_examples()`: fees, margin, liquidation price, funding, mark and PnL with the exact rounding
//!   (`engine/vectors/worked-examples.json`; the frontend's `lib/calc.ts` and an independent Python checker,
//!   `engine/vectors/check-worked-examples.py`, must reproduce every number).
//! - `proofs_json()`: the 16 checkpoint fields of VAULT.md 5.2, every account leaf with its Merkle proof (escape
//!   claims), every withdrawal record with its proof.
//! - `rehearsal()`: a deterministic 15-block history that reaches deposits, fills, a withdrawal, a forced request,
//!   a liquidation through the backstop and an underwater account; source of the snapshot and proof examples.
use crate::calc;
use crate::engine::{system_address, state_root_of, Engine, ACCOUNT_DEPTH, ACCT_BACKSTOP, ACCT_INSURANCE, WITHDRAWAL_DEPTH};
use crate::genesis::{Checked, Tier};
use crate::hash::{hex0x, keccak};
use crate::merkle;
use crate::num::*;
use crate::testkit::*;
use crate::types::*;
use serde_json::{json, Value};

fn s<T: ToString>(x: T) -> Value {
    Value::String(x.to_string())
}

fn tiers_json(t: &[Tier]) -> Value {
    Value::Array(t.iter().map(|x| json!({"floor": s(x.floor), "imfPpm": s(x.imf_ppm), "mmfPpm": s(x.mmf_ppm)})).collect())
}

/// The BTC-USD tiers of `compliance/markets.json` 0.2.1 (floors in USDC units) and the test market's tiers.
fn tier_sets() -> Vec<(&'static str, Vec<Tier>)> {
    vec![
        (
            "compliance-0.2.1 BTC-USD",
            vec![
                Tier { floor: 0, imf_ppm: 50_000, mmf_ppm: 25_000 },
                Tier { floor: 500_000_000_000, imf_ppm: 100_000, mmf_ppm: 50_000 },
                Tier { floor: 2_000_000_000_000, imf_ppm: 200_000, mmf_ppm: 100_000 },
            ],
        ),
        ("engine test market", vec![Tier { floor: 0, imf_ppm: 20_000, mmf_ppm: 10_000 }, Tier { floor: 1_000_000_000_000, imf_ppm: 40_000, mmf_ppm: 20_000 }]),
    ]
}

/// A small fixed generator (64-bit LCG, constants of Knuth's MMIX) so that the vectors are the same on every run.
struct Lcg(u64);

impl Lcg {
    fn next(&mut self) -> u64 {
        self.0 = self.0.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
        self.0 >> 11
    }
    fn range(&mut self, lo: u64, hi: u64) -> u64 {
        lo + self.next() % (hi - lo + 1)
    }
}

pub fn worked_examples() -> Value {
    let mut g = Lcg(0x430_2026_1118);
    let sets = tier_sets();

    // ---- fees (S-9.3)
    let mut fees = vec![];
    let mut fee_case = |price: u64, size: u64, ppm: u32| {
        let n = calc::notional(price, size);
        fees.push(json!({"price": s(price), "size": s(size), "feePpm": s(ppm), "notional": s(n), "fee": s(calc::trade_fee(n, ppm))}));
    };
    fee_case(6_725_000_000_000, 50_000_000, 450); // SPEC 15
    fee_case(6_725_000_000_000, 50_000_000, 150);
    fee_case(6_725_000_000_000, 50_000_000, 0);
    fee_case(1, 1, 450); // the smallest notional still pays one unit
    fee_case(100_000_000, 100_000_000, 1); // 1 USD at 1 ppm: one millionth of a USDC, exactly one unit
    fee_case(100_000_000, 100_000_001, 1);
    fee_case(MAX_PRICE, MAX_SIZE, 10_000);
    for _ in 0..24 {
        let (p, z, f) = (g.range(1, 9_000_000) * 10_000_000, g.range(1, 5_000_000) * 100, [0u32, 40, 80, 120, 150, 280, 300, 350, 400, 450][g.range(0, 9) as usize]);
        fee_case(p, z, f);
    }

    // ---- margin (6.2)
    let mut margin = vec![];
    for (name, tiers) in &sets {
        let mut case = |price: u64, size: u64, lev: u32| {
            let n = calc::notional(price, size);
            let (ir, mr) = (calc::initial_req(n, tiers, lev), calc::maintenance_req(n, tiers));
            margin.push(json!({
                "tiers": name, "price": s(price), "size": s(size), "leverage": s(lev), "notional": s(n),
                "leverageFractionPpm": s(calc::leverage_fraction_ppm(lev)),
                "initialReqNotional": s(ir), "initialMargin": s(calc::req_usdc(ir)),
                "maintenanceReqNotional": s(mr), "maintenanceMargin": s(calc::req_usdc(mr)),
            }));
        };
        case(6_725_000_000_000, 50_000_000, 10); // SPEC 15
        case(6_725_000_000_000, 50_000_000, 3); // R395-15: ceil(10^6 / 3) = 333334
        case(6_725_000_000_000, 50_000_000, 7);
        case(6_725_000_000_000, 50_000_000, 20);
        case(6_725_000_000_000, 1_000_000_000, 20); // 672,500 USD: crosses the first compliance tier floor
        case(6_725_000_000_000, 3_000_000_000, 5); // 2,017,500 USD: crosses the second
        case(5_000_000_000_000, 1_000_000_000, 20); // exactly 500,000 USD: on a floor
        case(5_000_000_000_000, 1_000_000_001, 20);
        case(1, 1, 1);
        for _ in 0..16 {
            let (p, z, l) = (g.range(1, 9_000_000) * 10_000_000, g.range(1, 40_000_000) * 100, g.range(1, 20) as u32);
            case(p, z, l);
        }
    }

    // ---- liquidation price shown to a user with one cross position (6.2: E < MR, strict)
    let mut liq = vec![];
    for (name, tiers) in &sets {
        let mut case = |balance: i128, size: i64, entry: u64| {
            let open = (size as i128) * (entry as i128);
            let p = calc::liquidation_price_single(balance, size, open, tiers);
            let detail = |px: u64| {
                let e = balance * N_PER_USDC + calc::unrealised(size, open, px);
                let mr = calc::maintenance_req(calc::notional(px, size.unsigned_abs()), tiers);
                json!({"mark": s(px), "equityNotional": s(e), "maintenanceReqNotional": s(mr), "liquidatable": e < mr})
            };
            let (at, beyond) = match p {
                Some(px) => (detail(px), if size > 0 { if px < MAX_PRICE { detail(px + 1) } else { Value::Null } } else if px > 1 { detail(px - 1) } else { Value::Null }),
                None => (Value::Null, Value::Null),
            };
            liq.push(json!({
                "tiers": name, "balance": s(balance), "size": s(size), "entryPrice": s(entry), "openNotional": s(open),
                "liquidationPrice": p.map(s).unwrap_or(Value::Null),
                "atLiquidationPrice": at, "oneUnitOnTheSafeSide": beyond,
            }));
        };
        case(3_500_000_000, 50_000_000, 6_725_000_000_000); // long 0.5 BTC with 3,500 USDC
        case(3_500_000_000, -50_000_000, 6_725_000_000_000); // the same short
        case(40_000_000_000, 50_000_000, 6_725_000_000_000); // fully collateralised long: no liquidation price
        case(100_000_000_000, -1_000_000_000, 6_725_000_000_000); // short 10 BTC: crosses a tier before liquidation
        case(250_000_000_000, 3_000_000_000, 6_725_000_000_000); // long 30 BTC in the upper tiers
        case(0, 100_000, 6_725_000_000_000); // no collateral left: liquidatable above entry? (E = U, MR > 0)
        case(-5_000_000, 100_000, 6_725_000_000_000); // negative balance
        for _ in 0..14 {
            let entry = g.range(100, 9_000_000) * 10_000_000;
            let size = (g.range(1, 20_000_000) * 100) as i64;
            let n_usdc = (calc::notional(entry, size as u64) / N_PER_USDC).max(1);
            let bal = n_usdc * g.range(2, 60) as i128 / 100;
            case(bal, if g.next() % 2 == 0 { size } else { -size }, entry);
        }
    }

    // ---- liquidation execution (6.3 steps 2 and 4): bankruptcy price, price bound, fee
    let mut liq_exec = vec![];
    let mut exec = |size: i64, entry: u64, rest_usdc: i128, mark: u64, mmf0: u32, fee_ppm: u32| {
        let open = (size as i128) * (entry as i128);
        let rest = rest_usdc * N_PER_USDC;
        let bp = calc::bankruptcy_price(size, open, rest);
        liq_exec.push(json!({
            "size": s(size), "openNotional": s(open), "restNotional": s(rest), "mark": s(mark), "mmf0Ppm": s(mmf0), "liquidationFeePpm": s(fee_ppm),
            "bankruptcyPrice": s(bp), "liquidationBound": s(calc::liquidation_bound(size > 0, bp, mark, mmf0)),
            "liquidationFeeBeforeCap": s(calc::liquidation_fee(size.unsigned_abs(), mark, fee_ppm)),
        }));
    };
    exec(10_000_000, 6_725_000_000_000, 700, 6_100_000_000_000, 10_000, 5_000);
    exec(-10_000_000, 6_725_000_000_000, 700, 7_350_000_000_000, 10_000, 5_000);
    exec(10_000_000, 6_725_000_000_000, 700, 5_000_000_000_000, 25_000, 5_000); // gap: bankruptcy above mark
    exec(30_000_001, 6_725_130_000_000, 1_234, 6_400_000_000_000, 25_001, 5_000); // odd numbers: both roundings show
    exec(-30_000_001, 6_725_130_000_000, 1_234, 7_100_000_000_000, 25_001, 5_000);
    exec(1, 1, 0, 1, 10_000, 5_000);

    // ---- funding (section 7)
    let mut rates = vec![];
    let mut rate_case = |samples: Vec<i128>, interest: i64, clamp: i64, max: i64| {
        rates.push(json!({
            "premiumSamples": samples.iter().map(|x| s(*x)).collect::<Vec<_>>(), "interest": s(interest), "fundingClamp": s(clamp), "maxFundingRate": s(max),
            "rate": calc::funding_rate(&samples, interest, clamp, max).map(s).unwrap_or(Value::Null),
        }));
    };
    rate_case(vec![], 12_500, 62_500, 500_000);
    rate_case(vec![0], 12_500, 62_500, 500_000);
    rate_case(vec![900_000, -5, 100_000], 12_500, 62_500, 500_000);
    rate_case(vec![900_000, 900_000], 12_500, 62_500, 500_000);
    rate_case(vec![-900_000, -900_000, 3], 12_500, 62_500, 500_000);
    rate_case(vec![10, 20, 30, 40], 12_500, 62_500, 500_000); // even count: the LOWER middle
    rate_case(vec![-40_000], 12_500, 62_500, 500_000);
    rate_case(vec![200_000], 12_500, 500_000, 500_000);
    let mut pays = vec![];
    let mut pay_case = |size: i64, mark: u64, rate: i128| {
        pays.push(json!({"size": s(size), "mark": s(mark), "rate": s(rate), "credit": calc::funding_credit(size, mark, rate).map(s).unwrap_or(Value::Null)}));
    };
    pay_case(30_000_000, 6_725_130_000_000, 12_345);
    pay_case(-30_000_000, 6_725_130_000_000, 12_345);
    pay_case(30_000_000, 6_725_130_000_000, -12_345);
    pay_case(-30_000_000, 6_725_130_000_000, -12_345);
    pay_case(1, 1, 1); // the smallest payer still pays one unit; the receiver gets nothing
    pay_case(-1, 1, 1);
    pay_case(50_000_000, 6_725_000_000_000, 500_000);
    pay_case(MAX_SIZE as i64, MAX_PRICE, 10_000_000);
    for _ in 0..12 {
        let (z, m, r) = (g.range(1, 20_000_000) as i64 * 100, g.range(100, 9_000_000) * 10_000_000, g.range(0, 1_000_000) as i128 - 500_000);
        pay_case(if g.next() % 2 == 0 { z } else { -z }, m, r);
    }
    let mut prem = vec![];
    let mut prem_case = |ib: Option<u64>, ia: Option<u64>, index: u64| {
        prem.push(json!({"impactBid": ib.map(s).unwrap_or(Value::Null), "impactAsk": ia.map(s).unwrap_or(Value::Null), "index": s(index), "premium": s(calc::premium(ib, ia, index))}));
    };
    prem_case(None, None, 6_725_000_000_000);
    prem_case(Some(6_730_000_000_000), Some(6_735_000_000_000), 6_725_000_000_000); // both above the index: positive
    prem_case(Some(6_715_000_000_000), Some(6_720_000_000_000), 6_725_000_000_000); // both below: negative
    prem_case(Some(6_720_000_000_000), Some(6_730_000_000_000), 6_725_000_000_000); // index inside the spread: zero
    prem_case(Some(6_730_000_000_000), None, 6_725_000_000_000);
    prem_case(None, Some(6_720_000_000_001), 6_725_000_000_000);

    // ---- mark (S-5.2)
    let mut marks = vec![];
    let mut mark_case = |index: u64, bid: Option<u64>, ask: Option<u64>, last: Option<u64>, band: u32| {
        let o = |x: Option<u64>| x.map(s).unwrap_or(Value::Null);
        marks.push(json!({"index": s(index), "bestBid": o(bid), "bestAsk": o(ask), "freshLastTrade": o(last), "markBandBps": s(band), "mark": s(calc::mark_price(index, bid, ask, last, band))}));
    };
    let ix = 6_725_000_000_000;
    mark_case(ix, None, None, None, 50);
    mark_case(ix, Some(ix - 50_000_000), Some(ix + 50_000_000), Some(ix + 20_000_000), 50);
    mark_case(ix, Some(ix + 1_000_000_000_000), None, None, 50); // one high bid alone: the median is the index
    mark_case(ix, Some(ix + 100_000_000_000), Some(ix + 110_000_000_000), None, 50); // book 1.5% above: clamped to +0.5%
    mark_case(ix, Some(ix - 110_000_000_000), Some(ix - 100_000_000_000), Some(ix - 105_000_000_000), 50);
    mark_case(6_725_000_000_001, Some(7_000_000_000_000), Some(7_000_000_000_001), None, 50); // band rounds down
    mark_case(1, Some(5), Some(6), None, 50);

    // ---- unrealised result and what it is worth in USDC (S-0.3: toward the protocol)
    let mut pnl = vec![];
    let mut pnl_case = |size: i64, entry: u64, mark: u64| {
        let open = (size as i128) * (entry as i128);
        let u = calc::unrealised(size, open, mark);
        pnl.push(json!({"size": s(size), "openNotional": s(open), "mark": s(mark), "unrealisedNotional": s(u), "unrealisedUsdcTowardProtocol": s(to_usdc_toward_protocol(u).0)}));
    };
    pnl_case(50_000_000, 6_725_000_000_000, 6_800_000_000_000);
    pnl_case(-50_000_000, 6_725_000_000_000, 6_800_000_000_000);
    pnl_case(30_000_001, 6_725_130_000_000, 6_725_130_000_001);
    pnl_case(-30_000_001, 6_725_130_000_000, 6_725_130_000_001);
    pnl_case(1, 1, 2);

    json!({
        "_note": "UNAUDITED prototype. Worked examples generated by `engine examples` from the engine's own formulas (engine/src/calc.rs, called by the state machine). All integers are decimal strings in engine units (SPEC section 1): price 1e-8 USD, size 1e-8 base, USDC 1e-6, notional 1e-16 USD, rates in ppm, funding rate and premium 1e-9. Signed values use a leading minus. null = no value (no funding, no liquidation price, missing book side).",
        "specVersion": "0.2",
        "scales": {"PRICE_SCALE": s(PRICE_SCALE), "SIZE_SCALE": s(SIZE_SCALE), "USDC_SCALE": s(USDC_SCALE), "NOTIONAL_PER_USDC": s(N_PER_USDC), "PPM": s(PPM), "RATE_SCALE": s(RATE_SCALE), "MAX_PRICE": s(MAX_PRICE), "MAX_SIZE": s(MAX_SIZE)},
        "rounding": {
            "fee": "ceil(notional * feePpm / 10^6 / 10^10): the payer pays at least the exact amount",
            "margin": "sum over tiers of (part of the notional inside the tier) * fraction, then ceil(/10^6) in notional units; in USDC ceil(/10^10); imf' = max(imf, ceil(10^6 / leverage))",
            "liquidationPrice": "long: the highest mark with E < MR; short: the lowest; E = balance * 10^10 + size * mark - openNotional; one price unit beyond it the account is not liquidatable",
            "bankruptcyPrice": "long: ceil((openNotional - rest) / size); short: floor; clamped to [1, MAX_PRICE]; bound = max(bp, mark - floor(mark * floor(mmf0 / 2) / 10^6)) for a long, min(bp, mark + ...) for a short",
            "fundingRate": "P = lower-middle median of the samples; rate = clamp(P + clamp(interest - P, -clamp, +clamp), -max, +max); no samples: none",
            "fundingCredit": "floor(-(size * mark * rate) / 10^19) USDC units credited to the account (negative: it pays, rounded up in size)",
            "premium": "trunc((max(0, impactBid - index) - max(0, index - impactAsk)) * 10^9 / index); a missing side counts as the index",
            "mark": "index + clamp(median(bid, ask, lastTrade) - index, -band, +band), band = floor(index * markBandBps / 10^4); missing values = index",
            "pnl": "unrealised = size * mark - openNotional (exact, notional units); in USDC floor(/10^10) for gains and losses alike (a loss is rounded up in size)",
        },
        "tierSets": sets.iter().map(|(n, t)| json!({"name": n, "tiers": tiers_json(t)})).collect::<Vec<_>>(),
        "fees": fees,
        "margin": margin,
        "liquidationPrice": liq,
        "liquidationExecution": liq_exec,
        "fundingRate": rates,
        "fundingPayment": pays,
        "premium": prem,
        "mark": marks,
        "pnl": pnl,
    })
}

/// The 16 checkpoint fields of VAULT.md 5.2 (names as there), the state root and its parts, every account leaf and
/// every withdrawal record with their Merkle proofs (SPEC 11), from the engine's current state.
pub fn proofs_json(e: &Engine) -> Value {
    let leaves = e.account_leaves();
    let wleaves = e.withdrawal_leaves();
    let accounts_root = merkle::root(&leaves, ACCOUNT_DEPTH);
    let withdrawals_root = merkle::root(&wleaves, WITHDRAWAL_DEPTH);
    let markets_root = e.markets_root();
    let cf = e.checkpoint_fields();
    let esc = e.escape_values();
    let total_escape: u128 = esc.iter().map(|x| *x as u128).sum();
    let aux = e.aux_hash();
    let system_hash = e.system_hash();
    let state_root = state_root_of(e.height, e.time, &accounts_root, &markets_root, &withdrawals_root, &system_hash);
    debug_assert_eq!(state_root, e.state_root());
    let hx = |b: &[u8]| Value::String(hex0x(b));
    let sib = |v: Vec<[u8; 32]>| Value::Array(v.iter().map(|x| hx(x)).collect());
    let accounts: Vec<Value> = (0..e.accounts.len() as u32)
        .map(|ai| {
            let a = &e.accounts[ai as usize];
            let data = e.account_leaf_data(ai);
            json!({
                "account": hx(&a.address), "index": ai, "balance": s(a.balance), "escapeValue": s(esc[ai as usize]),
                "positionsHash": hx(&data[data.len() - 32..]),
                "equityNotional": s(e.equity(ai)),
                "positions": a.positions.iter().map(|(m, p)| json!({"marketId": m, "size": s(p.size), "openNotional": s(p.open_notional)})).collect::<Vec<_>>(),
                "leafData": hx(&data), "leafHash": hx(&leaves[ai as usize]),
                "siblings": sib(merkle::proof(&leaves, ACCOUNT_DEPTH, ai as usize)),
            })
        })
        .collect();
    let withdrawals: Vec<Value> = e
        .withdrawals
        .iter()
        .map(|w| {
            let data = Engine::withdrawal_leaf_data(w);
            json!({
                "withdrawalId": s(w.id), "account": hx(&w.account), "recipient": hx(&w.recipient), "amount": s(w.amount), "height": s(w.height),
                "leafData": hx(&data), "leafHash": hx(&wleaves[w.id as usize]),
                "siblings": sib(merkle::proof(&wleaves, WITHDRAWAL_DEPTH, w.id as usize)),
            })
        })
        .collect();
    json!({
        "_note": "UNAUDITED prototype. Produced by the engine (`engine prove`): SPEC 11 leaves and proofs, VAULT.md 5.2 checkpoint fields. Proof: siblings bottom-up; bit i of the index = 1 means the node is a right child at level i; leaf = keccak256(0x00 || leafData), node = keccak256(0x01 || left || right).",
        "domain": {"name": "PosternPerpsVault (the checkpoint is signed under the VAULT's domain, VAULT.md)", "chainId": s(e.g.g.chain_id), "vault": hx(&e.g.verifying_contract)},
        "checkpoint": {
            "height": s(e.height), "time": s(e.time), "logHash": hx(&e.log_hash),
            "accountsRoot": hx(&accounts_root), "marketsRoot": hx(&markets_root), "withdrawalsRoot": hx(&withdrawals_root),
            "nextDepositIndex": s(cf.deposits_credited), "nextForcedIndex": s(cf.forced_processed), "nextOrderId": s(e.next_order_id),
            "totalEscape": s(total_escape), "dust": s(e.dust), "auxHash": hx(&aux),
            "depositsHead": hx(&cf.deposits_head), "forcedHead": hx(&cf.forced_head),
            "withdrawalCount": s(cf.withdrawal_count), "withdrawalsTotal": s(cf.withdrawals_total),
        },
        "systemHash": hx(&system_hash),
        "stateRoot": hx(&state_root),
        "custody": s(e.total_deposits - e.total_withdrawn),
        "insuranceIndex": ACCT_INSURANCE, "backstopIndex": ACCT_BACKSTOP,
        "accounts": accounts,
        "withdrawals": withdrawals,
    })
}

/// A deterministic history (throwaway keys) for the snapshot and proof examples: three users, deposits, a seeded
/// insurance fund and backstop, trades in two markets, an agent, a resting order and a trigger, a withdrawal, a
/// forced withdrawal, a liquidation through the backstop, and at the end an account under water in a halted
/// market (so the escape values show the insurance-first rule, R395-23).
pub fn rehearsal() -> (Checked, Vec<Block>, Engine) {
    const T0: u64 = 1_794_960_000_000;
    const P: u64 = 6_725_000_000_000;
    const PE: u64 = 348_000_000_000;
    const TICK: u64 = 10_000_000;
    let g = genesis(5, vec![market(1, "BTC-USD"), market(2, "ETH-USD")]);
    let mut e = Engine::new(g.clone());
    let ok = oracle_keys(5);
    let (ka, kb, kc, kg) = (key(11), key(12), key(13), key(14));
    let (a, b, c, ag) = (addr(&ka), addr(&kb), addr(&kc), addr(&kg));
    let mut blocks: Vec<Block> = vec![];
    let mut t = T0;
    let mut dep = 0u64;
    let push = |e: &mut Engine, blocks: &mut Vec<Block>, t: &mut u64, dt: u64, prices: Option<(u64, Option<u64>)>, mut rest: Vec<Input>| {
        *t += dt;
        let mut inputs = vec![];
        if let Some((p1, p2)) = prices {
            let mut ps = vec![(1u32, p1)];
            if let Some(p2) = p2 {
                ps.push((2, p2));
            }
            inputs.push(oracle_round(e, &ok, *t, &ps));
        }
        inputs.append(&mut rest);
        let blk = Block { height: blocks.len() as u64 + 1, time: *t, inputs };
        e.apply_block(&blk).expect("rehearsal block applies");
        crate::invariants::check(e).expect("rehearsal invariants");
        blocks.push(blk);
    };
    let mut d = |account, usdc: u64| {
        dep += 1;
        Input::Deposit { index: dep - 1, account, amount: usdc * 1_000_000 }
    };
    let both = |p1: u64, p2: u64| Some((p1, Some(p2)));
    // 1: deposits
    let deps = vec![d(a, 700), d(b, 60_000), d(c, 5_000), d(system_address(ACCT_INSURANCE), 2_000), d(system_address(ACCT_BACKSTOP), 20_000)];
    push(&mut e, &mut blocks, &mut t, 3000, both(P, PE), deps);
    // 2: a long 0.1 BTC against b
    let n = t + 3000;
    let i2 = vec![
        sign_action(&e, &kb, place(b, n, 1, false, ORDER_LIMIT, TIF_GTC, P, 10_000_000, false)),
        sign_action(&e, &ka, place(a, n, 1, true, ORDER_MARKET, TIF_IOC, P + 100 * TICK, 10_000_000, false)),
    ];
    push(&mut e, &mut blocks, &mut t, 3000, both(P, PE), i2);
    // 3: c long 10 ETH against b; c approves an agent; the agent rests a bid; c stores a stop
    let n = t + 3000;
    let mut stop = place(c, n + 3, 2, false, ORDER_STOP_MARKET, TIF_IOC, PE - 5000 * TICK, 100_000_000, true);
    if let Action::PlaceOrder(p) = &mut stop {
        p.trigger_price = PE - 4000 * TICK;
    }
    let i3 = vec![
        sign_action(&e, &kb, place(b, n, 2, false, ORDER_LIMIT, TIF_GTC, PE, 1_000_000_000, false)),
        sign_action(&e, &kc, place(c, n, 2, true, ORDER_MARKET, TIF_IOC, PE + 100 * TICK, 1_000_000_000, false)),
        sign_action(&e, &kb, place(b, n + 5, 1, false, ORDER_LIMIT, TIF_GTC, P, 1_000_000, false)),
        sign_action(&e, &kc, place(c, n + 5, 1, true, ORDER_MARKET, TIF_IOC, P + 100 * TICK, 1_000_000, false)),
        sign_action(&e, &kc, Action::ApproveAgent { account: c, nonce: n + 1, agent: ag, name: "desk bot".into(), expires_at: n + 86_400_000 }),
        sign_action(&e, &kg, place(c, n + 2, 2, true, ORDER_LIMIT, TIF_GTC, PE - 300 * TICK, 10_000_000, false)),
        sign_action(&e, &kc, stop),
        sign_action(&e, &kb, Action::UpdateLeverage { account: b, nonce: n + 4, market_id: 1, leverage: 5, isolated: false }),
    ];
    push(&mut e, &mut blocks, &mut t, 3000, both(P, PE), i3);
    // 4: b withdraws 1,000 to another address; b requests a forced withdrawal of 250
    let n = t + 3000;
    let mut other = [0x5a; 20];
    other[0] = 0x42;
    let i4 = vec![
        Input::Forced { index: 0, account: b, kind: FORCED_WITHDRAW, deadline: T0 / 1000 + 172_800, arg: 250_000_000 },
        sign_action(&e, &kb, Action::Withdraw { account: b, nonce: n, amount: 1_000_000_000, recipient: other }),
        // resting orders and a trigger that stay to the end: an ETH ask far above, a BTC bid far below, a BTC stop
        // limit buy that never fires
        sign_action(&e, &kb, place(b, n + 1, 2, false, ORDER_LIMIT, TIF_GTC, PE + 3000 * TICK, 50_000_000, false)),
        sign_action(&e, &kb, place(b, n + 2, 1, true, ORDER_LIMIT, TIF_POST_ONLY, P / 2 / TICK * TICK, 1_000_000, false)),
        sign_action(&e, &kb, {
            let mut x = place(b, n + 3, 1, true, ORDER_STOP_LIMIT, TIF_GTC, P * 131 / 100 / TICK * TICK, 1_000_000, false);
            if let Action::PlaceOrder(p) = &mut x {
                p.trigger_price = P * 130 / 100 / TICK * TICK;
            }
            x
        }),
    ];
    push(&mut e, &mut blocks, &mut t, 3000, both(P, PE), i4);
    // 5 to 11: BTC falls 1.5% a round until a is liquidated (the backstop takes the position)
    let mut p = P;
    for _ in 0..7 {
        p = p * 985 / 1000 / TICK * TICK;
        push(&mut e, &mut blocks, &mut t, 3000, both(p, PE), vec![]);
    }
    // 12, 13: ETH's oracle goes dark (BTC rounds continue): ETH halts stale at 13
    push(&mut e, &mut blocks, &mut t, 6000, Some((p, None)), vec![]);
    push(&mut e, &mut blocks, &mut t, 6000, Some((p, None)), vec![]);
    // 14: no oracle round at all
    push(&mut e, &mut blocks, &mut t, 6000, None, vec![]);
    // 15: BTC is now stale and halts; the committee signs ETH 18% lower and ETH resumes there (confirmRounds 1 in
    // the test market). c (long 10 ETH and 0.01 BTC, about 5,000 USDC) is under water at the mark the root commits
    // to and is NOT liquidated while its BTC market is halted (SPEC 6.3): the escape values charge its negative
    // equity to INSURANCE first (R395-23).
    let eth_low = PE * 82 / 100 / TICK * TICK;
    *(&mut t) += 6000;
    let inputs = vec![oracle_round(&e, &ok, t, &[(2, eth_low)])];
    let blk = Block { height: blocks.len() as u64 + 1, time: t, inputs };
    e.apply_block(&blk).expect("rehearsal block applies");
    crate::invariants::check(&e).expect("rehearsal invariants");
    blocks.push(blk);
    (g, blocks, e)
}

/// `engine/vectors/snapshot-v1.json`: the rehearsal's genesis, log and final snapshot (format version 1).
pub fn snapshot_example() -> Value {
    let (g, blocks, e) = rehearsal();
    let snap = e.snapshot();
    let log: Vec<Value> = blocks.iter().map(|b| json!({"height": s(b.height), "time": s(b.time), "inputs": hex0x(&crate::codec::encode_inputs(&b.inputs))})).collect();
    json!({
        "_note": "UNAUDITED prototype. SPEC section 16 snapshot format version 1: the state after the blocks of `log` under `genesis` (throwaway keys; `genesis` is the canonical encoding as a string). `engine resume` and `Engine::restore` must load `snapshot` to `stateRoot`; replaying `log` from `genesis` must give the same root and the same snapshot bytes.",
        "formatVersion": crate::snapshot::VERSION,
        "genesis": String::from_utf8(g.g.encoding()).unwrap(),
        "genesisHash": hex0x(&g.hash),
        "log": log,
        "height": s(e.height),
        "time": s(e.time),
        "stateRoot": hex0x(&e.state_root()),
        "logHash": hex0x(&e.log_hash),
        "snapshotLength": snap.len(),
        "snapshotKeccak256": hex0x(&keccak(&snap)),
        "snapshot": hex0x(&snap),
    })
}

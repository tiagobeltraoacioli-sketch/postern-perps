//! Round 2 (agent 431): the API views and the market-data index over the REAL engine, driven through the
//! sequencer with engine time under the test's control (the engine never reads a clock, so rounds, funding
//! intervals and liquidations are stepped deterministically). THROWAWAY keys only. UNAUDITED PROTOTYPE.

mod common;

use common::*;
use k256::ecdsa::SigningKey;
use perps_node::dev::{dev_genesis_real_with, DevKeys, DevOptions};
use perps_node::engine::real::RealEngine;
use perps_node::engine::view;
use perps_node::genesis::Genesis;
use perps_node::oracle_pool::{observation_digest, Held, OraclePool};
use perps_node::sequencer::{Ack, BlockInfo, Sequencer};
use perps_node::sig::{key_address, sign};
use perps_node::types::{OraclePrice, SignedAction, SignedObservation};
use perps_node::util::{to_hex, Address};
use postern_perps_engine as pe;
use std::sync::Arc;

const BTC: u32 = 1;
/// 67250.0 USD in engine price units (on the 0.5 tick)
const P: u64 = 6_725_000_000_000;
/// 0.14 BTC / 0.5 BTC in engine size units
const S014: u64 = 14_000_000;
const S05: u64 = 50_000_000;

struct Sim {
    g: Arc<Genesis>,
    keys: DevKeys,
    checked: pe::genesis::Checked,
    seq: Sequencer,
    /// engine time of the next block (ms)
    t: u64,
    nonce: u64,
    dir: std::path::PathBuf,
    name: String,
}

fn observation(g: &Genesis, key: &SigningKey, signer: u8, t: u64, prices: &[(u32, u64)]) -> SignedObservation {
    let mut o = SignedObservation { signer, time_ms: t, prices: prices.iter().map(|(m, p)| OraclePrice { market_id: *m, price: *p, sources: 5 }).collect(), signature: [0u8; 65] };
    o.signature = sign(key, &observation_digest(g, t / 3000, &o));
    o
}

impl Sim {
    fn new(name: &str, o: DevOptions) -> Sim {
        let (g, keys, checked) = dev_genesis_real_with(&format!("views-{name}"), 7, 5, 10, 0, &o).unwrap();
        let dir = tmpdir(&format!("views-{name}"));
        let seq = Sequencer::open(g.clone(), keys.sequencer.clone(), Box::new(RealEngine::new(g.clone(), checked.clone()).unwrap()), dir.clone(), view()).unwrap();
        // a fixed start on a round boundary, far from any funding boundary the test does not ask for
        let t = (now() / 3_600_000) * 3_600_000 + 600_000;
        Sim { g, keys, checked, seq, t, nonce: 0, dir, name: name.to_string() }
    }

    fn e(&self) -> &pe::Engine {
        self.seq.engine.real().expect("real engine")
    }

    fn ai(&self, a: &Address) -> u32 {
        self.e().by_addr[a]
    }

    fn deposit(&mut self, index: u64, a: Address, usdc: u64) {
        self.seq.view.lock().unwrap().deposits.insert(index, deposit(index, a, usdc * 1_000_000));
    }

    /// Offers the round of the current time signed by every oracle signer (the ingest path).
    fn offer_round(&mut self, prices: &[(u32, u64)]) {
        let last = self.seq.engine.last_oracle_round();
        for (i, k) in self.keys.oracle.iter().enumerate() {
            let o = observation(&self.g, k, i as u8, self.t, prices);
            self.seq.oracle_pool.offer(&self.g, self.t / 3000, o, last, self.t).unwrap();
        }
    }

    fn produce(&mut self) -> Option<Arc<BlockInfo>> {
        let r = self.seq.produce(self.t + 5).unwrap();
        if let Some(i) = &r {
            assert_eq!(i.block.header.time, self.t + 5, "the test controls engine time");
        }
        r
    }

    /// The next oracle round (3 s later) with these prices, in its own block.
    fn round(&mut self, prices: &[(u32, u64)]) -> Arc<BlockInfo> {
        self.t += 3000;
        self.offer_round(prices);
        self.produce().expect("a block with the oracle round")
    }

    fn act(&mut self, a: SignedAction) -> Ack {
        self.seq.admit_signed(a, self.t + 5).unwrap()
    }

    fn next_nonce(&mut self) -> u64 {
        self.nonce += 1;
        self.t + self.nonce
    }

    fn result(&self, ack: &Ack) -> serde_json::Value {
        self.seq.acks[&ack.action_hash].result.clone().expect("included")
    }

    fn account(&self, a: &Address) -> serde_json::Value {
        self.seq.account_view(a, false).unwrap()
    }

    fn typed(info: &BlockInfo) -> &[pe::Event] {
        &info.outcome.typed
    }
}

#[allow(clippy::too_many_arguments)]
fn order(g: &Genesis, key: &SigningKey, nonce: u64, market: u32, is_buy: bool, order_type: u8, tif: u8, price: u64, size: u64, reduce_only: bool, trigger: u64) -> SignedAction {
    signed(
        g,
        key,
        "PlaceOrder",
        serde_json::json!({"account": to_hex(&key_address(key)), "nonce": nonce.to_string(), "marketId": market, "isBuy": is_buy, "orderType": order_type,
            "timeInForce": tif, "price": price.to_string(), "size": size.to_string(), "reduceOnly": reduce_only, "triggerPrice": trigger.to_string(),
            "clientOrderId": to_hex(&[0u8; 32])}),
    )
}

fn cancel(g: &Genesis, key: &SigningKey, nonce: u64, market: u32, order_id: u64) -> SignedAction {
    signed(g, key, "CancelOrder", serde_json::json!({"account": to_hex(&key_address(key)), "nonce": nonce.to_string(), "marketId": market, "orderId": order_id.to_string()}))
}

/// The engine's public `free_collateral` recomputed from the DISPLAY replica of SPEC 6.2 (valid while every
/// market of the account is active): floor((E - IR) / N), at most the balance, at least 0.
fn replica_free(e: &pe::Engine, ai: u32) -> i128 {
    let (ir, _) = view::account_req(e, ai);
    pe::num::div_floor(e.equity(ai) - ir, pe::num::N_PER_USDC).min(e.accounts[ai as usize].balance).max(0)
}

fn assert_replica_matches_engine(e: &pe::Engine) {
    for ai in 0..e.accounts.len() as u32 {
        assert_eq!(replica_free(e, ai), e.free_collateral(ai), "account #{ai}: SPEC 6.2 replica vs the engine's free_collateral");
    }
}

/// Activates BTC (three agreeing rounds: confirmRounds = 3) and funds A (100,000 USDC) and B (1,000 USDC).
fn start(name: &str, o: DevOptions) -> (Sim, SigningKey, SigningKey) {
    let mut s = Sim::new(name, o);
    let (a, b) = (user(1), user(2));
    s.deposit(0, key_address(&a), 100_000);
    s.deposit(1, key_address(&b), 1_000);
    for _ in 0..3 {
        s.round(&[(BTC, P)]);
    }
    let e = s.e();
    assert_eq!(e.markets[&BTC].oracle.status, pe::oracle::Status::Active, "three agreeing rounds resume the market");
    assert_eq!((e.markets[&BTC].oracle.index, e.markets[&BTC].mark), (P, P));
    (s, a, b)
}

/// A rests an ask of 0.5 BTC at P; B takes 0.14 with an IOC buy. Returns (A's ack, B's ack, the block).
fn trade(s: &mut Sim, a: &SigningKey, b: &SigningKey) -> (Ack, Ack, Arc<BlockInfo>) {
    let n1 = s.next_nonce();
    let ack_a = s.act(order(&s.g.clone(), a, n1, BTC, false, 0, 0, P, S05, false, 0));
    let n2 = s.next_nonce();
    let ack_b = s.act(order(&s.g.clone(), b, n2, BTC, true, 0, 1, P, S014, false, 0));
    s.t += 100;
    let info = s.produce().unwrap();
    (ack_a, ack_b, info)
}

#[test]
fn positions_margin_orders_fills_trades_candles_come_from_the_engine() {
    let (mut s, a, b) = start("trade", DevOptions { funding_interval_ms: Some(1_000_000_000_000), ..Default::default() });
    let (aa, ba) = (key_address(&a), key_address(&b));
    let (ack_a, ack_b, info) = trade(&mut s, &a, &b);
    let ix = s.seq.index.as_ref().unwrap();
    assert_eq!(ix.mismatches, 0, "{:?}", ix.last_mismatch);

    // acknowledgements carry the engine's order ids (recovered from the events; ids start at 1)
    let (ra, rb) = (s.result(&ack_a), s.result(&ack_b));
    assert_eq!((ra["ok"].as_bool(), ra["orderId"].as_str(), ra["orderStatus"].as_str(), ra["filledSize"].as_str()), (Some(true), Some("1"), Some("open"), Some("0.1400")), "{ra}");
    assert_eq!((rb["ok"].as_bool(), rb["orderId"].as_str(), rb["orderStatus"].as_str(), rb["filledSize"].as_str()), (Some(true), Some("2"), Some("filled"), Some("0.1400")), "{rb}");
    // the engine agrees: order 1 rests with 0.36 left, order 2 does not exist any more
    let e = s.e();
    assert_eq!(e.orders[&1].remaining, S05 - S014);
    assert!(!e.orders.contains_key(&2));
    assert_eq!(e.next_order_id, 3);

    // one public trade, one candle per interval
    let (trades, cursor) = ix.trades(BTC, 100, None);
    assert_eq!(trades.len(), 1);
    assert!(cursor.is_none());
    assert_eq!(trades[0], serde_json::json!({"tradeId": "1", "symbol": "BTC-USD", "price": "67250.0", "size": "0.1400", "side": "buy",
        "time": info.block.header.time, "height": info.block.header.height, "liquidation": false}));
    for (i, (_, iv)) in perps_node::index::INTERVALS.iter().enumerate() {
        let c = ix.candles(BTC, i, None, None, 10);
        assert_eq!(c.len(), 1);
        let t = info.block.header.time;
        assert_eq!(c[0], serde_json::json!({"t": t - t % iv, "o": "67250.0", "h": "67250.0", "l": "67250.0", "c": "67250.0", "v": "0.14", "q": "9415.000000", "n": 1}));
    }

    // fills: taker 4.5 bps and maker 1.5 bps of 9,415 USDC, rounded up (SPEC S-9.1), equal to the balance changes
    let (fb, _) = ix.fills(&ba, None, 10, None);
    let (fa, _) = ix.fills(&aa, None, 10, None);
    assert_eq!(fb.len(), 1);
    assert_eq!((fb[0]["fee"].as_str(), fb[0]["liquidity"].as_str(), fb[0]["side"].as_str(), fb[0]["orderId"].as_str(), fb[0]["realizedPnl"].as_str()), (Some("4.236750"), Some("taker"), Some("buy"), Some("2"), Some("0.000000")));
    assert_eq!((fa[0]["fee"].as_str(), fa[0]["liquidity"].as_str(), fa[0]["side"].as_str(), fa[0]["orderId"].as_str()), (Some("1.412250"), Some("maker"), Some("sell"), Some("1")));
    assert_eq!(fa[0]["tradeId"], fb[0]["tradeId"]);
    assert_eq!(e.accounts[s.ai(&ba) as usize].balance, 1_000_000_000 - 4_236_750);
    assert_eq!(e.accounts[s.ai(&aa) as usize].balance, 100_000_000_000 - 1_412_250);
    assert_eq!(e.accounts[pe::engine::ACCT_PROTOCOL_FEE as usize].balance, 4_236_750 + 1_412_250, "fees to the fee account");

    // account views
    let vb = s.account(&ba);
    assert_eq!(vb["collateral"], "995.763250");
    assert_eq!(vb["positions"].as_array().unwrap().len(), 1);
    let pb = &vb["positions"][0];
    assert_eq!((pb["symbol"].as_str(), pb["size"].as_str(), pb["entryPrice"].as_str(), pb["markPrice"].as_str(), pb["notional"].as_str()), (Some("BTC-USD"), Some("0.14"), Some("67250"), Some("67250"), Some("9415.000000")));
    assert_eq!((pb["unrealizedPnl"].as_str(), pb["marginMode"].as_str(), pb["leverage"].as_str()), (Some("0.000000"), Some("cross"), Some("10")));
    // default leverage 10: initial 10% of 9,415; maintenance 2.5% (tier 0)
    assert_eq!((pb["initialMargin"].as_str(), pb["maintenanceMargin"].as_str()), (Some("941.500000"), Some("235.375000")));
    assert_eq!((vb["initialMargin"].as_str(), vb["maintenanceMargin"].as_str(), vb["equity"].as_str()), (Some("941.500000"), Some("235.375000"), Some("995.763250")));
    assert_eq!(vb["freeCollateral"], "54.263250", "995.763250 - 941.5");
    assert_eq!(vb["marginRatio"], "0.236377", "235.375 / 995.76325 rounded up");
    assert_eq!(vb["feeTier"]["takerFeeBps"], "4.5");
    let va = s.account(&aa);
    assert_eq!(va["positions"][0]["size"], "-0.14");
    assert_eq!(va["openOrders"], 1);
    assert_replica_matches_engine(e);

    // orders: A's maker order is open and partly filled; B's taker order is filled
    let (oa, _) = ix.orders(&aa, true, None, 10, None);
    assert_eq!(oa.len(), 1);
    assert_eq!((oa[0]["orderId"].as_str(), oa[0]["status"].as_str(), oa[0]["filledSize"].as_str(), oa[0]["avgFillPrice"].as_str(), oa[0]["type"].as_str(), oa[0]["timeInForce"].as_str(), oa[0]["side"].as_str()),
        (Some("1"), Some("open"), Some("0.1400"), Some("67250"), Some("limit"), Some("GTC"), Some("sell")));
    assert!(ix.orders(&ba, true, None, 10, None).0.is_empty(), "a filled order is not open");
    let (ob, _) = ix.orders(&ba, false, None, 10, None);
    assert_eq!((ob[0]["status"].as_str(), ob[0]["timeInForce"].as_str()), (Some("filled"), Some("IOC")));

    // ticker
    let tk = s.seq.ticker_json(BTC).unwrap();
    assert_eq!((tk["markPrice"].as_str(), tk["indexPrice"].as_str(), tk["lastPrice"].as_str(), tk["openInterest"].as_str(), tk["volume24h"].as_str(), tk["high24h"].as_str()),
        (Some("67250"), Some("67250"), Some("67250"), Some("0.14"), Some("9415.000000"), Some("67250")));

    // A cancels the rest: canceled by the user (no reject reason), nothing open
    let n = s.next_nonce();
    let g = s.g.clone();
    let ack = s.act(cancel(&g, &a, n, BTC, 1));
    s.t += 100;
    s.produce().unwrap();
    assert_eq!(s.result(&ack)["ok"], true);
    let ix = s.seq.index.as_ref().unwrap();
    assert!(ix.orders(&aa, true, None, 10, None).0.is_empty());
    let o1 = ix.order_json(ix.order(1).unwrap());
    assert_eq!((o1["status"].as_str(), o1["rejectReason"].as_str(), o1["filledSize"].as_str()), (Some("canceled"), None, Some("0.1400")));

    // a refused order has no id and creates no order record; an IOC with nothing to hit is canceled NO_LIQUIDITY
    let n = s.next_nonce();
    let bad = s.act(order(&g, &b, n, BTC, true, 0, 0, P + 1, S014, false, 0)); // off the tick
    let n = s.next_nonce();
    let ioc = s.act(order(&g, &b, n, BTC, true, 0, 1, P, 20_000, false, 0));
    s.t += 100;
    s.produce().unwrap();
    assert_eq!((s.result(&bad)["ok"].as_bool(), s.result(&bad)["error"].as_str(), s.result(&bad).get("orderId")), (Some(false), Some("TICK"), None));
    let ri = s.result(&ioc);
    assert_eq!((ri["ok"].as_bool(), ri["orderStatus"].as_str(), ri["filledSize"].as_str()), (Some(true), Some("canceled"), Some("0.0000")), "{ri}");
    let ix = s.seq.index.as_ref().unwrap();
    let id: u64 = ri["orderId"].as_str().unwrap().parse().unwrap();
    assert_eq!(ix.order_json(ix.order(id).unwrap())["rejectReason"], "NO_LIQUIDITY");
    assert_eq!(ix.mismatches, 0, "{:?}", ix.last_mismatch);
    assert_replica_matches_engine(s.e());
}

/// The liquidation estimate is the engine's own boundary: one price unit above it the engine does not liquidate,
/// at it the engine does (the display replica of SPEC 6.2 maintenance cross-checked by the engine's decision).
#[test]
fn liquidation_estimate_is_the_engines_boundary_and_adl_fills_are_indexed() {
    let (mut s, a, b) = start("liq", DevOptions { funding_interval_ms: Some(1_000_000_000_000), ..Default::default() });
    let (aa, ba) = (key_address(&a), key_address(&b));
    trade(&mut s, &a, &b);
    // A cancels its resting ask so the book is empty: mark = index, and the liquidation goes to ADL against A
    let (n, g) = (s.next_nonce(), s.g.clone());
    s.act(cancel(&g, &a, n, BTC, 1));
    s.t += 100;
    s.produce().unwrap();
    let bi = s.ai(&ba);
    let liq = view::liquidation_price(s.e(), bi, BTC).expect("a long position has a liquidation price");
    let shown = s.account(&ba)["positions"][0]["liquidationPrice"].as_str().unwrap().to_string();
    assert_eq!(shown, view::exact8(liq));
    // E = 995.76325 + 0.14 (p - 67250), MR = 0.025 * 0.14 p  =>  p* = (9415 - 995.76325) / (0.14 * 0.975) = 61,679.39...
    assert!((6_167_900_000_000..6_168_000_000_000).contains(&liq), "{liq}");
    // walk the index down inside the 5% jump bound: 64,560 then one unit ABOVE the estimate: no liquidation
    for p in [6_456_000_000_000, liq + 1] {
        let info = s.round(&[(BTC, p)]);
        assert_eq!(s.e().markets[&BTC].mark, p, "empty book: mark = index");
        assert!(!Sim::typed(&info).iter().any(|e| matches!(e, pe::Event::Liquidated { .. })), "no liquidation at {p}");
        assert_eq!(s.e().accounts[bi as usize].positions[&BTC].size, S014 as i64);
        assert_replica_matches_engine(s.e());
    }
    // at the estimate the engine's own test E < MR holds
    let info = s.round(&[(BTC, liq)]);
    let evs = Sim::typed(&info);
    assert!(evs.iter().any(|e| matches!(e, pe::Event::Liquidated { account } if *account == bi)), "{evs:?}");
    assert!(evs.iter().any(|e| matches!(e, pe::Event::Fill { kind: pe::engine::FillKind::Adl, .. })), "no book, no backstop funds: ADL ({evs:?})");
    let e = s.e();
    assert!(e.accounts[bi as usize].positions.is_empty() && e.accounts[s.ai(&aa) as usize].positions.is_empty());
    let ix = s.seq.index.as_ref().unwrap();
    assert_eq!(ix.mismatches, 0, "{:?}", ix.last_mismatch);
    let (fb, _) = ix.fills(&ba, None, 10, None);
    assert_eq!(fb.len(), 2, "the opening fill and the liquidation");
    assert_eq!((fb[0]["liquidation"].as_bool(), fb[0]["side"].as_str(), fb[0]["orderId"].as_str(), fb[0]["liquidity"].as_str()), (Some(true), Some("sell"), Some("0"), Some("taker")));
    let realized: f64 = fb[0]["realizedPnl"].as_str().unwrap().parse().unwrap();
    assert!(realized < -700.0, "B lost most of its margin: {realized}");
    // the ADL counterparty's fill is indexed too, and the trade tape did not grow (ADL is not a book trade)
    let (fa, _) = ix.fills(&aa, None, 10, None);
    assert_eq!((fa[0]["liquidation"].as_bool(), fa[0]["side"].as_str(), fa[0]["liquidity"].as_str()), (Some(true), Some("buy"), Some("maker")));
    assert_eq!(ix.trades(BTC, 10, None).0.len(), 1);
    assert_eq!(s.account(&ba)["positions"].as_array().unwrap().len(), 0);
    // every displayed balance equals the engine's
    assert_eq!(s.account(&ba)["collateral"].as_str().unwrap(), view::usdc(e.accounts[bi as usize].balance));
}

/// Funding over several intervals: every payment the index reports equals the engine's balance change of that
/// account in that block to the last unit (so the step-4 mark replica is the mark the engine used, including a
/// block where the mark is not the index), and the history, position totals and replay digest agree.
#[test]
fn funding_payments_equal_the_engines_balance_changes_over_several_intervals() {
    let (mut s, a, b) = start("funding", DevOptions { funding_interval_ms: Some(60_000), ..Default::default() });
    let (aa, ba) = (key_address(&a), key_address(&b));
    trade(&mut s, &a, &b);
    let (ai, bi) = (s.ai(&aa), s.ai(&ba));
    // the index drops 0.1% while A's ask and the last trade stay at P: median3(bid = index, ask = P, last = P) = P,
    // inside the 0.5% band, so the mark is P and NOT the index
    let p2 = P - 6_725_000_000;
    let mut settlements = 0;
    let mut saw_mark_off_index = false;
    let (mut paid_a, mut paid_b) = (0i128, 0i128);
    for step in 0..75 {
        let (bal_a, bal_b) = (s.e().accounts[ai as usize].balance, s.e().accounts[bi as usize].balance);
        let info = s.round(&[(BTC, p2)]);
        let d = info.delta.as_ref().unwrap();
        let ix = s.seq.index.as_ref().unwrap();
        assert_eq!(ix.mismatches, 0, "{:?}", ix.last_mismatch);
        if d.funding.is_empty() {
            assert_eq!((s.e().accounts[ai as usize].balance, s.e().accounts[bi as usize].balance), (bal_a, bal_b), "step {step}: no settlement, no balance change");
            continue;
        }
        settlements += 1;
        let f = &d.funding[0];
        // interest 0.00125% per interval, premium 0 (no impact-size book): rate = +12500e-9, longs pay shorts
        assert_eq!((f.market, f.rate, f.premium, f.index), (BTC, 12_500, 0, p2));
        saw_mark_off_index |= f.mark != f.index;
        let pay = |addr: &Address| d.payments.iter().find(|(x, _)| x == addr).map(|(_, p)| p.clone()).expect("a payment for each holder");
        let (pa, pb) = (pay(&aa), pay(&ba));
        assert_eq!(pb.payment, s.e().accounts[bi as usize].balance - bal_b, "B: the reported payment is the engine's balance change");
        assert_eq!(pa.payment, s.e().accounts[ai as usize].balance - bal_a, "A: the reported payment is the engine's balance change");
        assert!(pb.payment < 0 && pa.payment > 0, "longs pay, shorts receive");
        assert_eq!((pb.size, pa.size, pb.mark), (S014 as i64, -(S014 as i64), f.mark));
        paid_a += pa.payment;
        paid_b += pb.payment;
        if settlements == 3 {
            break;
        }
    }
    assert_eq!(settlements, 3, "three funding intervals of 60 s in 75 rounds of 3 s");
    assert!(saw_mark_off_index, "at least one settlement used a mark that is not the index");
    let ix = s.seq.index.as_ref().unwrap();
    let (hist, _) = ix.funding_history(BTC, 10, None);
    assert_eq!(hist.len(), 3);
    assert_eq!((hist[0]["rate"].as_str(), hist[0]["premium"].as_str(), hist[0]["symbol"].as_str()), (Some("0.0000125"), Some("0"), Some("BTC-USD")));
    let (pays, cursor) = ix.payments(&ba, None, 2, None);
    assert_eq!(pays.len(), 2);
    let (rest, c2) = ix.payments(&ba, None, 2, cursor.as_deref().map(|c| c.parse().unwrap()));
    assert_eq!((rest.len(), c2), (1, None), "pagination: 2 + 1");
    // the position's fundingSinceOpen is the sum of its payments
    assert_eq!(s.account(&ba)["positions"][0]["fundingSinceOpen"].as_str().unwrap(), view::usdc(paid_b));
    assert_eq!(s.account(&aa)["positions"][0]["fundingSinceOpen"].as_str().unwrap(), view::usdc(paid_a));
    // the predicted rate and the next boundary in the ticker
    let tk = s.seq.ticker_json(BTC).unwrap();
    assert_eq!(tk["fundingRate"], "0.0000125");
    assert_eq!(tk["nextFundingTime"].as_u64().unwrap() % 60_000, 0);

    // the index is a function of the log: a node restarted on the same directory serves the same history
    let digest = ix.digest();
    let (next_trade, next_fill) = (ix.next_trade_id, ix.next_fill_id);
    let (g, key, dir, checked, root) = (s.g.clone(), s.keys.sequencer.clone(), s.dir.clone(), s.checked.clone(), s.seq.engine.state_root());
    let _ = &s.name;
    drop(s);
    let s2 = Sequencer::open(g.clone(), key, Box::new(RealEngine::new(g.clone(), checked).unwrap()), dir, view()).unwrap();
    assert_eq!(s2.engine.state_root(), root);
    let ix2 = s2.index.as_ref().unwrap();
    assert_eq!((ix2.digest(), ix2.next_trade_id, ix2.next_fill_id, ix2.mismatches), (digest, next_trade, next_fill, 0), "index_is_a_function_of_the_log");
}

/// Oracle ingest rules (wire form agreed with 432): pre-filter, one observation per (round, signer), no
/// selection, readiness at n or at q + gather time, and the ENGINE accepts what the pool builds.
#[test]
fn oracle_ingest_pool_rules() {
    let mut s = Sim::new("ingest", DevOptions::default());
    let g = s.g.clone();
    let t = s.t;
    let round = t / 3000;
    let mut pool = OraclePool::default();
    let obs = |i: usize, price: u64| observation(&g, &s.keys.oracle[i], i as u8, t, &[(BTC, price)]);
    // a signature by another key, a wrong signer index, a wrong round, a stale round: refused, nothing held
    let mut forged = obs(0, P);
    forged.signer = 1;
    assert_eq!(pool.offer(&g, round, forged, 0, t).unwrap_err().code, "ORACLE_BAD_SIGNATURE");
    let mut altered = obs(0, P);
    altered.prices[0].price += 1;
    assert_eq!(pool.offer(&g, round, altered, 0, t).unwrap_err().code, "ORACLE_BAD_SIGNATURE");
    let outsider = observation(&g, &user(77), 0, t, &[(BTC, P)]);
    assert_eq!(pool.offer(&g, round, outsider, 0, t).unwrap_err().code, "ORACLE_BAD_SIGNATURE");
    let mut out_of_range = obs(0, P);
    out_of_range.signer = 7;
    assert_eq!(pool.offer(&g, round, out_of_range, 0, t).unwrap_err().code, "ORACLE_MALFORMED");
    assert_eq!(pool.offer(&g, round + 1, obs(0, P), 0, t).unwrap_err().code, "ORACLE_MALFORMED", "round must be timeMs / 3000");
    assert_eq!(pool.offer(&g, round, obs(0, P), round, t).unwrap_err().code, "ORACLE_ROUND_NOT_NEW");
    let future = observation(&g, &s.keys.oracle[0], 0, t + 9000, &[(BTC, P)]);
    assert_eq!(pool.offer(&g, (t + 9000) / 3000, future, 0, t).unwrap_err().code, "ORACLE_TIME");
    assert_eq!((pool.accepted, pool.refused, pool.conflicts), (0, 7, 0));
    assert!(pool.take_ready(&g, 0, t + 60_000).is_none());

    // four signers: held, not ready, however long it waits (below the quorum of 5)
    for i in 0..4 {
        assert_eq!(pool.offer(&g, round, obs(i, P), 0, t).unwrap(), Held::Held(i + 1));
    }
    assert!(pool.take_ready(&g, 0, t + 60_000).is_none(), "below quorum is never ready");
    // the same observation again is idempotent; a DIFFERENT one by the same signer is a conflict, the first stays
    assert_eq!(pool.offer(&g, round, obs(0, P), 0, t).unwrap(), Held::Held(4));
    assert_eq!(pool.offer(&g, round, obs(0, P + 50_000_000), 0, t).unwrap_err().code, "ORACLE_CONFLICT");
    assert_eq!(pool.conflicts, 1);
    // the fifth makes quorum at t + 100: ready only after the gather time (500 ms), so a sixth can still join
    assert_eq!(pool.offer(&g, round, obs(4, P), 0, t + 100).unwrap(), Held::Held(5));
    assert!(pool.take_ready(&g, 0, t + 599).is_none(), "inside the gather window");
    assert_eq!(pool.offer(&g, round, obs(5, P), 0, t + 300).unwrap(), Held::Held(6));
    let u = pool.take_ready(&g, 0, t + 600).expect("ready 500 ms after the quorum");
    assert_eq!(u.observations.iter().map(|o| o.signer).collect::<Vec<_>>(), vec![0, 1, 2, 3, 4, 5], "EVERY held observation, signer ascending (R395-9)");
    assert_eq!(u.observations[0].prices[0].price, P, "the first observation of signer 0 was kept");
    assert!(pool.take_ready(&g, 0, t + 600).is_none(), "a round leaves the pool once built");

    // all seven: ready at once
    let t2 = t + 3000;
    let mut last = Held::Held(0);
    for (i, k) in s.keys.oracle.iter().enumerate() {
        last = pool.offer(&g, t2 / 3000, observation(&g, k, i as u8, t2, &[(BTC, P)]), 0, t2).unwrap();
    }
    assert_eq!(last, Held::Ready(7));
    // an older round arriving after a newer one is ready is dropped when the newer one is taken
    assert!(pool.take_ready(&g, 0, t2).is_some());

    // end to end through the sequencer: the engine accepts the pool's update (C1 to C4 verified by the ENGINE)
    s.t = t2 + 3000;
    s.offer_round(&[(BTC, P)]);
    let info = s.produce().unwrap();
    assert!(info.outcome.outcomes[0].ok, "{:?}", info.outcome.outcomes[0]);
    assert_eq!(s.seq.engine.last_oracle_round(), s.t / 3000);
    // a quorum that disagrees with itself is the ENGINE's call (C4), not the pool's: the pool relays, the engine
    // consumes the round and changes no price
    s.t += 3000;
    let last_round = s.seq.engine.last_oracle_round();
    for (i, k) in s.keys.oracle.iter().enumerate() {
        let price = if i < 3 { P } else { P * 2 };
        s.seq.oracle_pool.offer(&g, s.t / 3000, observation(&g, k, i as u8, s.t, &[(BTC, price)]), last_round, s.t).unwrap();
    }
    let info = s.produce().unwrap();
    assert!(info.outcome.outcomes[0].ok);
    assert_eq!(s.e().markets[&BTC].oracle.index, 0, "no agreeing quorum: the engine set no index");
}

/// Account and withdrawal proofs are the vault's inputs: 32 siblings that rebuild the roots a FINAL checkpoint
/// carries (verified here with the ENGINE crate's own Merkle verifier), for any accepted checkpoint by count.
#[test]
fn account_and_withdrawal_proofs_verify_against_final_checkpoints() {
    let mut n = net_real("proofs", 1);
    let (u, w) = (user(50), user(51));
    let (ua, wa) = (key_address(&u), key_address(&w));
    n.deposit_everywhere(deposit(0, ua, 5_000_000_000));
    n.deposit_everywhere(deposit(1, wa, 7_000_000_000));
    assert!(n.step().iter().all(|r| matches!(r, perps_node::validator::VResult::Applied { checkpoint: Some(_) })));
    let h1 = n.seq.latest_final().expect("final");
    let parse = |v: &serde_json::Value| -> Vec<[u8; 32]> { v.as_array().unwrap().iter().map(|p| perps_node::util::parse_hex_fixed::<32>(p.as_str().unwrap()).unwrap()).collect() };
    let check_account = |seq: &Sequencer, a: &Address, height: Option<u64>, balance: &str| {
        let p = seq.account_proof(a, height).unwrap();
        let body = perps_node::checkpoint::verify_checkpoint_json(&seq.g, &p["checkpoint"]).expect("the proof's checkpoint verifies from the committee alone");
        let leaf = perps_node::util::parse_hex_fixed::<32>(p["leaf"].as_str().unwrap()).unwrap();
        let index: u64 = p["index"].as_str().unwrap().parse().unwrap();
        let sib = parse(&p["proof"]);
        assert_eq!(sib.len(), 32);
        assert!(pe::merkle::verify(&leaf, index, &sib, &body.accounts_root), "leaf + siblings rebuild accountsRoot");
        assert!(!pe::merkle::verify(&leaf, index + 1, &sib, &body.accounts_root));
        assert_eq!((p["leafData"]["account"].as_str().unwrap(), p["leafData"]["balance"].as_str().unwrap()), (to_hex(a).as_str(), balance));
        // the leaf is keccak(0x00 || encoded): what claimEscape recomputes from the Account struct
        let enc = perps_node::util::parse_hex_vec(p["leafData"]["encoded"].as_str().unwrap()).unwrap();
        assert_eq!(pe::merkle::leaf_hash(&enc), leaf);
        assert_eq!(p["leafData"]["escapeValue"].as_str().unwrap(), balance, "no position: escape value = balance");
        body.height
    };
    assert_eq!(check_account(&n.seq, &ua, None, "5000000000"), h1);
    assert_eq!(check_account(&n.seq, &wa, Some(h1), "7000000000"), h1);
    // the system accounts the vault needs in escape (INSURANCE index 1, BACKSTOP index 2) are provable too
    assert_eq!(n.seq.account_proof(&pe::engine::system_address(pe::engine::ACCT_INSURANCE), None).unwrap()["index"], "1");
    assert_eq!(n.seq.account_proof(&pe::engine::system_address(pe::engine::ACCT_BACKSTOP), None).unwrap()["index"], "2");
    assert!(n.seq.account_proof(&key_address(&user(99)), None).is_err(), "an account that does not exist has no proof");
    assert!(n.seq.account_proof(&ua, Some(h1 + 1000)).is_err(), "no proof against a height without a final checkpoint");

    // two withdrawals in two checkpoints
    let t = now();
    n.seq.admit(&request(&n.g, &withdraw(&n.g, &u, ua, t, 1_000_000_000, ua)), t).unwrap();
    n.step();
    let h2 = n.seq.latest_final().unwrap();
    let t = now();
    n.seq.admit(&request(&n.g, &withdraw(&n.g, &w, wa, t, 2_000_000_000, wa)), t).unwrap();
    n.step();
    let h3 = n.seq.latest_final().unwrap();
    assert!(h1 < h2 && h2 < h3);
    let root_at = |h: u64| n.seq.checkpoint_for(h).unwrap().withdrawals_root;
    let rec0 = n.seq.engine.withdrawal(0).unwrap();
    // withdrawal 0 against the checkpoint that first contains it (count 1) and against the later one (count 2)
    for (count, h) in [(1u64, h2), (2, h3)] {
        let p = n.seq.withdrawal_proof(0, Some(count), None).unwrap();
        let sib = parse(&p["siblings"]);
        assert_eq!(sib.len(), 32);
        assert!(pe::merkle::verify(&rec0.leaf(), 0, &sib, &root_at(h)), "count {count}");
        assert_eq!(p["record"]["amount"], "999000000", "net of the 1 USDC fee");
        let by_height = n.seq.withdrawal_proof(0, None, Some(h)).unwrap();
        assert_eq!(by_height["siblings"], p["siblings"]);
        assert_eq!(by_height["withdrawalsRoot"].as_str().unwrap(), to_hex(&root_at(h)));
    }
    // the proof for count 1 does not verify against the root of count 2 (the caller must name the right checkpoint)
    let p1 = parse(&n.seq.withdrawal_proof(0, Some(1), None).unwrap()["siblings"]);
    assert!(!pe::merkle::verify(&rec0.leaf(), 0, &p1, &root_at(h3)));
    assert!(n.seq.withdrawal_proof(1, Some(1), None).is_err(), "withdrawal 1 is not in a tree of one record");
    assert!(n.seq.withdrawal_proof(0, Some(3), None).is_err(), "more records than exist");
    assert!(n.seq.withdrawal_proof(5, None, None).is_err());
    // account state at an OLDER final checkpoint is still provable (the frame of h1 is held): the balance before
    // the withdrawal
    assert_eq!(check_account(&n.seq, &ua, Some(h1), "5000000000"), h1);
    assert_eq!(check_account(&n.seq, &ua, Some(h3), "4000000000"), h3);
    // ?finalized=true reads the state of the latest final checkpoint
    assert_eq!(n.seq.account_view(&ua, true).unwrap()["collateral"], "4000.000000");
}

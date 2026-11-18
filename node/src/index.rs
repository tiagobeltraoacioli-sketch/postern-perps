//! The market-data index: orders, fills, public trades, candles, funding rates and funding payments, derived
//! DETERMINISTICALLY from the applied blocks (the engine's typed events plus the block's own inputs). It is NOT
//! consensus: nothing here enters a state root, a checkpoint or a signature, and a validator needs none of it.
//! The same log gives the same index on every machine (`digest()`; test `index_is_a_function_of_the_log`), so a
//! restarted node serves the same history it served before.
//!
//! What the engine's events do not carry in 0.1.6 and how it is recovered (each is cross-checked every block
//! against the engine's own state; a difference increments `mismatches`, is logged, and the shadow is re-read
//! from the engine, so the index can lag the truth for one block but never drift):
//! - realised PnL per fill: shadow positions updated with the engine's `trade_position` formula;
//! - order ids of the block's PlaceOrder actions: the K highest NEW ids named by a non-liquidation event, K = the
//!   block's accepted PlaceOrders. Proof: (1) the engine allocates ids in execution order: forced closes (before
//!   the first action), then accepted PlaceOrders in input order (exactly one id each, allocated after every
//!   refusal point), then liquidations (after every action); a fired trigger reuses its id; (2) every accepted
//!   PlaceOrder leaves at least one event with its id (TriggerStored, a Fill as taker, Rested or
//!   RemainderCancelled, since a remainder of zero implies a fill); (3) a liquidation id appears only in fills of
//!   kind Liquidation, which are excluded; so the new ids of non-liquidation events are the visible forced ids
//!   followed by all K action ids, and the K highest are the actions', in input order;
//! - the mark a funding settlement used: `view::step4_mark` over the pre-block book and the post-oracle index.
//!
//! Public trades are fills of kind Book and Liquidation (what crossed the book: the engine's own `last_trade`
//! rule). Backstop, ADL and settlement transfers happen at a bound price outside the book: they are in the
//! accounts' fills (and in the block's events), not in the trade tape, the candles or the 24 h statistics.
//!
//! Retention (memory only in this prototype): `TRADES_KEEP` trades per market, `CANDLES_KEEP` candles per
//! series, `FILLS_KEEP` fills, `PAYMENTS_KEEP` funding payments and `ORDERS_KEEP` closed orders per account.
//!
//! UNAUDITED PROTOTYPE.

use crate::actions::{field, field_addr, field_bool, field_u};
use crate::eip712::Val;
use crate::engine::view::{self, PositionExtras};
use crate::engine::InputOutcome;
use crate::genesis::Genesis;
use crate::types::Input;
use crate::util::{keccak, to_hex, Address, H256};
use pe::engine::FillKind;
use pe::num::{div_floor, N_PER_USDC, RATE_SCALE};
use postern_perps_engine as pe;
use std::collections::{BTreeMap, BTreeSet, VecDeque};

pub const INTERVALS: [(&str, u64); 6] = [("1m", 60_000), ("5m", 300_000), ("15m", 900_000), ("1h", 3_600_000), ("4h", 14_400_000), ("1d", 86_400_000)];
pub const TRADES_KEEP: usize = 100_000;
pub const CANDLES_KEEP: usize = 20_000;
pub const FILLS_KEEP: usize = 20_000;
pub const PAYMENTS_KEEP: usize = 10_000;
pub const ORDERS_KEEP: usize = 5_000;
pub const FUNDING_KEEP: usize = 20_000;

pub fn interval_index(name: &str) -> Option<usize> {
    INTERVALS.iter().position(|(n, _)| *n == name)
}

/// What the index needs from the engine state BEFORE a block (small; taken every block).
#[derive(Clone, Debug, Default)]
pub struct PreMarket {
    pub best_bid: Option<u64>,
    pub best_ask: Option<u64>,
    pub last_trade: Option<(u64, u64)>,
    pub samples: Vec<i128>,
}

#[derive(Clone, Debug, Default)]
pub struct PreInfo {
    pub next_order_id: u64,
    pub markets: BTreeMap<u32, PreMarket>,
}

impl PreInfo {
    pub fn of(e: &pe::Engine) -> PreInfo {
        PreInfo {
            next_order_id: e.next_order_id,
            markets: e
                .markets
                .iter()
                .map(|(id, m)| (*id, PreMarket { best_bid: m.book.best_bid(), best_ask: m.book.best_ask(), last_trade: m.last_trade, samples: m.premium_samples.clone() }))
                .collect(),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Trade {
    pub id: u64,
    pub market: u32,
    pub price: u64,
    pub size: u64,
    pub taker_is_buy: bool,
    pub time: u64,
    pub height: u64,
    pub liquidation: bool,
}

pub const KIND_BOOK: u8 = 0;
pub const KIND_LIQUIDATION: u8 = 1;
pub const KIND_BACKSTOP: u8 = 2;
pub const KIND_ADL: u8 = 3;
pub const KIND_SETTLEMENT: u8 = 4;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FillRec {
    pub id: u64,
    pub trade_id: u64,
    pub order_id: u64,
    pub market: u32,
    pub is_buy: bool,
    pub price: u64,
    pub size: u64,
    /// USDC units, positive = paid by the account
    pub fee: i128,
    pub maker: bool,
    /// USDC units actually credited to the balance by this fill (floored, S-0.3)
    pub realized: i128,
    pub liquidation: bool,
    pub kind: u8,
    pub time: u64,
    pub height: u64,
}

pub const ST_UNTRIGGERED: u8 = 0;
pub const ST_OPEN: u8 = 1;
pub const ST_FILLED: u8 = 2;
pub const ST_CANCELED: u8 = 3;
pub const ST_REJECTED: u8 = 4;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct OrderRec {
    pub id: u64,
    pub account: Address,
    pub market: u32,
    pub is_buy: bool,
    pub order_type: u8,
    pub tif: u8,
    pub price: u64,
    pub size: u64,
    pub filled: u64,
    /// sum of price x size of the fills (notional units)
    pub filled_notional: u128,
    pub reduce_only: bool,
    pub trigger_price: u64,
    pub client_order_id: H256,
    pub status: u8,
    pub created_at: u64,
    pub updated_at: u64,
    pub height: u64,
    pub reason: Option<&'static str>,
}

impl OrderRec {
    pub fn is_live(&self) -> bool {
        self.status == ST_UNTRIGGERED || self.status == ST_OPEN
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FundingRec {
    pub market: u32,
    /// the block time of the settlement (the first block at or after the interval boundary)
    pub time: u64,
    pub rate: i128,
    pub premium: i128,
    pub mark: u64,
    pub index: u64,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FundingPay {
    pub seq: u64,
    pub market: u32,
    pub time: u64,
    pub rate: i128,
    pub size: i64,
    pub mark: u64,
    /// USDC units credited to the balance (negative = paid by the account)
    pub payment: i128,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Candle {
    pub t: u64,
    pub o: u64,
    pub h: u64,
    pub l: u64,
    pub c: u64,
    /// base volume, size units
    pub v: u128,
    /// quote volume, notional units
    pub q: u128,
    pub n: u64,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
struct Shadow {
    size: i64,
    open_notional: i128,
    realized: i128,
    funding: i128,
}

#[derive(Clone, Debug, Default)]
struct AccountIdx {
    fills: VecDeque<FillRec>,
    payments: VecDeque<FundingPay>,
    /// every order id of the account, oldest first
    orders: VecDeque<u64>,
    /// agent -> (approvedAt ms, height)
    agents: BTreeMap<Address, (u64, u64)>,
}

/// What one block changed (for the WebSocket channels and the action acknowledgements).
#[derive(Clone, Debug, Default)]
pub struct IndexDelta {
    pub trades: Vec<Trade>,
    pub fills: Vec<(Address, FillRec)>,
    pub funding: Vec<FundingRec>,
    pub payments: Vec<(Address, FundingPay)>,
    pub orders: BTreeSet<u64>,
    /// (market, interval index) series whose last candle changed
    pub candles: BTreeSet<(u32, usize)>,
    pub accounts: BTreeSet<Address>,
    pub markets: BTreeSet<u32>,
    /// input index -> `ActionResult` fields (orderId, orderStatus, filledSize, withdrawalId)
    pub results: BTreeMap<usize, serde_json::Value>,
}

#[derive(Clone, Debug)]
pub struct MarketMeta {
    pub symbol: String,
    pub price_decimals: u32,
    pub size_decimals: u32,
}

#[derive(Clone, Debug, Default)]
pub struct Indexer {
    pub markets: BTreeMap<u32, MarketMeta>,
    trades: BTreeMap<u32, VecDeque<Trade>>,
    candles: BTreeMap<(u32, usize), BTreeMap<u64, Candle>>,
    funding: BTreeMap<u32, VecDeque<FundingRec>>,
    orders: BTreeMap<u64, OrderRec>,
    accounts: BTreeMap<Address, AccountIdx>,
    shadow: BTreeMap<(u32, u32), Shadow>,
    /// forced request index -> kind (0 CancelAll, 1 CloseAll, 2 Withdraw), from the forced inputs
    forced_kinds: BTreeMap<u64, u8>,
    /// forced request index -> the engine's last verdict (served | rejected | pending, reason, height)
    forced_outcomes: BTreeMap<u64, (&'static str, Option<&'static str>, u64)>,
    pub next_trade_id: u64,
    pub next_fill_id: u64,
    pub next_payment_seq: u64,
    /// last public trade per market (price, time)
    last: BTreeMap<u32, (u64, u64)>,
    pub height: u64,
    pub time: u64,
    /// Blocks in which a recovered value differed from the engine's state (0 in every test).
    pub mismatches: u64,
    pub last_mismatch: Option<String>,
    /// First height this index covers (1 unless the node recovered from a snapshot without an index).
    pub from_height: u64,
}

/// The engine's `trade_position` on a shadow position; returns the realised amount in notional units.
fn shadow_trade(pos: &mut Shadow, d: i128, p: i128) -> i128 {
    let (s, n) = (pos.size as i128, pos.open_notional);
    let mut realized = 0;
    if s == 0 || (s > 0) == (d > 0) {
        pos.open_notional = n + d * p;
        pos.size = (s + d) as i64;
    } else {
        let closed = d.abs().min(s.abs());
        let k = n * closed / s.abs();
        let c_signed = closed * s.signum();
        realized = c_signed * p - k;
        let mut new_n = n - k;
        let mut new_s = s - c_signed;
        let rem = d.abs() - closed;
        if rem > 0 {
            new_n += d.signum() * rem * p;
            new_s = d.signum() * rem;
        }
        pos.size = new_s as i64;
        pos.open_notional = new_n;
    }
    realized
}

fn field_h256(g: &Genesis, primary: &str, msg: &Val, name: &str) -> Option<H256> {
    match field(g, primary, msg, name)? {
        Val::Bytes32(h) => Some(*h),
        _ => None,
    }
}

fn order_type_name(t: u8) -> &'static str {
    match t {
        0 => "limit",
        1 => "market",
        2 => "stopMarket",
        3 => "stopLimit",
        4 => "takeProfitMarket",
        _ => "takeProfitLimit",
    }
}

fn tif_name(t: u8) -> &'static str {
    match t {
        0 => "GTC",
        1 => "IOC",
        _ => "POST_ONLY",
    }
}

fn status_name(s: u8) -> &'static str {
    match s {
        ST_UNTRIGGERED => "untriggered",
        ST_OPEN => "open",
        ST_FILLED => "filled",
        ST_CANCELED => "canceled",
        _ => "rejected",
    }
}

impl Indexer {
    pub fn new(g: &Genesis) -> Indexer {
        Indexer {
            markets: g.markets.iter().map(|m| (m.market_id, MarketMeta { symbol: m.symbol.clone(), price_decimals: m.price_decimals, size_decimals: m.size_decimals })).collect(),
            next_trade_id: 1,
            next_fill_id: 1,
            next_payment_seq: 1,
            from_height: 1,
            ..Default::default()
        }
    }

    fn meta(&self, mid: u32) -> MarketMeta {
        self.markets.get(&mid).cloned().unwrap_or(MarketMeta { symbol: format!("M{mid}-USD"), price_decimals: 8, size_decimals: 8 })
    }

    pub fn market_id(&self, symbol: &str) -> Option<u32> {
        self.markets.iter().find(|(_, m)| m.symbol == symbol).map(|(id, _)| *id)
    }

    /// Re-reads every shadow position from the engine (start of an index that does not begin at genesis, and the
    /// repair after a mismatch). Realised PnL and funding since open restart at zero for positions not known.
    pub fn resync_shadow(&mut self, e: &pe::Engine) {
        let mut next = BTreeMap::new();
        for (ai, a) in e.accounts.iter().enumerate() {
            for (mid, p) in &a.positions {
                if p.size == 0 {
                    continue;
                }
                let key = (ai as u32, *mid);
                let old = self.shadow.get(&key);
                next.insert(key, Shadow { size: p.size, open_notional: p.open_notional, realized: old.map(|o| o.realized).unwrap_or(0), funding: old.map(|o| o.funding).unwrap_or(0) });
            }
        }
        self.shadow = next;
    }

    fn check_shadow(&mut self, e: &pe::Engine, height: u64) {
        let mut bad: Option<String> = None;
        let mut count = 0usize;
        for (ai, a) in e.accounts.iter().enumerate() {
            for (mid, p) in &a.positions {
                if p.size == 0 {
                    continue;
                }
                count += 1;
                match self.shadow.get(&(ai as u32, *mid)) {
                    Some(s) if s.size == p.size && s.open_notional == p.open_notional => {}
                    other => {
                        bad.get_or_insert_with(|| format!("height {height}: account #{ai} market {mid}: engine ({}, {}) index {:?}", p.size, p.open_notional, other.map(|s| (s.size, s.open_notional))));
                    }
                }
            }
        }
        if bad.is_none() && count != self.shadow.len() {
            bad = Some(format!("height {height}: the index holds {} positions, the engine {count}", self.shadow.len()));
        }
        if let Some(b) = bad {
            eprintln!("index: shadow position mismatch, re-read from the engine: {b}");
            self.mismatches += 1;
            self.last_mismatch = Some(b);
            self.resync_shadow(e);
        }
    }

    fn push_fill(&mut self, d: &mut IndexDelta, addr: Address, f: FillRec) {
        let a = self.accounts.entry(addr).or_default();
        a.fills.push_back(f.clone());
        while a.fills.len() > FILLS_KEEP {
            a.fills.pop_front();
        }
        d.accounts.insert(addr);
        d.fills.push((addr, f));
    }

    fn add_candles(&mut self, d: &mut IndexDelta, t: &Trade) {
        for (i, (_, iv)) in INTERVALS.iter().enumerate() {
            let bucket = t.time - t.time % iv;
            let series = self.candles.entry((t.market, i)).or_default();
            let c = series.entry(bucket).or_insert(Candle { t: bucket, o: t.price, h: t.price, l: t.price, c: t.price, v: 0, q: 0, n: 0 });
            c.h = c.h.max(t.price);
            c.l = c.l.min(t.price);
            c.c = t.price;
            c.v += t.size as u128;
            c.q += (t.size as u128) * (t.price as u128);
            c.n += 1;
            while series.len() > CANDLES_KEEP {
                let k = *series.keys().next().unwrap();
                series.remove(&k);
            }
            d.candles.insert((t.market, i));
        }
    }

    fn touch_order(&mut self, d: &mut IndexDelta, id: u64, time: u64, f: impl FnOnce(&mut OrderRec)) {
        if let Some(o) = self.orders.get_mut(&id) {
            f(o);
            o.updated_at = time;
            d.orders.insert(id);
            d.accounts.insert(o.account);
        }
    }

    /// Applies one block. `pre` is `PreInfo::of` the engine before the block, `post` the engine after it.
    #[allow(clippy::too_many_arguments)]
    pub fn apply(&mut self, g: &Genesis, pre: &PreInfo, post: &pe::Engine, height: u64, time: u64, inputs: &[Input], outcomes: &[InputOutcome], events: &[pe::Event]) -> IndexDelta {
        let mut d = IndexDelta::default();
        self.height = height;
        self.time = time;

        // 1. the block's accepted actions
        struct Placed {
            input: usize,
            account: Address,
            market: u32,
            is_buy: bool,
            order_type: u8,
            tif: u8,
            price: u64,
            size: u64,
            reduce_only: bool,
            trigger_price: u64,
            coid: H256,
        }
        let mut placed: Vec<Placed> = Vec::new();
        let mut withdraw_inputs: Vec<(usize, Address)> = Vec::new();
        for (i, (inp, o)) in inputs.iter().zip(outcomes).enumerate() {
            match inp {
                Input::Forced(f) => {
                    self.forced_kinds.insert(f.index, f.kind as u8);
                    d.accounts.insert(f.account);
                }
                Input::Deposit(dep) => {
                    d.accounts.insert(dep.account);
                }
                Input::Action(a) => {
                    let Some(name) = g.types.name(a.type_index) else { continue };
                    let Some(account) = field_addr(g, name, &a.message, "account") else { continue };
                    d.accounts.insert(account);
                    if !o.ok {
                        continue;
                    }
                    let u = |k: &str| field_u(g, name, &a.message, k).unwrap_or(0);
                    match name {
                        "PlaceOrder" => placed.push(Placed {
                            input: i,
                            account,
                            market: u("marketId") as u32,
                            is_buy: field_bool(g, name, &a.message, "isBuy").unwrap_or(false),
                            order_type: u("orderType") as u8,
                            tif: u("timeInForce") as u8,
                            price: u("price") as u64,
                            size: u("size") as u64,
                            reduce_only: field_bool(g, name, &a.message, "reduceOnly").unwrap_or(false),
                            trigger_price: u("triggerPrice") as u64,
                            coid: field_h256(g, name, &a.message, "clientOrderId").unwrap_or([0u8; 32]),
                        }),
                        "Withdraw" => withdraw_inputs.push((i, account)),
                        "ApproveAgent" => {
                            if let Some(agent) = field_addr(g, name, &a.message, "agent") {
                                self.accounts.entry(account).or_default().agents.insert(agent, (time, height));
                            }
                        }
                        "RevokeAgent" => {
                            if let Some(agent) = field_addr(g, name, &a.message, "agent") {
                                self.accounts.entry(account).or_default().agents.remove(&agent);
                            }
                        }
                        _ => {}
                    }
                }
                Input::Oracle(_) => {}
            }
        }

        // 2. order ids of the accepted PlaceOrders (module doc: the K highest new non-liquidation ids)
        let mut visible: BTreeSet<u64> = BTreeSet::new();
        for ev in events {
            let id = match ev {
                pe::Event::Fill { kind, taker_order, .. } if *kind != FillKind::Liquidation => *taker_order,
                pe::Event::Rested { order } | pe::Event::TriggerStored { order } => *order,
                pe::Event::RemainderCancelled { order, .. } => *order,
                _ => 0,
            };
            if id >= pre.next_order_id && id != 0 {
                visible.insert(id);
            }
        }
        let ids: Vec<u64> = visible.iter().rev().take(placed.len()).rev().copied().collect();
        let contiguous = ids.windows(2).all(|w| w[1] == w[0] + 1);
        if ids.len() != placed.len() || !contiguous || ids.last().is_some_and(|l| *l >= post.next_order_id) {
            self.mismatches += 1;
            let m = format!("height {height}: {} accepted PlaceOrders, new ids seen {:?}", placed.len(), ids);
            eprintln!("index: order id recovery mismatch: {m}");
            self.last_mismatch = Some(m);
        }
        for (p, id) in placed.iter().zip(&ids) {
            let rec = OrderRec {
                id: *id,
                account: p.account,
                market: p.market,
                is_buy: p.is_buy,
                order_type: p.order_type,
                tif: p.tif,
                price: p.price,
                size: p.size,
                filled: 0,
                filled_notional: 0,
                reduce_only: p.reduce_only,
                trigger_price: p.trigger_price,
                client_order_id: p.coid,
                // provisional until the block's events say otherwise (a taker filled in full has only fills)
                status: ST_OPEN,
                created_at: time,
                updated_at: time,
                height,
                reason: None,
            };
            self.orders.insert(*id, rec);
            let a = self.accounts.entry(p.account).or_default();
            a.orders.push_back(*id);
            d.orders.insert(*id);
        }

        // 3. the engine's events, in execution order
        let addr_of = |ai: u32| post.accounts.get(ai as usize).map(|a| a.address).unwrap_or([0u8; 20]);
        let mut liquidating: BTreeSet<u32> = BTreeSet::new();
        let mut forced_withdrawals = 0usize;
        let mut withdrawal_ids: Vec<(u64, Address)> = Vec::new();
        for ev in events {
            match ev {
                pe::Event::Fill { market, kind, taker, maker, taker_order, maker_order, taker_is_buy, price, size, taker_fee, maker_fee, .. } => {
                    let q = *size as i128;
                    let p = *price as i128;
                    let dsz = if *taker_is_buy { q } else { -q };
                    let tr = {
                        let s = self.shadow.entry((*taker, *market)).or_default();
                        let was = s.size;
                        let r = div_floor(shadow_trade(s, dsz, p), N_PER_USDC);
                        if was != 0 && (s.size == 0 || (s.size > 0) != (was > 0)) {
                            s.realized = 0;
                            s.funding = 0;
                        } else {
                            s.realized += r;
                        }
                        r
                    };
                    let mr = {
                        let s = self.shadow.entry((*maker, *market)).or_default();
                        let was = s.size;
                        let r = div_floor(shadow_trade(s, -dsz, p), N_PER_USDC);
                        if was != 0 && (s.size == 0 || (s.size > 0) != (was > 0)) {
                            s.realized = 0;
                            s.funding = 0;
                        } else {
                            s.realized += r;
                        }
                        r
                    };
                    for k in [(*taker, *market), (*maker, *market)] {
                        if self.shadow.get(&k).is_some_and(|s| s.size == 0) {
                            self.shadow.remove(&k);
                        }
                    }
                    let k = match kind {
                        FillKind::Book => KIND_BOOK,
                        FillKind::Liquidation => KIND_LIQUIDATION,
                        FillKind::Backstop => KIND_BACKSTOP,
                        FillKind::Adl => KIND_ADL,
                    };
                    let liq = *kind == FillKind::Liquidation || (k != KIND_BOOK && liquidating.contains(taker));
                    let trade_id = self.next_trade_id;
                    self.next_trade_id += 1;
                    if k == KIND_BOOK || k == KIND_LIQUIDATION {
                        let t = Trade { id: trade_id, market: *market, price: *price, size: *size, taker_is_buy: *taker_is_buy, time, height, liquidation: liq };
                        let tape = self.trades.entry(*market).or_default();
                        tape.push_back(t.clone());
                        while tape.len() > TRADES_KEEP {
                            tape.pop_front();
                        }
                        self.last.insert(*market, (*price, time));
                        self.add_candles(&mut d, &t);
                        d.trades.push(t);
                    }
                    d.markets.insert(*market);
                    for (ai, order, is_buy, fee, maker_side, realized) in [(*taker, *taker_order, *taker_is_buy, *taker_fee, false, tr), (*maker, *maker_order, !*taker_is_buy, *maker_fee, true, mr)] {
                        let id = self.next_fill_id;
                        self.next_fill_id += 1;
                        let f = FillRec { id, trade_id, order_id: order, market: *market, is_buy, price: *price, size: *size, fee, maker: maker_side, realized, liquidation: liq, kind: k, time, height };
                        self.push_fill(&mut d, addr_of(ai), f);
                        if order != 0 {
                            let (sz, pr) = (*size, *price);
                            self.touch_order(&mut d, order, time, |o| {
                                o.filled = o.filled.saturating_add(sz);
                                o.filled_notional += (sz as u128) * (pr as u128);
                                if o.filled >= o.size {
                                    o.status = ST_FILLED;
                                }
                            });
                        }
                    }
                }
                pe::Event::Rested { order } => self.touch_order(&mut d, *order, time, |o| o.status = ST_OPEN),
                pe::Event::TriggerStored { order } => self.touch_order(&mut d, *order, time, |o| o.status = ST_UNTRIGGERED),
                pe::Event::Triggered { order } => self.touch_order(&mut d, *order, time, |o| o.status = ST_OPEN),
                pe::Event::Cancelled { order, reason } => {
                    let r = *reason;
                    self.touch_order(&mut d, *order, time, |o| {
                        o.status = ST_CANCELED;
                        o.reason = if r == pe::Error::Cancelled { None } else { Some(r.name()) };
                    })
                }
                pe::Event::RemainderCancelled { order, reason, .. } => {
                    let r = *reason;
                    self.touch_order(&mut d, *order, time, |o| {
                        // a taker whose remainder did not rest: canceled with the engine's reason; what filled stays
                        o.status = ST_CANCELED;
                        o.reason = Some(r.name());
                    })
                }
                pe::Event::Funding { market, rate, .. } => {
                    let pm = pre.markets.get(market).cloned().unwrap_or_default();
                    let (index, band) = post.markets.get(market).map(|m| (m.oracle.index, m.p.mark_band_bps)).unwrap_or((0, 0));
                    let mark = view::step4_mark(index, pm.best_bid, pm.best_ask, pm.last_trade, band, time);
                    let premium = post.markets.get(market).and_then(|m| view::funding_rate_of(&pm.samples, &m.p)).map(|x| x.1).unwrap_or(0);
                    let rec = FundingRec { market: *market, time, rate: *rate, premium, mark, index };
                    let hist = self.funding.entry(*market).or_default();
                    hist.push_back(rec.clone());
                    while hist.len() > FUNDING_KEEP {
                        hist.pop_front();
                    }
                    d.funding.push(rec);
                    d.markets.insert(*market);
                    let holders: Vec<(u32, i64)> = self.shadow.iter().filter(|((_, m), s)| m == market && s.size != 0).map(|((ai, _), s)| (*ai, s.size)).collect();
                    for (ai, size) in holders {
                        // the engine's S-7.3 credit: floor(-(size x mark x rate) / (RATE_SCALE x N))
                        let x = (size as i128).saturating_mul(mark as i128).saturating_mul(*rate);
                        let payment = div_floor(-x, RATE_SCALE * N_PER_USDC);
                        if let Some(s) = self.shadow.get_mut(&(ai, *market)) {
                            s.funding += payment;
                        }
                        let seq = self.next_payment_seq;
                        self.next_payment_seq += 1;
                        let pay = FundingPay { seq, market: *market, time, rate: *rate, size, mark, payment };
                        let addr = addr_of(ai);
                        let a = self.accounts.entry(addr).or_default();
                        a.payments.push_back(pay.clone());
                        while a.payments.len() > PAYMENTS_KEEP {
                            a.payments.pop_front();
                        }
                        d.accounts.insert(addr);
                        d.payments.push((addr, pay));
                    }
                }
                pe::Event::MarketSettled { market, price } => {
                    // X3: every position of the market closed at the settle price (no counterparty, no fee)
                    let holders: Vec<(u32, i64)> = self.shadow.iter().filter(|((_, m), s)| m == market && s.size != 0).map(|((ai, _), s)| (*ai, s.size)).collect();
                    let trade_id = self.next_trade_id;
                    self.next_trade_id += 1;
                    for (ai, size) in holders {
                        let mut s = self.shadow.remove(&(ai, *market)).unwrap_or_default();
                        let realized = div_floor(shadow_trade(&mut s, -(size as i128), *price as i128), N_PER_USDC);
                        let id = self.next_fill_id;
                        self.next_fill_id += 1;
                        let f = FillRec { id, trade_id, order_id: 0, market: *market, is_buy: size < 0, price: *price, size: size.unsigned_abs(), fee: 0, maker: false, realized, liquidation: false, kind: KIND_SETTLEMENT, time, height };
                        self.push_fill(&mut d, addr_of(ai), f);
                    }
                    d.markets.insert(*market);
                }
                pe::Event::Liquidated { account } => {
                    liquidating.insert(*account);
                    d.accounts.insert(addr_of(*account));
                }
                pe::Event::ForcedServed { index } => {
                    if self.forced_kinds.get(index) == Some(&2) {
                        forced_withdrawals += 1;
                    }
                    self.forced_outcomes.insert(*index, ("served", None, height));
                }
                pe::Event::ForcedRejected { index, reason } => {
                    self.forced_outcomes.insert(*index, ("rejected", Some(reason.name()), height));
                }
                pe::Event::ForcedPending { index, reason } => {
                    self.forced_outcomes.insert(*index, ("pending", Some(reason.name()), height));
                }
                pe::Event::Withdrawal(w) => {
                    withdrawal_ids.push((w.id, w.account));
                    d.accounts.insert(w.account);
                }
                pe::Event::Deposit { account, .. } => {
                    d.accounts.insert(*account);
                }
                pe::Event::InsuranceCover { account, .. } | pe::Event::LiquidationFee { account, .. } | pe::Event::BadDebt { account, .. } => {
                    d.accounts.insert(addr_of(*account));
                }
                pe::Event::Halt { market } | pe::Event::Resume { market } => {
                    d.markets.insert(*market);
                }
                pe::Event::Launched => {}
                // an event kind of a newer engine: nothing to index until this file learns it
                #[allow(unreachable_patterns)]
                _ => {}
            }
        }

        // 4. acknowledgement fields: withdrawals of the block are forced ones first, then the Withdraw actions in
        //    input order, then fee sweeps (the engine's execution order)
        for ((input, account), (id, wacc)) in withdraw_inputs.iter().zip(withdrawal_ids.iter().skip(forced_withdrawals)) {
            if account == wacc {
                d.results.insert(*input, serde_json::json!({"withdrawalId": id.to_string()}));
            } else {
                self.mismatches += 1;
                self.last_mismatch = Some(format!("height {height}: withdrawal {id} does not belong to the Withdraw action at input {input}"));
            }
        }
        for (p, id) in placed.iter().zip(&ids) {
            if let Some(o) = self.orders.get(id) {
                let sd = self.meta(o.market).size_decimals;
                d.results.insert(p.input, serde_json::json!({"orderId": id.to_string(), "orderStatus": status_name(o.status), "filledSize": view::grid(o.filled, sd)}));
            }
        }

        // 5. retention of closed orders, then the cross-check against the engine
        let touched: Vec<Address> = placed.iter().map(|p| p.account).collect();
        for acc in touched {
            let Some(a) = self.accounts.get_mut(&acc) else { continue };
            while a.orders.len() > ORDERS_KEEP {
                let Some(pos) = a.orders.iter().position(|id| self.orders.get(id).is_none_or(|o| !o.is_live())) else { break };
                if let Some(id) = a.orders.remove(pos) {
                    self.orders.remove(&id);
                }
            }
        }
        self.check_shadow(post, height);
        d
    }

    // ------------------------------------------------------------------------------------ JSON views

    pub fn extras(&self, ai: u32, mid: u32) -> PositionExtras {
        self.shadow.get(&(ai, mid)).map(|s| PositionExtras { realized_pnl: s.realized, funding: s.funding }).unwrap_or_default()
    }

    pub fn trade_json(&self, t: &Trade) -> serde_json::Value {
        let m = self.meta(t.market);
        serde_json::json!({"tradeId": t.id.to_string(), "symbol": m.symbol, "price": view::grid(t.price, m.price_decimals),
            "size": view::grid(t.size, m.size_decimals), "side": if t.taker_is_buy {"buy"} else {"sell"}, "time": t.time,
            "height": t.height, "liquidation": t.liquidation})
    }

    pub fn fill_json(&self, account: &Address, f: &FillRec) -> serde_json::Value {
        let m = self.meta(f.market);
        serde_json::json!({"fillId": f.id.to_string(), "tradeId": f.trade_id.to_string(), "orderId": f.order_id.to_string(),
            "account": to_hex(account), "symbol": m.symbol, "side": if f.is_buy {"buy"} else {"sell"},
            "price": view::grid(f.price, m.price_decimals), "size": view::grid(f.size, m.size_decimals), "fee": view::usdc(f.fee),
            "liquidity": if f.maker {"maker"} else {"taker"}, "realizedPnl": view::usdc(f.realized), "liquidation": f.liquidation,
            "time": f.time, "height": f.height})
    }

    pub fn order_json(&self, o: &OrderRec) -> serde_json::Value {
        let m = self.meta(o.market);
        let avg = if o.filled > 0 { Some(view::exact8((o.filled_notional / o.filled as u128).min(u64::MAX as u128) as u64)) } else { None };
        let stop = o.order_type == 2 || o.order_type == 3;
        let cond = if o.order_type < 2 {
            None
        } else if (stop && o.is_buy) || (!stop && !o.is_buy) {
            Some("markAbove")
        } else {
            Some("markBelow")
        };
        serde_json::json!({"orderId": o.id.to_string(),
            "clientOrderId": if o.client_order_id == [0u8; 32] { serde_json::Value::Null } else { to_hex(&o.client_order_id).into() },
            "account": to_hex(&o.account), "symbol": m.symbol, "side": if o.is_buy {"buy"} else {"sell"},
            "type": order_type_name(o.order_type), "timeInForce": tif_name(o.tif), "price": view::grid(o.price, m.price_decimals),
            "size": view::grid(o.size, m.size_decimals), "filledSize": view::grid(o.filled, m.size_decimals), "avgFillPrice": avg,
            "reduceOnly": o.reduce_only,
            "triggerPrice": if o.order_type >= 2 { Some(view::grid(o.trigger_price, m.price_decimals)) } else { None },
            "triggerCondition": cond, "status": status_name(o.status), "createdAt": o.created_at, "updatedAt": o.updated_at,
            "height": o.height, "rejectReason": o.reason})
    }

    pub fn order(&self, id: u64) -> Option<&OrderRec> {
        self.orders.get(&id)
    }

    pub fn payment_json(&self, p: &FundingPay) -> serde_json::Value {
        let m = self.meta(p.market);
        serde_json::json!({"symbol": m.symbol, "time": p.time, "rate": view::rate9(p.rate), "positionSize": view::exact8_signed(p.size),
            "markPrice": view::exact8(p.mark), "payment": view::usdc(p.payment)})
    }

    pub fn funding_json(&self, f: &FundingRec) -> serde_json::Value {
        let m = self.meta(f.market);
        serde_json::json!({"symbol": m.symbol, "time": f.time, "rate": view::rate9(f.rate), "premium": view::rate9(f.premium),
            "markPrice": view::exact8(f.mark), "indexPrice": view::exact8(f.index)})
    }

    pub fn candle_json(&self, market: u32, c: &Candle) -> serde_json::Value {
        let m = self.meta(market);
        let g = |v: u64| view::grid(v, m.price_decimals);
        serde_json::json!({"t": c.t, "o": g(c.o), "h": g(c.h), "l": g(c.l), "c": g(c.c),
            "v": crate::engine::real::trim(crate::util::scaled_to_decimal(c.v, 8)),
            "q": view::usdc((c.q / N_PER_USDC as u128) as i128), "n": c.n})
    }

    /// Newest first; `before` is an exclusive trade id.
    pub fn trades(&self, market: u32, limit: usize, before: Option<u64>) -> (Vec<serde_json::Value>, Option<String>) {
        let Some(tape) = self.trades.get(&market) else { return (Vec::new(), None) };
        let mut it = tape.iter().rev().filter(|t| before.is_none_or(|b| t.id < b));
        let out: Vec<&Trade> = it.by_ref().take(limit).collect();
        let more = it.next().is_some();
        let cursor = if more { out.last().map(|t| t.id.to_string()) } else { None };
        (out.into_iter().map(|t| self.trade_json(t)).collect(), cursor)
    }

    /// Trades of one block (oldest first), for the WebSocket `trades` update.
    pub fn last_trade(&self, market: u32) -> Option<(u64, u64)> {
        self.last.get(&market).copied()
    }

    /// Oldest first; `from` / `to` are inclusive open times; at most `limit` candles, the NEWEST ones in the range.
    pub fn candles(&self, market: u32, interval: usize, from: Option<u64>, to: Option<u64>, limit: usize) -> Vec<serde_json::Value> {
        let Some(series) = self.candles.get(&(market, interval)) else { return Vec::new() };
        let lo = from.unwrap_or(0);
        let hi = to.unwrap_or(u64::MAX);
        if lo > hi {
            return Vec::new();
        }
        let mut v: Vec<&Candle> = series.range(lo..=hi).rev().take(limit).map(|(_, c)| c).collect();
        v.reverse();
        v.into_iter().map(|c| self.candle_json(market, c)).collect()
    }

    pub fn last_candle(&self, market: u32, interval: usize) -> Option<serde_json::Value> {
        self.candles.get(&(market, interval)).and_then(|s| s.values().next_back()).map(|c| self.candle_json(market, c))
    }

    /// Newest first; `before` is an exclusive settlement time.
    pub fn funding_history(&self, market: u32, limit: usize, before: Option<u64>) -> (Vec<serde_json::Value>, Option<String>) {
        let Some(h) = self.funding.get(&market) else { return (Vec::new(), None) };
        let mut it = h.iter().rev().filter(|f| before.is_none_or(|b| f.time < b));
        let out: Vec<&FundingRec> = it.by_ref().take(limit).collect();
        let more = it.next().is_some();
        let cursor = if more { out.last().map(|f| f.time.to_string()) } else { None };
        (out.into_iter().map(|f| self.funding_json(f)).collect(), cursor)
    }

    pub fn last_funding(&self, market: u32) -> Option<&FundingRec> {
        self.funding.get(&market).and_then(|h| h.back())
    }

    /// Newest first; `before` is an exclusive fill id.
    pub fn fills(&self, account: &Address, market: Option<u32>, limit: usize, before: Option<u64>) -> (Vec<serde_json::Value>, Option<String>) {
        let Some(a) = self.accounts.get(account) else { return (Vec::new(), None) };
        let mut it = a.fills.iter().rev().filter(|f| before.is_none_or(|b| f.id < b) && market.is_none_or(|m| f.market == m));
        let out: Vec<&FillRec> = it.by_ref().take(limit).collect();
        let more = it.next().is_some();
        let cursor = if more { out.last().map(|f| f.id.to_string()) } else { None };
        (out.into_iter().map(|f| self.fill_json(account, f)).collect(), cursor)
    }

    /// Newest first; `before` is an exclusive payment sequence number (the cursor of the previous page).
    pub fn payments(&self, account: &Address, market: Option<u32>, limit: usize, before: Option<u64>) -> (Vec<serde_json::Value>, Option<String>) {
        let Some(a) = self.accounts.get(account) else { return (Vec::new(), None) };
        let mut it = a.payments.iter().rev().filter(|p| before.is_none_or(|b| p.seq < b) && market.is_none_or(|m| p.market == m));
        let out: Vec<&FundingPay> = it.by_ref().take(limit).collect();
        let more = it.next().is_some();
        let cursor = if more { out.last().map(|p| p.seq.to_string()) } else { None };
        (out.into_iter().map(|p| self.payment_json(p)).collect(), cursor)
    }

    /// Newest first; `open_only` = resting and untriggered orders; `before` is an exclusive order id.
    pub fn orders(&self, account: &Address, open_only: bool, market: Option<u32>, limit: usize, before: Option<u64>) -> (Vec<serde_json::Value>, Option<String>) {
        let Some(a) = self.accounts.get(account) else { return (Vec::new(), None) };
        let mut it = a
            .orders
            .iter()
            .rev()
            .filter_map(|id| self.orders.get(id))
            .filter(|o| before.is_none_or(|b| o.id < b) && market.is_none_or(|m| o.market == m) && (!open_only || o.is_live()));
        let out: Vec<&OrderRec> = it.by_ref().take(limit).collect();
        let more = it.next().is_some();
        let cursor = if more { out.last().map(|o| o.id.to_string()) } else { None };
        (out.into_iter().map(|o| self.order_json(o)).collect(), cursor)
    }

    pub fn forced_outcome(&self, index: u64) -> Option<(&'static str, Option<&'static str>, u64)> {
        self.forced_outcomes.get(&index).copied()
    }

    pub fn agent_meta(&self, account: &Address, agent: &Address) -> Option<(u64, u64)> {
        self.accounts.get(account).and_then(|a| a.agents.get(agent)).copied()
    }

    /// 24 h statistics from the 1-minute candles, at engine time `now`: (volume in notional units, high, low,
    /// price 24 h ago). `price24hAgo` is the close of the last 1 m candle that opened at least 24 h ago, else the
    /// open of the first candle inside the window, else `None`.
    pub fn stats_24h(&self, market: u32, now: u64) -> (u128, Option<u64>, Option<u64>, Option<u64>) {
        let Some(series) = self.candles.get(&(market, 0)) else { return (0, None, None, None) };
        let start = now.saturating_sub(86_400_000);
        let (mut vol, mut hi, mut lo, mut first_open) = (0u128, None::<u64>, None::<u64>, None::<u64>);
        for (_, c) in series.range(start..) {
            vol += c.q;
            hi = Some(hi.map_or(c.h, |h| h.max(c.h)));
            lo = Some(lo.map_or(c.l, |l| l.min(c.l)));
            first_open.get_or_insert(c.o);
        }
        let ago = series.range(..start).next_back().map(|(_, c)| c.c).or(first_open);
        (vol, hi, lo, ago)
    }

    /// A commitment to everything the index holds (test of determinism across replay).
    pub fn digest(&self) -> H256 {
        keccak(format!("{:?}|{:?}|{:?}|{:?}|{:?}|{:?}|{}|{}|{}", self.trades, self.candles, self.funding, self.orders, self.accounts, self.shadow, self.next_trade_id, self.next_fill_id, self.next_payment_seq).as_bytes())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shadow_trade_is_the_engine_formula() {
        // open long 2 @ 100, add 1 @ 130, close 2 @ 150, flip through zero
        let mut s = Shadow::default();
        assert_eq!(shadow_trade(&mut s, 2, 100), 0);
        assert_eq!(shadow_trade(&mut s, 1, 130), 0);
        assert_eq!((s.size, s.open_notional), (3, 330));
        // close 2 of 3: cost basis 330 * 2 / 3 = 220, realised 300 - 220 = 80
        assert_eq!(shadow_trade(&mut s, -2, 150), 80);
        assert_eq!((s.size, s.open_notional), (1, 110));
        // sell 3: closes 1 (realised 90 - 110 = -20) and opens short 2 @ 90
        assert_eq!(shadow_trade(&mut s, -3, 90), -20);
        assert_eq!((s.size, s.open_notional), (-2, -180));
        // buy back 2 @ 80: realised (-2 * 80) - (-180) = 20
        assert_eq!(shadow_trade(&mut s, 2, 80), 20);
        assert_eq!((s.size, s.open_notional), (0, 0));
    }

    #[test]
    fn interval_names() {
        assert_eq!(interval_index("1m"), Some(0));
        assert_eq!(interval_index("1d"), Some(5));
        assert_eq!(interval_index("2m"), None);
        for w in INTERVALS.windows(2) {
            assert!(w[1].1 % w[0].1 == 0 || w[1].1 > w[0].1);
        }
    }
}

//! Read-only API views over the real engine's PUBLIC state (positions, margin summary, liquidation estimate,
//! ticker, oracle status). Nothing here is consensus: no value computed in this file enters a block, a state
//! root or a checkpoint. The engine decides; this file only shows.
//!
//! WHAT COMES FROM THE ENGINE (since engine 0.2.3, which made them public on this node's request): the initial and
//! maintenance requirements of the account and of each position (`Engine::account_req`, `Engine::market_req`),
//! the leverage setting (`Engine::leverage_of`), the free collateral (`Engine::free_collateral`).
//!
//! DISPLAY REPLICAS that remain (stated, cross-checked in `tests/api_views.rs`):
//! - `tier_req`, `market_req_at`, `account_req_replica`: SPEC 6.2 at a HYPOTHETICAL price, which the liquidation
//!   estimate needs and the engine does not expose (`market_req_at` is private there). Cross-checks: at the mark
//!   the replica equals the engine's `account_req` for every account after every block of the tests, and the
//!   estimate is the engine's own boundary (one price unit above it no liquidation, at it liquidation).
//! - `step4_mark`: the mark of block step 4 (SPEC 5.2), now only a TEST ORACLE: the index takes the mark of a
//!   funding settlement from the engine's `Event::Funding` and compares it with this replica every time.
//! - `predicted_funding`: SPEC 7.2 applied to the samples collected so far (a forecast, not an engine value).
//!
//! Rounding of displayed USDC values (6 decimals), stated once: amounts OWED TO the account (equity, PnL,
//! notional) are floored; requirements (initial / maintenance margin) are rounded up; `freeCollateral` and
//! `withdrawable` are the engine's own integer (`Engine::free_collateral`), not recomputed.
//!
//! UNAUDITED PROTOTYPE.

use crate::engine::real::trim;
use crate::util::{scaled_to_decimal, scaled_to_decimal_signed, to_hex, Address};
use pe::genesis::Tier;
use pe::num::{div_ceil, div_floor, median3, MAX_PRICE, N_PER_USDC, PPM};
use pe::oracle::{HaltReason, Status};
use postern_perps_engine as pe;
use std::collections::BTreeSet;

/// Exact decimal of an engine price or size (1e-8), trailing zeros removed.
pub fn exact8(v: u64) -> String {
    trim(scaled_to_decimal(v as u128, 8))
}

/// Signed size (1e-8), exact.
pub fn exact8_signed(v: i64) -> String {
    trim(scaled_to_decimal_signed(v as i128, 8))
}

/// A value on the market's display grid at `decimals` digits (book levels, trades, orders); a value that is not on
/// that grid is printed exactly instead (never truncated).
pub fn grid(v: u64, decimals: u32) -> String {
    let d = decimals.min(8);
    let unit = 10u64.pow(8 - d);
    if v % unit == 0 {
        scaled_to_decimal((v / unit) as u128, d)
    } else {
        exact8(v)
    }
}

/// USDC (1e-6) as a decimal string with 6 digits.
pub fn usdc(v: i128) -> String {
    scaled_to_decimal_signed(v, 6)
}

/// Notional units (1e-16 USD) owed to the account -> USDC units, floored.
pub fn n_floor(n: i128) -> i128 {
    div_floor(n, N_PER_USDC)
}

/// Notional units required from the account -> USDC units, rounded up.
pub fn n_ceil(n: i128) -> i128 {
    div_ceil(n, N_PER_USDC)
}

/// A rate in RATE_SCALE (1e-9) as a decimal fraction.
pub fn rate9(r: i128) -> String {
    trim(scaled_to_decimal_signed(r, 9))
}

/// ppm of notional as basis points ("4.5").
pub fn ppm_bps(ppm: u32) -> String {
    trim(scaled_to_decimal(ppm as u128, 2))
}

// ---------------------------------------------------------------------------------------- replicas (SPEC 6.2)

/// REPLICA of engine `tier_req`: continuous piecewise requirement, sum over tiers of (part in the tier) x fraction,
/// rounded up.
pub fn tier_req(n: i128, tiers: &[Tier], frac: impl Fn(&Tier) -> i128) -> i128 {
    if n <= 0 {
        return 0;
    }
    let mut acc: i128 = 0;
    for (i, t) in tiers.iter().enumerate() {
        let lo = (t.floor as i128) * N_PER_USDC;
        if n <= lo {
            break;
        }
        let hi = tiers.get(i + 1).map(|x| (x.floor as i128) * N_PER_USDC).unwrap_or(i128::MAX);
        let part = n.min(hi) - lo;
        acc += part * frac(t);
    }
    div_ceil(acc, PPM)
}

pub fn leverage_of(e: &pe::Engine, ai: u32, mid: u32) -> u32 {
    e.leverage_of(ai, mid)
}

/// REPLICA of engine `market_req_at` without the hypothetical extra order: (IR, MR) of one market in notional
/// units, the position valued at `price`.
pub fn market_req_at(e: &pe::Engine, ai: u32, mid: u32, size: i64, price: u64) -> (i128, i128) {
    let m = &e.markets[&mid];
    let mark = price as i128;
    let lev = leverage_of(e, ai, mid) as i128;
    let lev_frac = div_ceil(PPM, lev.max(1));
    let (mut bids, mut asks, mut pb, mut pa) = (0i128, 0i128, 0i128, 0i128);
    for oid in &e.accounts[ai as usize].orders {
        let Some(o) = e.orders.get(oid) else { continue };
        if o.market != mid || o.trigger.is_some() || o.reduce_only {
            continue;
        }
        if o.is_buy {
            bids += o.remaining as i128;
            pb = pb.max(o.price as i128);
        } else {
            asks += o.remaining as i128;
            pa = pa.max(o.price as i128);
        }
    }
    let s = size as i128;
    let n_pos = s.abs() * mark;
    let n_buy = (s + bids).abs() * mark.max(if bids > 0 { pb } else { 0 });
    let n_sell = (s - asks).abs() * mark.max(if asks > 0 { pa } else { 0 });
    let n_ir = n_pos.max(n_buy).max(n_sell);
    let ir = tier_req(n_ir, &m.p.tiers, |t| (t.imf_ppm as i128).max(lev_frac));
    let mr = tier_req(n_pos, &m.p.tiers, |t| t.mmf_ppm as i128);
    (ir, mr)
}

/// Markets in which the account has a position or a resting order.
pub fn account_markets(e: &pe::Engine, ai: u32) -> BTreeSet<u32> {
    let a = &e.accounts[ai as usize];
    let mut mids: BTreeSet<u32> = a.positions.keys().copied().collect();
    for oid in &a.orders {
        if let Some(o) = e.orders.get(oid) {
            mids.insert(o.market);
        }
    }
    mids
}

/// (IR, MR) of the account over all markets at the mark, notional units: the ENGINE's own `account_req`.
pub fn account_req(e: &pe::Engine, ai: u32) -> (i128, i128) {
    e.account_req(ai, None)
}

/// REPLICA of engine `account_req(ai, None)` (test oracle for `market_req_at`, which the liquidation estimate
/// uses at hypothetical prices).
pub fn account_req_replica(e: &pe::Engine, ai: u32) -> (i128, i128) {
    let a = &e.accounts[ai as usize];
    let (mut ir, mut mr) = (0, 0);
    for mid in account_markets(e, ai) {
        let size = a.positions.get(&mid).map(|p| p.size).unwrap_or(0);
        let (i, m) = market_req_at(e, ai, mid, size, e.markets[&mid].mark);
        ir += i;
        mr += m;
    }
    (ir, mr)
}

/// REPLICA of block step 4 (`update_marks`, SPEC 5.2) for one market: index + clamp(median3(bid, ask, last) -
/// index, band). `last` counts only while not older than MARK_TRADE_MAX_AGE_MS at `t`.
pub fn step4_mark(index: u64, best_bid: Option<u64>, best_ask: Option<u64>, last_trade: Option<(u64, u64)>, mark_band_bps: u32, t: u64) -> u64 {
    if index == 0 {
        return 0;
    }
    let bid = best_bid.unwrap_or(index);
    let ask = best_ask.unwrap_or(index);
    let last = match last_trade {
        Some((p, lt)) if t.saturating_sub(lt) <= pe::engine::MARK_TRADE_MAX_AGE_MS => p,
        _ => index,
    };
    let book_ref = median3(bid, ask, last) as i128;
    let band = (index as i128) * (mark_band_bps as i128) / 10000;
    let mark = index as i128 + (book_ref - index as i128).clamp(-band, band);
    mark.clamp(1, MAX_PRICE as i128) as u64
}

/// SPEC 7.2 on a set of premium samples: lower-middle median, interest clamp, cap. `None` without samples.
pub fn funding_rate_of(samples: &[i128], p: &pe::genesis::MarketParams) -> Option<(i128, i128)> {
    if samples.is_empty() {
        return None;
    }
    let mut s = samples.to_vec();
    s.sort_unstable();
    let prem = s[(s.len() - 1) / 2];
    let (interest, clampc, maxr) = (p.interest as i128, p.funding_clamp as i128, p.max_funding_rate as i128);
    let rate = (prem + (interest - prem).clamp(-clampc, clampc)).clamp(-maxr, maxr);
    Some((rate, prem))
}

// ---------------------------------------------------------------------------------------- liquidation estimate

/// E - MR of the account (notional units) if market `mid` were marked at `p`, every other mark held.
fn margin_excess_at(e: &pe::Engine, ai: u32, mid: u32, p: u64) -> i128 {
    let a = &e.accounts[ai as usize];
    let mut eq = a.balance * N_PER_USDC;
    let mut mr = 0i128;
    for (k, pos) in &a.positions {
        let price = if *k == mid { p } else { e.markets[k].mark };
        eq += (pos.size as i128) * (price as i128) - pos.open_notional;
        let (_, m) = market_req_at(e, ai, *k, pos.size, price);
        mr += m;
    }
    eq - mr
}

/// The mark of `mid` at which the engine's liquidation test `E < MR` first holds, every other mark held still:
/// for a long the HIGHEST such price, for a short the LOWEST. `None` when no price in `1..=MAX_PRICE` liquidates
/// (or the account is already below maintenance in the other direction). Binary search on the integer price
/// (E - MR is monotone in the price of one position because every maintenance fraction is below 100%).
pub fn liquidation_price(e: &pe::Engine, ai: u32, mid: u32) -> Option<u64> {
    let size = e.accounts[ai as usize].positions.get(&mid)?.size;
    if size == 0 {
        return None;
    }
    let liq = |p: u64| margin_excess_at(e, ai, mid, p) < 0;
    if size > 0 {
        // liquidated at low prices: find the highest p with liq(p)
        if !liq(1) {
            return None;
        }
        if liq(MAX_PRICE) {
            return Some(MAX_PRICE);
        }
        let (mut lo, mut hi) = (1u64, MAX_PRICE); // liq(lo), !liq(hi)
        while hi - lo > 1 {
            let mid_p = lo + (hi - lo) / 2;
            if liq(mid_p) {
                lo = mid_p;
            } else {
                hi = mid_p;
            }
        }
        Some(lo)
    } else {
        if !liq(MAX_PRICE) {
            return None;
        }
        if liq(1) {
            return Some(1);
        }
        let (mut lo, mut hi) = (1u64, MAX_PRICE); // !liq(lo), liq(hi)
        while hi - lo > 1 {
            let mid_p = lo + (hi - lo) / 2;
            if liq(mid_p) {
                hi = mid_p;
            } else {
                lo = mid_p;
            }
        }
        Some(hi)
    }
}

// ---------------------------------------------------------------------------------------- account

/// Per-position values the index keeps and the engine does not (realised PnL and funding since the position
/// opened), USDC units.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct PositionExtras {
    pub realized_pnl: i128,
    pub funding: i128,
}

/// The engine's fee tier of an account now: (tier index, maker ppm, taker ppm, 14-day volume in USDC units).
pub fn fee_tier(e: &pe::Engine, ai: Option<u32>) -> (usize, u32, u32, u128) {
    let tiers = &e.g.g.fee_tiers;
    let today = e.time / pe::engine::DAY_MS;
    let vol: u128 = match ai {
        Some(ai) => e.accounts[ai as usize].volume.range(today.saturating_sub(14)..today).map(|(_, v)| *v).sum(),
        None => 0,
    };
    let idx = tiers.iter().rposition(|t| vol >= t.min_volume_14d as u128).unwrap_or(0);
    let t = &tiers[idx];
    (idx, t.maker_fee_ppm, t.taker_fee_ppm, vol)
}

/// API `Position` objects of an account. `symbol_of` maps a market id to its API symbol; `extras` gives the
/// index's per-position values.
pub fn positions_json(e: &pe::Engine, ai: u32, extras: &dyn Fn(u32) -> PositionExtras) -> Vec<serde_json::Value> {
    let a = &e.accounts[ai as usize];
    let mut out = Vec::new();
    for (mid, pos) in &a.positions {
        if pos.size == 0 {
            continue;
        }
        let m = &e.markets[mid];
        let abs = pos.size.unsigned_abs() as i128;
        let entry = (pos.open_notional.abs() / abs.max(1)).min(u64::MAX as i128) as u64; // truncated to 1e-8
        let upnl = (pos.size as i128) * (m.mark as i128) - pos.open_notional;
        let (ir, mr) = e.market_req(ai, *mid, pos.size, None);
        let x = extras(*mid);
        out.push(serde_json::json!({
            "symbol": m.p.symbol, "size": exact8_signed(pos.size), "entryPrice": exact8(entry), "markPrice": exact8(m.mark),
            // the engine's own integer (signed sum of size x price of the open part, 1e-16 USD): exact where
            // entryPrice is truncated
            "openNotional": pos.open_notional.to_string(),
            "notional": usdc(n_floor(abs * m.mark as i128)), "unrealizedPnl": usdc(n_floor(upnl)),
            "realizedPnl": usdc(x.realized_pnl),
            "liquidationPrice": liquidation_price(e, ai, *mid).map(exact8),
            "marginMode": "cross", "leverage": leverage_of(e, ai, *mid).to_string(), "isolatedMargin": null,
            "initialMargin": usdc(n_ceil(ir)), "maintenanceMargin": usdc(n_ceil(mr)),
            "fundingSinceOpen": usdc(x.funding),
        }));
    }
    out
}

/// API `Account` (docs/API.md section 4) from the engine's state. An unknown address is a zero account.
pub fn account_json(e: &pe::Engine, addr: &Address, extras: &dyn Fn(u32) -> PositionExtras) -> serde_json::Value {
    let ai = e.by_addr.get(addr).copied();
    let (tier, maker, taker, vol) = fee_tier(e, ai);
    let fee_tier = serde_json::json!({"tier": tier, "makerFeeBps": ppm_bps(maker), "takerFeeBps": ppm_bps(taker), "volume30d": usdc(vol as i128)});
    let Some(ai) = ai else {
        return serde_json::json!({
            "address": to_hex(addr), "height": e.height, "time": e.time, "collateral": "0.000000", "unrealizedPnl": "0.000000",
            "equity": "0.000000", "initialMargin": "0.000000", "maintenanceMargin": "0.000000", "freeCollateral": "0.000000",
            "withdrawable": "0.000000", "marginRatio": "0", "feeTier": fee_tier, "positions": [], "openOrders": 0,
        });
    };
    let a = &e.accounts[ai as usize];
    let eq = e.equity(ai);
    let upnl = eq - a.balance * N_PER_USDC;
    let (ir, mr) = account_req(e, ai);
    let free = e.free_collateral(ai);
    // marginRatio = MR / E rounded UP to 6 decimals (toward the liquidation side); E <= 0 with a requirement is
    // shown as the cap 1000000
    let ratio = if mr == 0 {
        "0".to_string()
    } else if eq <= 0 {
        "1000000".to_string()
    } else {
        trim(scaled_to_decimal(div_ceil(mr.saturating_mul(1_000_000), eq).clamp(0, 1_000_000_000_000) as u128, 6))
    };
    serde_json::json!({
        "address": to_hex(addr), "height": e.height, "time": e.time,
        "collateral": usdc(a.balance), "unrealizedPnl": usdc(n_floor(upnl)), "equity": usdc(n_floor(eq)),
        "initialMargin": usdc(n_ceil(ir)), "maintenanceMargin": usdc(n_ceil(mr)),
        "freeCollateral": usdc(free), "withdrawable": usdc(free.max(0)), "marginRatio": ratio,
        "feeTier": fee_tier, "positions": positions_json(e, ai, extras), "openOrders": a.orders.len(),
    })
}

// ---------------------------------------------------------------------------------------- markets

pub fn halt_reason(s: Status) -> Option<&'static str> {
    match s {
        Status::Active => None,
        Status::Halted(HaltReason::Stale) => Some("STALE"),
        Status::Halted(HaltReason::Jump) => Some("JUMP"),
        Status::Halted(HaltReason::Arithmetic) => Some("ARITHMETIC"),
    }
}

/// API `OracleStatus`: what the ENGINE holds (committee index, its time, the last accepted round). The per-venue
/// `sources` list is the feeder's knowledge, not the node's: empty here.
pub fn oracle_json(e: &pe::Engine, mid: u32, quorum: usize, held_signers: usize) -> Option<serde_json::Value> {
    let m = e.markets.get(&mid)?;
    let status = match m.oracle.status {
        Status::Active => "ok",
        Status::Halted(HaltReason::Stale) => "stale",
        Status::Halted(HaltReason::Jump) => "deviation",
        Status::Halted(HaltReason::Arithmetic) => "halted",
    };
    Some(serde_json::json!({
        "symbol": m.p.symbol, "status": status, "price": exact8(m.oracle.index), "time": m.oracle.index_time,
        "round": e.last_oracle_round.unwrap_or(0).to_string(), "sources": [], "signers": held_signers, "threshold": quorum,
        "haltReason": halt_reason(m.oracle.status),
    }))
}

/// The funding rate that would settle at the next boundary if the premium samples stayed as they are (0 without
/// samples), and that boundary.
pub fn predicted_funding(e: &pe::Engine, mid: u32) -> (i128, u64) {
    let m = &e.markets[&mid];
    let iv = m.p.funding_interval_ms.max(1);
    let next = (e.time / iv + 1) * iv;
    (funding_rate_of(&m.premium_samples, &m.p).map(|x| x.0).unwrap_or(0), next)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn grid_never_truncates() {
        assert_eq!(grid(6_725_050_000_000, 1), "67250.5");
        assert_eq!(grid(6_725_055_000_000, 1), "67250.55"); // off the 0.1 grid: exact, not cut
        assert_eq!(grid(120_000, 4), "0.0012");
        assert_eq!(exact8(100_000_000), "1");
        assert_eq!(exact8_signed(-50_000_000), "-0.5");
        assert_eq!(rate9(12_500), "0.0000125");
        assert_eq!(rate9(-12_500), "-0.0000125");
        assert_eq!(ppm_bps(450), "4.5");
        assert_eq!(usdc(-1_500_000), "-1.500000");
    }

    #[test]
    fn step4_mark_is_the_spec_formula() {
        // no book, no trade: the index
        assert_eq!(step4_mark(1000, None, None, None, 50, 10), 1000);
        // median3(bid, ask, last) inside the band
        assert_eq!(step4_mark(100_000, Some(100_100), Some(100_300), Some((100_200, 5)), 50, 10), 100_200);
        // clamped to the band (0.5% of 100000 = 500)
        assert_eq!(step4_mark(100_000, Some(102_000), Some(103_000), Some((102_500, 5)), 50, 10), 100_500);
        assert_eq!(step4_mark(100_000, Some(90_000), Some(91_000), Some((90_500, 5)), 50, 10), 99_500);
        // an old last trade counts as the index
        assert_eq!(step4_mark(100_000, Some(100_100), Some(100_300), Some((100_200, 0)), 50, 60_002), 100_100);
        assert_eq!(step4_mark(0, Some(1), Some(2), None, 50, 1), 0);
    }
}

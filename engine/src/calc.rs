//! The engine's formulas as pure functions (SPEC sections 5, 6, 7, 9), one definition each. `engine.rs` calls
//! these, the worked-example vectors (`engine/vectors/worked-examples.json`, SPEC 15) are their outputs, and the
//! tests compare them with what the state machine actually did. Integer arithmetic only; every rounding direction
//! is stated (S-0.3). UNAUDITED PROTOTYPE.
use crate::genesis::Tier;
use crate::num::*;

/// Notional of `size` at `price`, notional units (1e-16 USD). Exact.
pub fn notional(price: u64, size: u64) -> i128 {
    (price as i128) * (size as i128)
}

/// S-9.3: trading fee in USDC units, rounded UP (the user pays at least the exact amount).
pub fn trade_fee(notional: i128, fee_ppm: u32) -> i128 {
    div_ceil(notional * fee_ppm as i128, PPM * N_PER_USDC)
}

/// 6.3 step 4: liquidation fee in USDC units on the notional at mark, rounded UP (before the cap at the equity
/// that remains).
pub fn liquidation_fee(size_abs: u64, mark: u64, liquidation_fee_ppm: u32) -> i128 {
    div_ceil(notional(mark, size_abs) * liquidation_fee_ppm as i128, PPM * N_PER_USDC)
}

/// 6.2 (R395-15): the leverage fraction in ppm, rounded UP: `ceil(10^6 / leverage)`.
pub fn leverage_fraction_ppm(leverage: u32) -> i128 {
    div_ceil(PPM, leverage as i128)
}

/// 6.2: continuous piecewise requirement in NOTIONAL units: sum over tiers of (part of `n` inside the tier) x
/// fraction, divided by 10^6 rounding UP. `frac` gives the tier's fraction in ppm.
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

/// 6.2: maintenance requirement of a notional, notional units (rounded up).
pub fn maintenance_req(n: i128, tiers: &[Tier]) -> i128 {
    tier_req(n, tiers, |t| t.mmf_ppm as i128)
}

/// 6.2: initial requirement of a notional at `leverage`, notional units (rounded up):
/// `imf' = max(imf_t, ceil(10^6 / leverage))` per tier.
pub fn initial_req(n: i128, tiers: &[Tier], leverage: u32) -> i128 {
    let lf = leverage_fraction_ppm(leverage);
    tier_req(n, tiers, |t| (t.imf_ppm as i128).max(lf))
}

/// A requirement in notional units as USDC units, rounded UP (what must be held).
pub fn req_usdc(req_notional: i128) -> i128 {
    div_ceil(req_notional, N_PER_USDC)
}

/// 6.2: unrealised result of a position at `price`, notional units (signed, exact).
pub fn unrealised(size: i64, open_notional: i128, price: u64) -> i128 {
    (size as i128) * (price as i128) - open_notional
}

/// S-5.2: mark price. A missing bid / ask / fresh last trade is replaced by the index; the band rounds DOWN.
pub fn mark_price(index: u64, best_bid: Option<u64>, best_ask: Option<u64>, last_trade: Option<u64>, mark_band_bps: u32) -> u64 {
    let book_ref = median3(best_bid.unwrap_or(index), best_ask.unwrap_or(index), last_trade.unwrap_or(index)) as i128;
    let band = (index as i128) * (mark_band_bps as i128) / 10000;
    let mark = index as i128 + (book_ref - index as i128).clamp(-band, band);
    mark.clamp(1, MAX_PRICE as i128) as u64
}

/// S-7.1: premium sample in RATE_SCALE from the impact prices (a side too thin = `None` = the index), rounded
/// toward zero.
pub fn premium(impact_bid: Option<u64>, impact_ask: Option<u64>, index: u64) -> i128 {
    let index = index as i128;
    let ib = impact_bid.map(|x| x as i128).unwrap_or(index);
    let ia = impact_ask.map(|x| x as i128).unwrap_or(index);
    let num = (ib - index).max(0) - (index - ia).max(0);
    num * RATE_SCALE / index
}

/// S-7.2: the interval's funding rate (RATE_SCALE) from its premium samples: the MEDIAN (lower middle), then
/// `clamp(P + clamp(interest - P, -c, +c), -max, +max)`. `None` when there is no sample (no funding).
pub fn funding_rate(samples: &[i128], interest: i64, funding_clamp: i64, max_funding_rate: i64) -> Option<i128> {
    if samples.is_empty() {
        return None;
    }
    let mut s = samples.to_vec();
    s.sort_unstable();
    let p = s[(s.len() - 1) / 2];
    let (interest, c, maxr) = (interest as i128, funding_clamp as i128, max_funding_rate as i128);
    Some((p + (interest - p).clamp(-c, c)).clamp(-maxr, maxr))
}

/// S-7.3: the USDC units CREDITED to a position's account for one funding interval (negative = it pays):
/// `floor(-(size * mark * rate) / (RATE_SCALE * 10^10))`: a payer pays the exact amount rounded up, a receiver
/// gets it rounded down. `None` on overflow (the market halts with ARITHMETIC, S-0.2).
pub fn funding_credit(size: i64, mark: u64, rate: i128) -> Option<i128> {
    let x = (size as i128).checked_mul(mark as i128)?.checked_mul(rate)?;
    Some(div_floor(-x, RATE_SCALE * N_PER_USDC))
}

/// 6.3 step 2: bankruptcy price of a position: the price at which the account's equity is 0 with `rest` = balance
/// x 10^10 plus the unrealised result of its OTHER positions at mark (notional units). Rounded toward the account
/// (up for a long, down for a short), clamped to `[1, MAX_PRICE]`.
pub fn bankruptcy_price(size: i64, open_notional: i128, rest: i128) -> u64 {
    let s = size as i128;
    let num = open_notional - rest;
    let bp = if s > 0 { div_ceil(num, s) } else { div_floor(-num, -s) };
    bp.clamp(1, MAX_PRICE as i128) as u64
}

/// 6.3 step 2 (R395-13): the liquidation price bound: `max(bp, mark - mark * mmf_0 / 2)` for a long,
/// `min(bp, mark + mark * mmf_0 / 2)` for a short; the offset uses `floor(mmf_0 / 2)` ppm and rounds DOWN.
pub fn liquidation_bound(is_long: bool, bp: u64, mark: u64, mmf0_ppm: u32) -> u64 {
    let half_mmf = mmf0_ppm as u128 / 2;
    let off = ((mark as u128) * half_mmf / 1_000_000) as u64;
    if is_long {
        bp.max(mark.saturating_sub(off).max(1))
    } else {
        bp.min(mark.saturating_add(off).min(MAX_PRICE))
    }
}

/// Is a cross account holding ONE position liquidatable at `mark` (6.2: `E < MR`, strict)?
pub fn single_position_liquidatable(balance: i128, size: i64, open_notional: i128, mark: u64, tiers: &[Tier]) -> bool {
    let e = balance * N_PER_USDC + unrealised(size, open_notional, mark);
    let mr = maintenance_req(notional(mark, size.unsigned_abs()), tiers);
    e < mr
}

/// The liquidation price a client displays for a cross account holding ONE position and nothing else: for a long,
/// the HIGHEST mark at which the account is liquidatable; for a short, the LOWEST. `None` when no mark in
/// `[1, MAX_PRICE]` liquidates it. Found by bisection on the exact predicate above (which is monotonic in the mark
/// because every maintenance fraction is below 100%), so it carries the engine's rounding and tiers exactly.
pub fn liquidation_price_single(balance: i128, size: i64, open_notional: i128, tiers: &[Tier]) -> Option<u64> {
    if size == 0 {
        return None;
    }
    let liq = |p: u64| single_position_liquidatable(balance, size, open_notional, p, tiers);
    if size > 0 {
        // liquidatable on [1, x], safe above
        if !liq(1) {
            return None;
        }
        if liq(MAX_PRICE) {
            return Some(MAX_PRICE);
        }
        let (mut lo, mut hi) = (1u64, MAX_PRICE); // liq(lo), !liq(hi)
        while hi - lo > 1 {
            let mid = lo + (hi - lo) / 2;
            if liq(mid) {
                lo = mid;
            } else {
                hi = mid;
            }
        }
        Some(lo)
    } else {
        // safe below, liquidatable on [x, MAX_PRICE]
        if !liq(MAX_PRICE) {
            return None;
        }
        if liq(1) {
            return Some(1);
        }
        let (mut lo, mut hi) = (1u64, MAX_PRICE); // !liq(lo), liq(hi)
        while hi - lo > 1 {
            let mid = lo + (hi - lo) / 2;
            if liq(mid) {
                hi = mid;
            } else {
                lo = mid;
            }
        }
        Some(hi)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn tiers() -> Vec<Tier> {
        vec![Tier { floor: 0, imf_ppm: 20_000, mmf_ppm: 10_000 }, Tier { floor: 1_000_000_000_000, imf_ppm: 40_000, mmf_ppm: 20_000 }]
    }
    #[test]
    fn spec_15_worked_example() {
        let n = notional(6_725_000_000_000, 50_000_000);
        assert_eq!(n, 336_250_000_000_000_000_000);
        assert_eq!(trade_fee(n, 450), 15_131_250);
        assert_eq!(req_usdc(initial_req(n, &tiers(), 10)), 3_362_500_000);
        assert_eq!(req_usdc(maintenance_req(n, &tiers())), 336_250_000);
    }
    #[test]
    fn liquidation_price_is_the_exact_threshold() {
        let t = tiers();
        // long 0.5 BTC opened at 67,250 with 3,500 USDC
        let (size, open) = (50_000_000i64, notional(6_725_000_000_000, 50_000_000));
        let p = liquidation_price_single(3_500_000_000, size, open, &t).unwrap();
        assert!(single_position_liquidatable(3_500_000_000, size, open, p, &t));
        assert!(!single_position_liquidatable(3_500_000_000, size, open, p + 1, &t));
        // the same short
        let p = liquidation_price_single(3_500_000_000, -size, -open, &t).unwrap();
        assert!(single_position_liquidatable(3_500_000_000, -size, -open, p, &t));
        assert!(!single_position_liquidatable(3_500_000_000, -size, -open, p - 1, &t));
        // fully collateralised long: never liquidated
        assert_eq!(liquidation_price_single(40_000_000_000, size, open, &t), None);
    }
    #[test]
    fn funding_rounds_against_both_sides() {
        // 0.3 BTC at 67,251.3 and rate 12,345e-9: exact 249065.1896 micro-USDC: the payer pays the ceiling, the receiver gets the floor
        let c_long = funding_credit(30_000_000, 6_725_130_000_000, 12_345).unwrap();
        let c_short = funding_credit(-30_000_000, 6_725_130_000_000, 12_345).unwrap();
        assert_eq!((c_long, c_short), (-249_066, 249_065));
        assert_eq!(funding_rate(&[], 12_500, 62_500, 500_000), None);
        assert_eq!(funding_rate(&[0], 12_500, 62_500, 500_000), Some(12_500));
        assert_eq!(funding_rate(&[900_000, -5, 100_000], 12_500, 62_500, 500_000), Some(37_500));
        assert_eq!(funding_rate(&[900_000, 900_000], 12_500, 62_500, 500_000), Some(500_000));
    }
}

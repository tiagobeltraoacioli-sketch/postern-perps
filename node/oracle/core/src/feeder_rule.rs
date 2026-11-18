//! One validator's aggregation of venue readings into an observation price (README section 7, F2 to F6).
//!
//! This runs in each validator's feeder, not in the engine, so it may use the feeder's clock (`received_ms`).
//! It is still pure and integer-only so that recorded fixtures replay to the same observation on every machine
//! (`perps-oracle replay`). All prices here are in units of 1e-18 USD (see [`crate::decimal`]).

use crate::decimal::{mul_div_floor_d18, to_scale_half_up, within_bps, D18, MAX_D18};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Quote {
    Usd,
    /// Converted with the round's USDT/USD rate (F3).
    Usdt,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VenueTime {
    /// Time of the venue's quote snapshot.
    Snapshot(u64),
    /// Time of the venue's last trade.
    LastTrade(u64),
}

/// One venue's reading for one pair, as an adapter parsed it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Reading {
    pub bid: u128,
    pub ask: u128,
    pub last: Option<u128>,
    pub venue_time: Option<VenueTime>,
    /// The feeder's clock when the response arrived.
    pub received_ms: u64,
    pub quote: Quote,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FeederParams {
    pub max_spread_bps: u32,
    pub max_snapshot_age_ms: u64,
    pub max_trade_age_ms: u64,
    pub max_future_skew_ms: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Invalid {
    ZeroOrHuge,
    Crossed,
    Spread,
    ZeroLast,
    VenueTimeFuture,
    VenueTimeOld,
    Frozen,
    NoUsdtRate,
}

impl Invalid {
    pub fn code(&self) -> &'static str {
        match self {
            Invalid::ZeroOrHuge => "zero-or-huge",
            Invalid::Crossed => "crossed",
            Invalid::Spread => "spread",
            Invalid::ZeroLast => "zero-last",
            Invalid::VenueTimeFuture => "venue-time-future",
            Invalid::VenueTimeOld => "venue-time-old",
            Invalid::Frozen => "frozen",
            Invalid::NoUsdtRate => "no-usdt-rate",
        }
    }
}

/// F2 and F3 without the quote conversion and the frozen check: the reading's price in its own quote currency.
pub fn reading_price(r: &Reading, p: &FeederParams) -> Result<u128, Invalid> {
    if r.bid == 0 || r.ask == 0 || r.bid > MAX_D18 || r.ask > MAX_D18 {
        return Err(Invalid::ZeroOrHuge);
    }
    if r.bid > r.ask {
        return Err(Invalid::Crossed);
    }
    let mid = (r.bid + r.ask) / 2;
    if (r.ask - r.bid) * 10_000 > u128::from(p.max_spread_bps) * mid {
        return Err(Invalid::Spread);
    }
    if let Some(l) = r.last {
        if l == 0 || l > MAX_D18 {
            return Err(Invalid::ZeroLast);
        }
    }
    if let Some(vt) = r.venue_time {
        let (t, max_age) = match vt {
            VenueTime::Snapshot(t) => (t, p.max_snapshot_age_ms),
            VenueTime::LastTrade(t) => (t, p.max_trade_age_ms),
        };
        if t > r.received_ms.saturating_add(p.max_future_skew_ms) {
            return Err(Invalid::VenueTimeFuture);
        }
        if r.received_ms.saturating_sub(t) > max_age {
            return Err(Invalid::VenueTimeOld);
        }
    }
    // F3: median(bid, ask, last) = last clamped into [bid, ask]; the mid when there is no last price.
    Ok(match r.last {
        Some(l) => l.clamp(r.bid, r.ask),
        None => mid,
    })
}

/// F3 conversion of a USDT-quoted price to USD (rounded down at 1e-18).
pub fn to_usd(price: u128, quote: Quote, usdt_rate: Option<u128>) -> Result<u128, Invalid> {
    match quote {
        Quote::Usd => Ok(price),
        Quote::Usdt => {
            let rate = usdt_rate.ok_or(Invalid::NoUsdtRate)?;
            mul_div_floor_d18(price, rate).ok_or(Invalid::NoUsdtRate)
        }
    }
}

/// The USDT/USD rate of the round from its own aggregation (F1 to F6 over USD venues of USDT/USD), accepted only
/// within `max_depeg_bps` of 1 (F3). `None`: readings quoted in USDT are dropped this round.
pub fn usdt_rate(aggregated: Option<u128>, max_depeg_bps: u32) -> Option<u128> {
    aggregated.filter(|&r| within_bps(r, D18, max_depeg_bps, D18))
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Abstain {
    NoReadings,
    TooFewSurvivors { survivors: u8, valid: u8 },
    NoMajority { survivors: u8, valid: u8 },
    Scale,
}

/// F4 to F5: the reference median of the valid USD prices, the survivors within `max_source_deviation_bps` of it,
/// and the strict-majority rule. Returns `(sort(survivors)[len/2], survivors)` at 1e-18 (F6 before rounding).
pub fn aggregate(
    prices: &[u128],
    min_sources: u8,
    max_source_deviation_bps: u32,
) -> Result<(u128, u8), Abstain> {
    if prices.is_empty() {
        return Err(Abstain::NoReadings);
    }
    let mut v = prices.to_vec();
    v.sort_unstable();
    let m0 = v[v.len() / 2];
    let surv: Vec<u128> = v
        .iter()
        .copied()
        .filter(|&p| within_bps(p, m0, max_source_deviation_bps, m0))
        .collect();
    let c = v.len().min(255) as u8;
    let s = surv.len().min(255) as u8;
    if surv.len() < usize::from(min_sources) {
        return Err(Abstain::TooFewSurvivors {
            survivors: s,
            valid: c,
        });
    }
    if 2 * surv.len() <= v.len() {
        return Err(Abstain::NoMajority {
            survivors: s,
            valid: c,
        });
    }
    Ok((surv[surv.len() / 2], s))
}

/// F6: the observation entry `(price in the market scale, rounded half up; sources)`.
pub fn observe(
    prices: &[u128],
    min_sources: u8,
    max_source_deviation_bps: u32,
    price_decimals: u8,
) -> Result<(u64, u8), Abstain> {
    let (p, s) = aggregate(prices, min_sources, max_source_deviation_bps)?;
    let scaled = to_scale_half_up(p, price_decimals).ok_or(Abstain::Scale)?;
    Ok((scaled, s))
}

/// F2's frozen check, per venue and pair. A venue whose price has not changed for `frozen_after_ms` while this
/// feeder's own observation of the market moved by at least `frozen_move_bps` over the same time is serving a
/// stuck cache: its reading is invalid until its price changes.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct FrozenTracker {
    /// (venue price, since when unchanged, this feeder's observation at that time)
    state: Option<(u128, u64, Option<u128>)>,
}

impl FrozenTracker {
    /// `own_observation`: this feeder's latest observation of the market (1e-18), if any.
    pub fn check(
        &mut self,
        price: u128,
        now_ms: u64,
        own_observation: Option<u128>,
        frozen_after_ms: u64,
        frozen_move_bps: u32,
    ) -> bool {
        match self.state {
            Some((last, since, ref_obs)) if last == price => {
                let ref_obs = ref_obs.or(own_observation);
                self.state = Some((last, since, ref_obs));
                match (ref_obs, own_observation) {
                    (Some(a), Some(b)) => {
                        now_ms.saturating_sub(since) >= frozen_after_ms
                            && a.abs_diff(b) * 10_000 >= u128::from(frozen_move_bps) * a
                    }
                    _ => false,
                }
            }
            _ => {
                self.state = Some((price, now_ms, own_observation));
                false
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::decimal::parse_d18;

    fn d(s: &str) -> u128 {
        parse_d18(s).unwrap()
    }

    const FP: FeederParams = FeederParams {
        max_spread_bps: 100,
        max_snapshot_age_ms: 5000,
        max_trade_age_ms: 300_000,
        max_future_skew_ms: 2000,
    };

    fn r(bid: &str, ask: &str, last: Option<&str>) -> Reading {
        Reading {
            bid: d(bid),
            ask: d(ask),
            last: last.map(d),
            venue_time: None,
            received_ms: 1_000_000,
            quote: Quote::Usd,
        }
    }

    #[test]
    fn price_is_last_clamped_into_the_book() {
        assert_eq!(
            reading_price(&r("100", "101", Some("100.5")), &FP),
            Ok(d("100.5"))
        );
        assert_eq!(
            reading_price(&r("100", "101", Some("90")), &FP),
            Ok(d("100")),
            "a stale low last is clamped to the bid"
        );
        assert_eq!(
            reading_price(&r("100", "101", Some("150")), &FP),
            Ok(d("101"))
        );
        assert_eq!(reading_price(&r("100", "101", None), &FP), Ok(d("100.5")));
    }

    #[test]
    fn invalid_readings() {
        assert_eq!(
            reading_price(&r("0", "1", None), &FP),
            Err(Invalid::ZeroOrHuge)
        );
        assert_eq!(
            reading_price(&r("101", "100", None), &FP),
            Err(Invalid::Crossed)
        );
        // 1.1% spread with a 1% bound.
        assert_eq!(
            reading_price(&r("100", "101.1", None), &FP),
            Err(Invalid::Spread)
        );
        assert_eq!(
            reading_price(&r("100", "100.1", Some("0")), &FP),
            Err(Invalid::ZeroLast)
        );
        let mut x = r("100", "100.1", None);
        x.venue_time = Some(VenueTime::Snapshot(1_000_000 - 5001));
        assert_eq!(reading_price(&x, &FP), Err(Invalid::VenueTimeOld));
        x.venue_time = Some(VenueTime::Snapshot(1_000_000 - 5000));
        assert!(reading_price(&x, &FP).is_ok());
        x.venue_time = Some(VenueTime::LastTrade(1_000_000 - 300_001));
        assert_eq!(reading_price(&x, &FP), Err(Invalid::VenueTimeOld));
        x.venue_time = Some(VenueTime::Snapshot(1_000_000 + 2001));
        assert_eq!(reading_price(&x, &FP), Err(Invalid::VenueTimeFuture));
    }

    #[test]
    fn usdt_conversion_and_depeg() {
        assert_eq!(usdt_rate(Some(d("0.999")), 500), Some(d("0.999")));
        assert_eq!(
            usdt_rate(Some(d("0.95")), 500),
            Some(d("0.95")),
            "exactly at the bound"
        );
        assert_eq!(usdt_rate(Some(d("0.9499")), 500), None);
        assert_eq!(usdt_rate(Some(d("1.0501")), 500), None);
        assert_eq!(usdt_rate(None, 500), None);
        assert_eq!(
            to_usd(d("64939.07"), Quote::Usdt, Some(d("0.99903"))),
            Ok(d("64876.0791021"))
        );
        assert_eq!(
            to_usd(d("64939.07"), Quote::Usdt, None),
            Err(Invalid::NoUsdtRate)
        );
        assert_eq!(to_usd(d("64939.07"), Quote::Usd, None), Ok(d("64939.07")));
    }

    #[test]
    fn one_liar_among_five_is_rejected() {
        let v = [d("100"), d("100.1"), d("99.9"), d("100.05"), d("150")];
        assert_eq!(aggregate(&v, 3, 200), Ok((d("100.05"), 4)));
        // The liar is cut, so the observation is the median of the four honest values (upper median).
        assert_eq!(observe(&v, 3, 200, 2), Ok((10005, 4)));
    }

    #[test]
    fn a_lying_majority_or_a_split_abstains() {
        // 2 honest at 100, 2 liars at 150: the reference median is a liar; survivors 2 of 4: no strict majority.
        let v = [d("100"), d("100"), d("150"), d("150")];
        assert_eq!(
            aggregate(&v, 2, 200),
            Err(Abstain::NoMajority {
                survivors: 2,
                valid: 4
            })
        );
        // Three venues, all different by 5%: each alone.
        let v = [d("100"), d("105"), d("110.25")];
        assert_eq!(
            aggregate(&v, 3, 200),
            Err(Abstain::TooFewSurvivors {
                survivors: 1,
                valid: 3
            })
        );
        assert_eq!(aggregate(&[], 1, 200), Err(Abstain::NoReadings));
    }

    #[test]
    fn rounding_to_scale_is_half_up() {
        assert_eq!(
            observe(&[d("64870.25"), d("64870.25"), d("64870.25")], 3, 200, 1),
            Ok((648_703, 3))
        );
        assert_eq!(
            observe(&[d("0.0000001"), d("0.0000001"), d("0.0000001")], 3, 200, 1),
            Err(Abstain::Scale)
        );
    }

    #[test]
    fn frozen_venue() {
        let mut t = FrozenTracker::default();
        let p = d("100");
        assert!(!t.check(p, 0, Some(d("100")), 60_000, 20));
        // Unchanged for 60 s while the market moved 0.19%: not frozen.
        assert!(!t.check(p, 60_000, Some(d("100.19")), 60_000, 20));
        // Moved 0.2% (= frozenMoveBps): frozen.
        assert!(t.check(p, 60_000, Some(d("100.2")), 60_000, 20));
        // Moved a lot but only for 59 s: not yet.
        let mut t = FrozenTracker::default();
        t.check(p, 0, Some(d("100")), 60_000, 20);
        assert!(!t.check(p, 59_999, Some(d("110")), 60_000, 20));
        // Between 0.19% and 0.2% (0.195%): not frozen (the bound is exact, >= 20 bps).
        let mut t = FrozenTracker::default();
        t.check(p, 0, Some(d("100")), 60_000, 20);
        assert!(!t.check(p, 60_000, Some(d("100.195")), 60_000, 20));
        // A changed venue price restarts the clock.
        assert!(!t.check(d("100.01"), 120_000, Some(d("110")), 60_000, 20));
    }
}

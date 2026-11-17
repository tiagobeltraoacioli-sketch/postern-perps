//! One validator's observation for one round, from the venues' responses (README section 7).

use std::collections::BTreeMap;

use perps_oracle::decimal::{within_bps, D18};
use perps_oracle::feeder_rule::{self, Abstain, FeederParams, FrozenTracker, VenueTime};
use perps_oracle::types::{round_of, Observation, OraclePrice};

use crate::venues::{default_sources, Source};

#[derive(Debug, Clone)]
pub struct MarketConfig {
    pub market_id: u32,
    pub symbol: String,
    pub price_decimals: u8,
    pub min_sources: u8,
    pub max_source_deviation_bps: u32,
    pub sources: Vec<Source>,
}

#[derive(Debug, Clone)]
pub struct FeederConfig {
    pub markets: Vec<MarketConfig>,
    pub usdt_sources: Vec<Source>,
    pub usdt_min_sources: u8,
    pub usdt_max_source_deviation_bps: u32,
    pub max_depeg_bps: u32,
    pub params: FeederParams,
    pub frozen_after_ms: u64,
    pub frozen_move_bps: u32,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ConfigError {
    MarketOrder,
    DuplicateOperator(String),
    MinSources(String),
    Decimals(String),
    NoSources(String),
}

impl FeederConfig {
    /// F1: one reading per operator per pair; enough configured sources to reach `min_sources`.
    pub fn validate(&self) -> Result<(), ConfigError> {
        for (i, m) in self.markets.iter().enumerate() {
            if i > 0 && self.markets[i - 1].market_id >= m.market_id {
                return Err(ConfigError::MarketOrder);
            }
            if m.price_decimals > 18 {
                return Err(ConfigError::Decimals(m.symbol.clone()));
            }
            check_sources(&m.symbol, &m.sources, m.min_sources)?;
        }
        check_sources("USDT-USD", &self.usdt_sources, self.usdt_min_sources)
    }
}

fn check_sources(name: &str, s: &[Source], min: u8) -> Result<(), ConfigError> {
    if s.is_empty() {
        return Err(ConfigError::NoSources(name.into()));
    }
    for (i, a) in s.iter().enumerate() {
        if s[..i].iter().any(|b| b.operator == a.operator) {
            return Err(ConfigError::DuplicateOperator(format!(
                "{name}: {}",
                a.operator
            )));
        }
    }
    if min == 0 || usize::from(min) > s.len() {
        return Err(ConfigError::MinSources(name.into()));
    }
    Ok(())
}

/// The launch markets with the provisional scales of `node/fixtures/markets.json` (389) and the parameters of
/// README section 8, until 392's `markets.json` and 388's SPEC fix them.
pub fn default_config() -> FeederConfig {
    let m = |id, sym: &str, base, dec| MarketConfig {
        market_id: id,
        symbol: sym.into(),
        price_decimals: dec,
        min_sources: 3,
        max_source_deviation_bps: 200,
        sources: default_sources(base),
    };
    FeederConfig {
        markets: vec![
            m(1, "BTC-USD", "BTC", 1),
            m(2, "ETH-USD", "ETH", 2),
            m(3, "SOL-USD", "SOL", 3),
        ],
        usdt_sources: default_sources("USDT"),
        usdt_min_sources: 3,
        usdt_max_source_deviation_bps: 200,
        max_depeg_bps: 500,
        params: FeederParams {
            max_spread_bps: 100,
            max_snapshot_age_ms: 5000,
            max_trade_age_ms: 300_000,
            max_future_skew_ms: 2000,
        },
        frozen_after_ms: 60_000,
        frozen_move_bps: 20,
    }
}

/// One venue response as fetched or recorded. `body: None` is a failed fetch (timeout, TLS, HTTP error).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Response {
    pub source: String,
    pub body: Option<String>,
    pub received_ms: u64,
    pub error: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SourceReport {
    pub name: String,
    /// USD price at 1e-18 (after conversion for USDT quotes), when the reading was valid.
    pub price18: Option<u128>,
    pub time: Option<u64>,
    pub used: bool,
    pub reason: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MarketReport {
    pub market_id: u32,
    pub symbol: String,
    pub result: Result<OraclePrice, String>,
    pub sources: Vec<SourceReport>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RoundReport {
    pub round: u64,
    pub time_ms: u64,
    pub usdt_rate18: Option<u128>,
    pub usdt_sources: Vec<SourceReport>,
    pub markets: Vec<MarketReport>,
}

/// The feeder's memory across rounds: the frozen trackers and its own last observation per market (1e-18).
pub struct Feeder {
    pub config: FeederConfig,
    frozen: BTreeMap<String, FrozenTracker>,
    last_obs: BTreeMap<u32, u128>,
    last_usdt: Option<u128>,
}

fn abstain_text(a: Abstain) -> String {
    match a {
        Abstain::NoReadings => "no valid readings".into(),
        Abstain::TooFewSurvivors { survivors, valid } => {
            format!("too few sources: {survivors} agreeing of {valid} valid")
        }
        Abstain::NoMajority { survivors, valid } => {
            format!("no majority: {survivors} agreeing of {valid} valid")
        }
        Abstain::Scale => "price does not fit the market scale".into(),
    }
}

impl Feeder {
    pub fn new(config: FeederConfig) -> Result<Self, ConfigError> {
        config.validate()?;
        Ok(Feeder {
            config,
            frozen: BTreeMap::new(),
            last_obs: BTreeMap::new(),
            last_usdt: None,
        })
    }

    /// F2 and F3 for one source: the reading's USD price, or the reason it is not valid.
    fn read(
        &mut self,
        s: &Source,
        resp: Option<&Response>,
        usdt_rate: Option<u128>,
        own: Option<u128>,
    ) -> SourceReport {
        let mut rep = SourceReport {
            name: s.name.clone(),
            price18: None,
            time: None,
            used: false,
            reason: None,
        };
        let Some(resp) = resp else {
            rep.reason = Some("no-response".into());
            return rep;
        };
        let Some(body) = resp.body.as_deref() else {
            rep.reason = Some(format!(
                "fetch-failed: {}",
                resp.error.as_deref().unwrap_or("unknown")
            ));
            return rep;
        };
        let reading = match s.parse(body, resp.received_ms) {
            Ok(r) => r,
            Err(e) => {
                rep.reason = Some(format!("parse: {e}"));
                return rep;
            }
        };
        rep.time = reading.venue_time.map(|t| match t {
            VenueTime::Snapshot(t) | VenueTime::LastTrade(t) => t,
        });
        let p = match feeder_rule::reading_price(&reading, &self.config.params)
            .and_then(|p| feeder_rule::to_usd(p, s.quote, usdt_rate))
        {
            Ok(p) => p,
            Err(e) => {
                rep.reason = Some(e.code().into());
                return rep;
            }
        };
        let (after, mv) = (self.config.frozen_after_ms, self.config.frozen_move_bps);
        // The frozen check compares the venue's own quote (before conversion), so a moving USDT rate cannot hide
        // a stuck USDT-quoted venue.
        let raw = feeder_rule::reading_price(&reading, &self.config.params).expect("checked above");
        if self.frozen.entry(s.name.clone()).or_default().check(
            raw,
            resp.received_ms,
            own,
            after,
            mv,
        ) {
            rep.reason = Some("frozen".into());
            return rep;
        }
        rep.price18 = Some(p);
        rep
    }

    /// F4 to F6 over valid reports; marks `used` and the outlier reason; returns the 1e-18 result.
    fn aggregate(
        reps: &mut [SourceReport],
        min_sources: u8,
        dev_bps: u32,
    ) -> Result<(u128, u8), Abstain> {
        let prices: Vec<u128> = reps.iter().filter_map(|r| r.price18).collect();
        let res = feeder_rule::aggregate(&prices, min_sources, dev_bps);
        if !prices.is_empty() {
            let mut v = prices.clone();
            v.sort_unstable();
            let m0 = v[v.len() / 2];
            for r in reps.iter_mut() {
                if let Some(p) = r.price18 {
                    if within_bps(p, m0, dev_bps, m0) {
                        r.used = res.is_ok();
                    } else {
                        r.reason = Some("outlier".into());
                    }
                }
            }
        }
        res
    }

    /// The observation of `round` sampled at `time_ms` from `responses`. A market without a safe price is absent
    /// from the observation (the signer abstains for it). Returns `None` for the observation when no market has
    /// a price (nothing to sign).
    pub fn observe(
        &mut self,
        round: u64,
        time_ms: u64,
        responses: &[Response],
    ) -> (Option<Observation>, RoundReport) {
        assert_eq!(
            round_of(time_ms),
            round,
            "the sampling time must be inside the round"
        );
        let by_name: BTreeMap<&str, &Response> =
            responses.iter().map(|r| (r.source.as_str(), r)).collect();

        // USDT/USD rate of the round (F3).
        let usdt_sources = self.config.usdt_sources.clone();
        let own_usdt = self.last_usdt;
        let mut usdt_reps: Vec<SourceReport> = usdt_sources
            .iter()
            .map(|s| self.read(s, by_name.get(s.name.as_str()).copied(), None, own_usdt))
            .collect();
        let agg = Self::aggregate(
            &mut usdt_reps,
            self.config.usdt_min_sources,
            self.config.usdt_max_source_deviation_bps,
        )
        .ok()
        .map(|(p, _)| p);
        if let Some(p) = agg {
            self.last_usdt = Some(p);
        }
        let rate = feeder_rule::usdt_rate(agg, self.config.max_depeg_bps);

        let mut prices = Vec::new();
        let mut markets = Vec::new();
        for m in self.config.markets.clone() {
            let own = self.last_obs.get(&m.market_id).copied();
            let mut reps: Vec<SourceReport> = m
                .sources
                .iter()
                .map(|s| self.read(s, by_name.get(s.name.as_str()).copied(), rate, own))
                .collect();
            let result = match Self::aggregate(&mut reps, m.min_sources, m.max_source_deviation_bps)
            {
                Ok((p18, sources)) => {
                    match perps_oracle::decimal::to_scale_half_up(p18, m.price_decimals) {
                        Some(price) => {
                            self.last_obs.insert(m.market_id, p18);
                            let e = OraclePrice {
                                market_id: m.market_id,
                                price,
                                sources,
                            };
                            prices.push(e);
                            Ok(e)
                        }
                        None => Err(abstain_text(Abstain::Scale)),
                    }
                }
                Err(a) => Err(abstain_text(a)),
            };
            markets.push(MarketReport {
                market_id: m.market_id,
                symbol: m.symbol.clone(),
                result,
                sources: reps,
            });
        }
        let report = RoundReport {
            round,
            time_ms,
            usdt_rate18: rate,
            usdt_sources: usdt_reps,
            markets,
        };
        let obs = if prices.is_empty() {
            None
        } else {
            Some(Observation {
                round,
                time_ms,
                prices,
            })
        };
        (obs, report)
    }
}

/// Decimal text of a 1e-18 value with `dp` fraction digits, truncated (reports only, never signed).
pub fn fmt18(v: u128, dp: usize) -> String {
    let int = v / D18;
    let frac = format!("{:018}", v % D18);
    if dp == 0 {
        int.to_string()
    } else {
        format!("{int}.{}", &frac[..dp.min(18)])
    }
}

//! The engine's oracle acceptance rule (README section 5, C1 to C5) and the per-market halt machine (section 6,
//! H1 to H3). Deterministic: integer arithmetic on the input, the parameters fixed at genesis and the block time
//! `T` the validators accepted. No clock is read.
//!
//! Atomicity: [`apply_update`] validates the whole input before it changes anything; on any error the state is
//! exactly as before (tested by comparing the canonical state bytes).
//!
//! Error order (part of the rule, so that every implementation reports the same error for the same input): C1;
//! then C2 over the whole input (count, signer order, every price list); then for each observation in input order,
//! C2c (time) before C2b (signature). The first failing check is returned.

use crate::decimal::within_bps;
use crate::eip712;
use crate::sig::{self, SigError};
use crate::types::OracleUpdate;
use crate::{MAX_PRICES, ROUND_MS};

/// Per-market oracle parameters (README section 8 for the defaults; 392 publishes the values in `markets.json`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MarketParams {
    pub market_id: u32,
    /// An entry with fewer surviving venues is ignored (C3).
    pub min_sources: u8,
    /// Agreement band around the committee median (C4) and between confirmation rounds (H3).
    pub agree_bps: u32,
    /// Largest step of the index from a fresh index in one committee price (H2, H3 fast return).
    pub jump_bps: u32,
    /// The index is stale (H1) when older than this at the start of a block.
    pub max_staleness_ms: u64,
    /// Agreeing committee prices needed to (re)start the index after a halt (H3).
    pub confirm_rounds: u8,
    /// Exit price band while halted (X1, README section 6a, R395-5): base half-width in bps.
    pub exit_haircut_bps: u32,
    /// Added to the half-width per whole minute since the last good index.
    pub exit_haircut_per_min_bps: u32,
    /// Largest half-width (< 10000, so the low side of the band stays above 0).
    pub exit_max_haircut_bps: u32,
    /// After this long without a good index the market is due for settlement at the band (X3).
    pub max_halt_ms: u64,
}

/// Oracle parameters fixed at genesis.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CommitteeParams {
    /// The PerpsVault address of this deployment: the `verifyingContract` of the engine signing domain (SPEC 2.1).
    pub verifying_contract: [u8; 20],
    /// The oracle committee: Ethereum addresses of the dedicated oracle keys, index = `SignedObservation.signer`.
    pub signers: Vec<[u8; 20]>,
    /// Minimum number of signed observations in one update (C2) and of agreeing values per market (C3, C4).
    pub quorum: u8,
    /// An observation may be at most this far ahead of the block time (C2c).
    pub max_future_ms: u64,
    /// An observation may be at most this old at the block time (C2c).
    pub max_age_ms: u64,
    /// Markets, `market_id` strictly ascending.
    pub markets: Vec<MarketParams>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ParamError {
    CommitteeSize,
    QuorumNotMajority,
    ZeroSigner,
    DuplicateSigner,
    NoMarkets,
    MarketOrder,
    MinSources,
    Bps,
    AgreeAboveJump,
    Staleness,
    AgeAboveStaleness,
    ConfirmRounds,
    ZeroVault,
    ExitBand,
}

impl CommitteeParams {
    /// Refuses parameters under which the rules lose their meaning. Called by [`OracleState::genesis`].
    pub fn validate(&self) -> Result<(), ParamError> {
        if self.verifying_contract == [0u8; 20] {
            return Err(ParamError::ZeroVault);
        }
        let n = self.signers.len();
        if n == 0 || n > 255 {
            return Err(ParamError::CommitteeSize);
        }
        // q > n/2: two quorums share a signer, and the median of q values tolerates (q-1)/2 liars.
        if self.quorum == 0 || 2 * usize::from(self.quorum) <= n || usize::from(self.quorum) > n {
            return Err(ParamError::QuorumNotMajority);
        }
        for (i, s) in self.signers.iter().enumerate() {
            if *s == [0u8; 20] {
                return Err(ParamError::ZeroSigner);
            }
            if self.signers[..i].contains(s) {
                return Err(ParamError::DuplicateSigner);
            }
        }
        if self.markets.is_empty() || self.markets.len() > MAX_PRICES {
            return Err(ParamError::NoMarkets);
        }
        for (i, m) in self.markets.iter().enumerate() {
            if i > 0 && self.markets[i - 1].market_id >= m.market_id {
                return Err(ParamError::MarketOrder);
            }
            if m.min_sources == 0 {
                return Err(ParamError::MinSources);
            }
            if m.agree_bps == 0 || m.agree_bps > 10_000 || m.jump_bps == 0 || m.jump_bps > 10_000 {
                return Err(ParamError::Bps);
            }
            // Honest dispersion inside one round must not by itself look like a jump.
            if m.agree_bps > m.jump_bps {
                return Err(ParamError::AgreeAboveJump);
            }
            if m.max_staleness_ms < ROUND_MS {
                return Err(ParamError::Staleness);
            }
            // An observation accepted at the age limit must not be stale on arrival.
            if self.max_age_ms > m.max_staleness_ms {
                return Err(ParamError::AgeAboveStaleness);
            }
            // One round of confirmation would follow any jump at once: no protection.
            if m.confirm_rounds < 2 {
                return Err(ParamError::ConfirmRounds);
            }
            // The band starts at least as wide as one permitted step, never shrinks, keeps its low side above 0,
            // and settlement waits at least for a confirmation to be possible.
            if m.exit_haircut_bps < m.jump_bps
                || m.exit_max_haircut_bps < m.exit_haircut_bps
                || m.exit_max_haircut_bps >= 10_000
                || m.max_halt_ms < m.max_staleness_ms
            {
                return Err(ParamError::ExitBand);
            }
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HaltReason {
    /// No index yet (genesis): the first index needs `confirm_rounds` agreeing committee prices.
    Init,
    Stale,
    Jump,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Status {
    Active,
    Halted(HaltReason),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Run {
    pub last: u64,
    pub count: u8,
    pub last_time_ms: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MarketOracle {
    pub market_id: u32,
    /// 0 until the first index.
    pub index: u64,
    pub index_time_ms: u64,
    pub status: Status,
    pub run: Option<Run>,
    /// Lowest and highest committee price accepted by C4 since the market halted (0 while active): the exit band
    /// always covers every price the committee has stood behind since the last good index (X1).
    pub halt_low: u64,
    pub halt_high: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OracleState {
    /// 0 until the first accepted update (round 0 is 1970: never accepted).
    pub last_round: u64,
    pub markets: Vec<MarketOracle>,
}

impl OracleState {
    pub fn genesis(params: &CommitteeParams) -> Result<Self, ParamError> {
        params.validate()?;
        Ok(Self {
            last_round: 0,
            markets: params
                .markets
                .iter()
                .map(|m| MarketOracle {
                    market_id: m.market_id,
                    index: 0,
                    index_time_ms: 0,
                    status: Status::Halted(HaltReason::Init),
                    run: None,
                    halt_low: 0,
                    halt_high: 0,
                })
                .collect(),
        })
    }

    pub fn market(&self, market_id: u32) -> Option<&MarketOracle> {
        self.markets.iter().find(|m| m.market_id == market_id)
    }

    /// The price the engine may use for `market_id`: `Some(index)` only while the market is active.
    pub fn safe_index(&self, market_id: u32) -> Option<u64> {
        self.market(market_id)
            .filter(|m| m.status == Status::Active)
            .map(|m| m.index)
    }

    /// Canonical bytes of the oracle state, for the engine's state commitment:
    /// `lastRound:u64be count:u16be { marketId:u32be index:u64be indexTimeMs:u64be status:u8 hasRun:u8
    /// [runLast:u64be runCount:u8 runLastTimeMs:u64be] haltLow:u64be haltHigh:u64be }`, status 0 active, 1 init,
    /// 2 stale, 3 jump.
    pub fn encode(&self) -> Vec<u8> {
        let mut b = Vec::with_capacity(10 + self.markets.len() * 39);
        b.extend_from_slice(&self.last_round.to_be_bytes());
        b.extend_from_slice(&(self.markets.len() as u16).to_be_bytes());
        for m in &self.markets {
            b.extend_from_slice(&m.market_id.to_be_bytes());
            b.extend_from_slice(&m.index.to_be_bytes());
            b.extend_from_slice(&m.index_time_ms.to_be_bytes());
            b.push(match m.status {
                Status::Active => 0,
                Status::Halted(HaltReason::Init) => 1,
                Status::Halted(HaltReason::Stale) => 2,
                Status::Halted(HaltReason::Jump) => 3,
            });
            match m.run {
                None => b.push(0),
                Some(r) => {
                    b.push(1);
                    b.extend_from_slice(&r.last.to_be_bytes());
                    b.push(r.count);
                    b.extend_from_slice(&r.last_time_ms.to_be_bytes());
                }
            }
            b.extend_from_slice(&m.halt_low.to_be_bytes());
            b.extend_from_slice(&m.halt_high.to_be_bytes());
        }
        b
    }
}

/// H1, at the start of every block before its inputs: an active market whose index is older than its staleness
/// bound halts with `STALE`. Returns the markets halted by this call.
pub fn begin_block(
    params: &CommitteeParams,
    state: &mut OracleState,
    block_time_ms: u64,
) -> Vec<u32> {
    let mut halted = Vec::new();
    for (m, p) in state.markets.iter_mut().zip(&params.markets) {
        if m.status == Status::Active
            && block_time_ms.saturating_sub(m.index_time_ms) > p.max_staleness_ms
        {
            m.status = Status::Halted(HaltReason::Stale);
            m.run = None;
            halted.push(m.market_id);
        }
    }
    halted
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Malformed {
    Count,
    SignerOrder,
    PriceCount,
    PriceOrder,
    ZeroPrice,
    RoundOverflow,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OracleError {
    /// C1 `ORACLE_ROUND_NOT_NEW`.
    RoundNotNew,
    /// C2 `ORACLE_MALFORMED`.
    Malformed(Malformed),
    /// `ORACLE_TIME`, with the index of the observation in the input: C2c (the observation is outside the
    /// update's round), or C2d (fewer than `q` observations are inside the time bounds at this block time; the
    /// index is the first one outside them).
    Time { observation: usize },
    /// C2b `ORACLE_BAD_SIGNATURE`, with the index of the observation in the input.
    BadSignature {
        observation: usize,
        reason: Option<SigError>,
    },
}

impl OracleError {
    /// The error code string of README section 5.
    pub fn code(&self) -> &'static str {
        match self {
            OracleError::RoundNotNew => "ORACLE_ROUND_NOT_NEW",
            OracleError::Malformed(_) => "ORACLE_MALFORMED",
            OracleError::Time { .. } => "ORACLE_TIME",
            OracleError::BadSignature { .. } => "ORACLE_BAD_SIGNATURE",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MarketOutcome {
    /// C3: fewer than `quorum` usable values.
    NoQuorum { have: u8 },
    /// C4: fewer than `quorum` values within `agree_bps` of the median.
    Disagree { median: u64, agree: u8 },
    /// H2: the index moved to the committee price.
    Moved { price: u64 },
    /// H2: the step exceeded `jump_bps`; the market halted, the index did not move.
    HaltedJump { price: u64 },
    /// H3: halted, the committee price counts towards confirmation.
    Confirming { price: u64, count: u8 },
    /// H3: the market resumed (fast return to a fresh index, or confirmation).
    Resumed { price: u64, confirmed: bool },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UpdateReport {
    pub round: u64,
    /// One entry per market of the parameters, ascending `market_id`.
    pub outcomes: Vec<(u32, MarketOutcome)>,
    /// C2d: the signers (ascending) whose observation was outside the time bounds at this block time and
    /// entered no median.
    pub ignored: Vec<u8>,
}

/// C1, C2, then per observation in input order: round membership (`ORACLE_TIME`), signature (C2b), and C2d.
/// Returns, per observation, whether C2d ignores it at block time `t`.
fn validate(
    params: &CommitteeParams,
    last_round: u64,
    u: &OracleUpdate,
    t: u64,
) -> Result<Vec<bool>, OracleError> {
    // C1
    if u.round <= last_round {
        return Err(OracleError::RoundNotNew);
    }
    // C2
    let n = params.signers.len();
    let count = u.observations.len();
    if count < usize::from(params.quorum) || count > n {
        return Err(OracleError::Malformed(Malformed::Count));
    }
    for (i, o) in u.observations.iter().enumerate() {
        if usize::from(o.signer) >= n || (i > 0 && u.observations[i - 1].signer >= o.signer) {
            return Err(OracleError::Malformed(Malformed::SignerOrder));
        }
        if o.prices.is_empty() || o.prices.len() > MAX_PRICES {
            return Err(OracleError::Malformed(Malformed::PriceCount));
        }
        for (j, p) in o.prices.iter().enumerate() {
            if j > 0 && o.prices[j - 1].market_id >= p.market_id {
                return Err(OracleError::Malformed(Malformed::PriceOrder));
            }
            if p.price == 0 {
                return Err(OracleError::Malformed(Malformed::ZeroPrice));
            }
        }
    }
    let start = u
        .round
        .checked_mul(ROUND_MS)
        .ok_or(OracleError::Malformed(Malformed::RoundOverflow))?;
    let end = start
        .checked_add(ROUND_MS)
        .ok_or(OracleError::Malformed(Malformed::RoundOverflow))?;
    let domain = eip712::domain_separator(&params.verifying_contract);
    let mut ignored = vec![false; count];
    for (i, o) in u.observations.iter().enumerate() {
        // C2c: the observation belongs to this round. A fact of the observation alone: the whole input is
        // refused (an honest sequencer never includes it).
        if o.time_ms < start || o.time_ms >= end {
            return Err(OracleError::Time { observation: i });
        }
        // C2d (R436-12): the two bounds relative to the block time do not refuse the input; the observation is
        // ignored. One signer with a skewed clock must not veto a round that q others carry.
        ignored[i] = o.time_ms > t.saturating_add(params.max_future_ms)
            || t.saturating_sub(o.time_ms) > params.max_age_ms;
        // C2b
        let digest = eip712::digest(&domain, &o.message(u.round));
        match sig::recover(&digest, &o.signature) {
            Ok(a) if a == params.signers[usize::from(o.signer)] => {}
            Ok(_) => {
                return Err(OracleError::BadSignature {
                    observation: i,
                    reason: None,
                })
            }
            Err(e) => {
                return Err(OracleError::BadSignature {
                    observation: i,
                    reason: Some(e),
                })
            }
        }
    }
    // C2d: fewer than q observations inside the time bounds refuse the whole input (the round is NOT consumed:
    // it may be presented again at a later block time). The first ignored observation is reported.
    let in_time = ignored.iter().filter(|x| !**x).count();
    if in_time < usize::from(params.quorum) {
        let first = ignored.iter().position(|x| *x).unwrap_or(0);
        return Err(OracleError::Time { observation: first });
    }
    Ok(ignored)
}

/// Applies one `oracle` block input at block time `t` (C1 to C5, then H2/H3 per market). On error nothing changes.
pub fn apply_update(
    params: &CommitteeParams,
    state: &mut OracleState,
    u: &OracleUpdate,
    t: u64,
) -> Result<UpdateReport, OracleError> {
    let ignored = validate(params, state.last_round, u, t)?;
    state.last_round = u.round;
    let tau = u.round * ROUND_MS; // no overflow: checked in validate
    let q = usize::from(params.quorum);
    let mut outcomes = Vec::with_capacity(params.markets.len());
    for (m, p) in state.markets.iter_mut().zip(&params.markets) {
        // C3
        let mut v: Vec<u64> = u
            .observations
            .iter()
            .zip(&ignored)
            .filter(|(_, ig)| !**ig)
            .filter_map(|(o, _)| o.prices.iter().find(|e| e.market_id == p.market_id))
            .filter(|e| e.sources >= p.min_sources)
            .map(|e| e.price)
            .collect();
        if v.len() < q {
            outcomes.push((
                p.market_id,
                MarketOutcome::NoQuorum {
                    have: v.len() as u8,
                },
            ));
            continue;
        }
        v.sort_unstable();
        let med = v[v.len() / 2];
        // C4
        let agree = v
            .iter()
            .filter(|&&x| within_bps(u128::from(x), u128::from(med), p.agree_bps, u128::from(med)))
            .count();
        if agree < q {
            outcomes.push((
                p.market_id,
                MarketOutcome::Disagree {
                    median: med,
                    agree: agree as u8,
                },
            ));
            continue;
        }
        // C5: the halt machine consumes (med, tau).
        outcomes.push((p.market_id, step(p, m, med, tau)));
    }
    Ok(UpdateReport {
        round: u.round,
        outcomes,
        ignored: u
            .observations
            .iter()
            .zip(&ignored)
            .filter(|(_, ig)| **ig)
            .map(|(o, _)| o.signer)
            .collect(),
    })
}

fn step(p: &MarketParams, m: &mut MarketOracle, price: u64, tau: u64) -> MarketOutcome {
    let near_index = m.index > 0
        && within_bps(
            u128::from(price),
            u128::from(m.index),
            p.jump_bps,
            u128::from(m.index),
        );
    match m.status {
        // H2
        Status::Active => {
            if near_index {
                m.index = price;
                m.index_time_ms = tau;
                MarketOutcome::Moved { price }
            } else {
                m.status = Status::Halted(HaltReason::Jump);
                m.run = Some(Run {
                    last: price,
                    count: 1,
                    last_time_ms: tau,
                });
                m.halt_low = price;
                m.halt_high = price;
                MarketOutcome::HaltedJump { price }
            }
        }
        // H3
        Status::Halted(_) => {
            if near_index && tau.saturating_sub(m.index_time_ms) <= p.max_staleness_ms {
                m.index = price;
                m.index_time_ms = tau;
                m.status = Status::Active;
                m.run = None;
                m.halt_low = 0;
                m.halt_high = 0;
                return MarketOutcome::Resumed {
                    price,
                    confirmed: false,
                };
            }
            // X2: every committee price while halted widens the exit band (never narrows it).
            m.halt_low = if m.halt_low == 0 {
                price
            } else {
                m.halt_low.min(price)
            };
            m.halt_high = m.halt_high.max(price);
            let run = match m.run {
                Some(r)
                    if tau.saturating_sub(r.last_time_ms) <= p.max_staleness_ms
                        && within_bps(
                            u128::from(price),
                            u128::from(r.last),
                            p.agree_bps,
                            u128::from(r.last),
                        ) =>
                {
                    Run {
                        last: price,
                        count: r.count.saturating_add(1),
                        last_time_ms: tau,
                    }
                }
                _ => Run {
                    last: price,
                    count: 1,
                    last_time_ms: tau,
                },
            };
            if run.count >= p.confirm_rounds {
                m.index = price;
                m.index_time_ms = tau;
                m.status = Status::Active;
                m.run = None;
                m.halt_low = 0;
                m.halt_high = 0;
                MarketOutcome::Resumed {
                    price,
                    confirmed: true,
                }
            } else {
                m.run = Some(run);
                MarketOutcome::Confirming {
                    price,
                    count: run.count,
                }
            }
        }
    }
}

/// Where an exit price comes from (README section 6a).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExitSource {
    /// The market is active: the index itself.
    Index,
    /// The market is halted: the band around the last good index and every committee price since.
    HaltBand,
}

/// The defined price path for exits (withdrawal of free collateral, forced closes) in every oracle state except
/// genesis (R395-5). The engine values a position at the side of the band that is worse for its holder: a long at
/// `low`, a short at `high`; a close of ONE account while halted settles at that side ([`ExitPrice::close_price`],
/// R436-10). `settle` is only for X3, the settlement of every position of the market at once.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ExitPrice {
    pub source: ExitSource,
    pub low: u64,
    pub high: u64,
    /// The last good index and its age at the block time.
    pub reference: u64,
    pub age_ms: u64,
    /// X2: the settlement price of X3 (every position of the market at once): the latest committee price since
    /// the halt, else the last good index; the index itself while active. NEVER the price of one account's exit
    /// (R436-10): whoever controls the halt would hold a free option at the frozen index.
    pub settle: u64,
    /// The time of the committee price that `settle` is, when it is one (halted, and the committee has signed a
    /// price since the halt). `None` while active and when `settle` is only the pre-halt index (nothing signed
    /// since the halt: the state a sequencer reaches alone by withholding oracle inputs).
    pub settle_committee_time_ms: Option<u64>,
    /// X3: no good index for `max_halt_ms`: the market is due for settlement of every position at the band.
    pub settle_due: bool,
}

impl ExitPrice {
    /// The price at which ONE account leaves a position of this market (valuation for a withdrawal, a forced
    /// close): the side of the band worse for it, a long at `low`, a short at `high`. Equal to the index while
    /// the market is active.
    pub fn close_price(&self, long: bool) -> u64 {
        if long {
            self.low
        } else {
            self.high
        }
    }
}

/// X1 to X3 for `market_id` at block time `t`. `None` only before the first index (genesis `INIT`: no trade can
/// have happened, so no position exists) or for an unknown market.
pub fn exit_price(
    params: &CommitteeParams,
    state: &OracleState,
    market_id: u32,
    t: u64,
) -> Option<ExitPrice> {
    let i = params
        .markets
        .iter()
        .position(|m| m.market_id == market_id)?;
    let (p, m) = (&params.markets[i], &state.markets[i]);
    if m.index == 0 {
        return None;
    }
    let age_ms = t.saturating_sub(m.index_time_ms);
    if m.status == Status::Active {
        return Some(ExitPrice {
            source: ExitSource::Index,
            low: m.index,
            high: m.index,
            reference: m.index,
            age_ms,
            settle: m.index,
            settle_committee_time_ms: None,
            settle_due: false,
        });
    }
    // X1: half-width h = min(max, base + perMin * whole minutes since the last good index), in bps.
    let minutes = age_ms / 60_000;
    let h = u64::from(p.exit_haircut_bps)
        .saturating_add(u64::from(p.exit_haircut_per_min_bps).saturating_mul(minutes))
        .min(u64::from(p.exit_max_haircut_bps));
    let (lo_ref, hi_ref) = if m.halt_low == 0 {
        (m.index, m.index)
    } else {
        (m.index.min(m.halt_low), m.index.max(m.halt_high))
    };
    // Outward rounding: low down, high up. u128: 2^64 * 2 * 10^4 < 2^128.
    let low = u128::from(lo_ref) * u128::from(10_000 - h) / 10_000;
    let high = (u128::from(hi_ref) * u128::from(10_000 + h)).div_ceil(10_000);
    Some(ExitPrice {
        source: ExitSource::HaltBand,
        // h < 10000 (validated) and lo_ref >= 1: low >= 0; never 0 for a price >= 2 (stated: a 1-unit index at
        // a 50% band floors to 0, clamped to 1).
        low: u64::try_from(low).unwrap_or(u64::MAX).max(1),
        high: u64::try_from(high).unwrap_or(u64::MAX),
        reference: m.index,
        age_ms,
        // X2: settlement at the latest committee price since the halt, else the last good index.
        settle: m.run.map(|r| r.last).unwrap_or(m.index),
        settle_committee_time_ms: m.run.map(|r| r.last_time_ms),
        // X3
        settle_due: age_ms >= p.max_halt_ms,
    })
}

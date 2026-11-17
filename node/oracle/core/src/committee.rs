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
}

/// Oracle parameters fixed at genesis.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CommitteeParams {
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
}

impl CommitteeParams {
    /// Refuses parameters under which the rules lose their meaning. Called by [`OracleState::genesis`].
    pub fn validate(&self) -> Result<(), ParamError> {
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
                })
                .collect(),
        })
    }

    pub fn market(&self, market_id: u32) -> Option<&MarketOracle> {
        self.markets.iter().find(|m| m.market_id == market_id)
    }

    /// The price the engine may use for `market_id`: `Some(index)` only while the market is active.
    pub fn safe_index(&self, market_id: u32) -> Option<u64> {
        self.market(market_id).filter(|m| m.status == Status::Active).map(|m| m.index)
    }

    /// Canonical bytes of the oracle state, for the engine's state commitment:
    /// `lastRound:u64be count:u16be { marketId:u32be index:u64be indexTimeMs:u64be status:u8 hasRun:u8
    /// [runLast:u64be runCount:u8 runLastTimeMs:u64be] }`, status 0 active, 1 init, 2 stale, 3 jump.
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
        }
        b
    }
}

/// H1, at the start of every block before its inputs: an active market whose index is older than its staleness
/// bound halts with `STALE`. Returns the markets halted by this call.
pub fn begin_block(params: &CommitteeParams, state: &mut OracleState, block_time_ms: u64) -> Vec<u32> {
    let mut halted = Vec::new();
    for (m, p) in state.markets.iter_mut().zip(&params.markets) {
        if m.status == Status::Active && block_time_ms.saturating_sub(m.index_time_ms) > p.max_staleness_ms {
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
    /// C2c `ORACLE_TIME`, with the index of the observation in the input.
    Time { observation: usize },
    /// C2b `ORACLE_BAD_SIGNATURE`, with the index of the observation in the input.
    BadSignature { observation: usize, reason: Option<SigError> },
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
}

fn validate(params: &CommitteeParams, last_round: u64, u: &OracleUpdate, t: u64) -> Result<(), OracleError> {
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
    let start = u.round.checked_mul(ROUND_MS).ok_or(OracleError::Malformed(Malformed::RoundOverflow))?;
    let end = start.checked_add(ROUND_MS).ok_or(OracleError::Malformed(Malformed::RoundOverflow))?;
    let domain = eip712::domain_separator();
    for (i, o) in u.observations.iter().enumerate() {
        // C2c
        if o.time_ms < start || o.time_ms >= end || o.time_ms > t.saturating_add(params.max_future_ms) || t.saturating_sub(o.time_ms) > params.max_age_ms {
            return Err(OracleError::Time { observation: i });
        }
        // C2b
        let digest = eip712::digest_with_domain(&domain, &o.message(u.round));
        match sig::recover(&digest, &o.signature) {
            Ok(a) if a == params.signers[usize::from(o.signer)] => {}
            Ok(_) => return Err(OracleError::BadSignature { observation: i, reason: None }),
            Err(e) => return Err(OracleError::BadSignature { observation: i, reason: Some(e) }),
        }
    }
    Ok(())
}

/// Applies one `oracle` block input at block time `t` (C1 to C5, then H2/H3 per market). On error nothing changes.
pub fn apply_update(params: &CommitteeParams, state: &mut OracleState, u: &OracleUpdate, t: u64) -> Result<UpdateReport, OracleError> {
    validate(params, state.last_round, u, t)?;
    state.last_round = u.round;
    let tau = u.round * ROUND_MS; // no overflow: checked in validate
    let q = usize::from(params.quorum);
    let mut outcomes = Vec::with_capacity(params.markets.len());
    for (m, p) in state.markets.iter_mut().zip(&params.markets) {
        // C3
        let mut v: Vec<u64> = u
            .observations
            .iter()
            .filter_map(|o| o.prices.iter().find(|e| e.market_id == p.market_id))
            .filter(|e| e.sources >= p.min_sources)
            .map(|e| e.price)
            .collect();
        if v.len() < q {
            outcomes.push((p.market_id, MarketOutcome::NoQuorum { have: v.len() as u8 }));
            continue;
        }
        v.sort_unstable();
        let med = v[v.len() / 2];
        // C4
        let agree = v.iter().filter(|&&x| within_bps(u128::from(x), u128::from(med), p.agree_bps, u128::from(med))).count();
        if agree < q {
            outcomes.push((p.market_id, MarketOutcome::Disagree { median: med, agree: agree as u8 }));
            continue;
        }
        // C5: the halt machine consumes (med, tau).
        outcomes.push((p.market_id, step(p, m, med, tau)));
    }
    Ok(UpdateReport { round: u.round, outcomes })
}

fn step(p: &MarketParams, m: &mut MarketOracle, price: u64, tau: u64) -> MarketOutcome {
    let near_index = m.index > 0 && within_bps(u128::from(price), u128::from(m.index), p.jump_bps, u128::from(m.index));
    match m.status {
        // H2
        Status::Active => {
            if near_index {
                m.index = price;
                m.index_time_ms = tau;
                MarketOutcome::Moved { price }
            } else {
                m.status = Status::Halted(HaltReason::Jump);
                m.run = Some(Run { last: price, count: 1, last_time_ms: tau });
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
                return MarketOutcome::Resumed { price, confirmed: false };
            }
            let run = match m.run {
                Some(r)
                    if tau.saturating_sub(r.last_time_ms) <= p.max_staleness_ms
                        && within_bps(u128::from(price), u128::from(r.last), p.agree_bps, u128::from(r.last)) =>
                {
                    Run { last: price, count: r.count.saturating_add(1), last_time_ms: tau }
                }
                _ => Run { last: price, count: 1, last_time_ms: tau },
            };
            if run.count >= p.confirm_rounds {
                m.index = price;
                m.index_time_ms = tau;
                m.status = Status::Active;
                m.run = None;
                MarketOutcome::Resumed { price, confirmed: true }
            } else {
                m.run = Some(run);
                MarketOutcome::Confirming { price, count: run.count }
            }
        }
    }
}

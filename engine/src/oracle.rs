//! Committee oracle rules C1 to C5 and halt rules H1 to H3, as normative in agent 394's
//! `node/oracle/README.md` sections 4 to 6 (SPEC S-5.1). Integer arithmetic only.
use crate::eip712::{digest, oracle_struct_hash, recover};
use crate::genesis::OracleParams;
use crate::hash::{Addr, H256};
use crate::types::{Error, OracleUpdate};

pub const ROUND_MS: u64 = 3000;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Status {
    Active,
    Halted(HaltReason),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum HaltReason {
    /// No index yet (genesis) or index too old (H1).
    Stale,
    Jump,
    /// S-0.2: arithmetic overflow in a system step.
    Arithmetic,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct OracleState {
    pub index: u64,
    pub index_time: u64,
    pub status: Status,
    /// confirmation run (last, count, lastTimeMs)
    pub run: Option<(u64, u32, u64)>,
    /// X2: lowest / highest committee price since the halt (0 = none); only widen, cleared on resume.
    pub halt_low: u64,
    pub halt_high: u64,
}

impl Default for OracleState {
    fn default() -> Self {
        OracleState { index: 0, index_time: 0, status: Status::Halted(HaltReason::Stale), run: None, halt_low: 0, halt_high: 0 }
    }
}

/// 394's `exit_price` (README 6a, X0 to X3).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ExitPrice {
    pub low: u64,
    pub high: u64,
    pub settle: u64,
    pub settle_due: bool,
}

/// `|a - b| * 10000 <= bps * b`, in u128.
pub fn within_bps(a: u64, b: u64, bps: u32) -> bool {
    let d = (a as i128 - b as i128).unsigned_abs();
    d * 10000 <= (bps as u128) * (b as u128)
}

impl OracleState {
    pub fn exit_price(&self, p: &OracleParams, t: u64) -> Option<ExitPrice> {
        if self.index == 0 {
            return None;
        }
        let age = t.saturating_sub(self.index_time);
        if self.status == Status::Active {
            return Some(ExitPrice { low: self.index, high: self.index, settle: self.index, settle_due: false });
        }
        let h = (p.exit_haircut_bps as u64).saturating_add((p.exit_haircut_per_min_bps as u64).saturating_mul(age / 60_000)).min(p.exit_max_haircut_bps as u64);
        let (lo, hi) = if self.halt_low == 0 { (self.index, self.index) } else { (self.index.min(self.halt_low), self.index.max(self.halt_high)) };
        let low = (lo as u128 * (10_000 - h as u128) / 10_000) as u64;
        let high = (hi as u128 * (10_000 + h as u128)).div_ceil(10_000);
        Some(ExitPrice {
            low: low.max(1),
            high: u64::try_from(high).unwrap_or(u64::MAX),
            settle: self.run.map(|r| r.0).unwrap_or(self.index),
            settle_due: age >= p.max_halt_ms,
        })
    }

    /// H1, at the start of every block.
    pub fn h1(&mut self, t: u64, p: &OracleParams) {
        if self.status == Status::Active && t.saturating_sub(self.index_time) > p.max_staleness_ms {
            self.status = Status::Halted(HaltReason::Stale);
            self.run = None;
        }
    }

    /// H2 / H3 for a committee price `(m, tau)`.
    pub fn consume(&mut self, m: u64, tau: u64, p: &OracleParams) {
        match self.status {
            Status::Active => {
                if within_bps(m, self.index, p.jump_bps) {
                    self.index = m;
                    self.index_time = tau;
                } else {
                    self.status = Status::Halted(HaltReason::Jump);
                    self.run = Some((m, 1, tau));
                    self.halt_low = m;
                    self.halt_high = m;
                }
            }
            Status::Halted(HaltReason::Arithmetic) => {
                // Not resumable by prices: a bug halt stays until a new engine version (SPEC S-0.2).
            }
            Status::Halted(_) => {
                if self.index > 0
                    && tau.saturating_sub(self.index_time) <= p.max_staleness_ms
                    && within_bps(m, self.index, p.jump_bps)
                {
                    self.index = m;
                    self.index_time = tau;
                    self.status = Status::Active;
                    self.run = None;
                    self.halt_low = 0;
                    self.halt_high = 0;
                    return;
                }
                self.halt_low = if self.halt_low == 0 { m } else { self.halt_low.min(m) };
                self.halt_high = self.halt_high.max(m);
                let next = match self.run {
                    Some((last, count, lt)) if tau.saturating_sub(lt) <= p.max_staleness_ms && within_bps(m, last, p.agree_bps) => {
                        (m, count + 1, tau)
                    }
                    _ => (m, 1, tau),
                };
                if next.1 >= p.confirm_rounds {
                    self.index = m;
                    self.index_time = tau;
                    self.status = Status::Active;
                    self.run = None;
                    self.halt_low = 0;
                    self.halt_high = 0;
                } else {
                    self.run = Some(next);
                }
            }
        }
    }
}

/// C1, C2, C2b, C2c over the whole input, then C3 and C4 per market. Returns the committee prices
/// `(marketId, M)` for the markets that reached quorum and agreement, ascending by marketId, with `tau`.
pub fn committee_prices(
    u: &OracleUpdate,
    t: u64,
    last_round: Option<u64>,
    signers: &[Addr],
    quorum: usize,
    max_future_ms: u64,
    max_age_ms: u64,
    domain_sep: &H256,
    markets: &[(u32, &OracleParams)],
) -> Result<(Vec<(u32, u64)>, u64), Error> {
    if let Some(lr) = last_round {
        if u.round <= lr {
            return Err(Error::OracleRoundNotNew);
        }
    }
    let n = signers.len();
    let c = u.observations.len();
    if c < quorum || c > n {
        return Err(Error::OracleMalformed);
    }
    let mut prev_signer: Option<u8> = None;
    for o in &u.observations {
        if (o.signer as usize) >= n || prev_signer.is_some_and(|p| o.signer <= p) {
            return Err(Error::OracleMalformed);
        }
        prev_signer = Some(o.signer);
        if o.prices.is_empty() || o.prices.len() > 256 {
            return Err(Error::OracleMalformed);
        }
        let mut pm: Option<u32> = None;
        for p in &o.prices {
            if p.price == 0 || pm.is_some_and(|x| p.market_id <= x) {
                return Err(Error::OracleMalformed);
            }
            pm = Some(p.market_id);
        }
    }
    let round_start = u.round.checked_mul(ROUND_MS).ok_or(Error::OracleMalformed)?;
    for o in &u.observations {
        let d = digest(domain_sep, &oracle_struct_hash(u.round, o.time_ms, &o.prices));
        if recover(&d, &o.signature) != Some(signers[o.signer as usize]) {
            return Err(Error::OracleBadSignature);
        }
        if o.time_ms < round_start || o.time_ms >= round_start + ROUND_MS {
            return Err(Error::OracleTime);
        }
        if o.time_ms > t.saturating_add(max_future_ms) || (o.time_ms <= t && t - o.time_ms > max_age_ms) {
            return Err(Error::OracleTime);
        }
    }
    let mut out = Vec::new();
    for (mid, p) in markets {
        let mut v: Vec<u64> = u
            .observations
            .iter()
            .filter_map(|o| o.prices.iter().find(|x| x.market_id == *mid))
            .filter(|x| x.sources >= p.min_sources)
            // R430-2 (0.2): a value above MAX_PRICE (SPEC section 1) is not a price of this engine: the observation
            // does not count for the market (as if its sources were too few), so a committee that signs such a
            // value gets no index and the market goes stale and halts (fail closed) instead of an index outside
            // the bound every product of section 1 relies on.
            .filter(|x| x.price <= crate::num::MAX_PRICE)
            .map(|x| x.price)
            .collect();
        if v.len() < quorum {
            continue; // NO_QUORUM
        }
        v.sort_unstable();
        let m = v[v.len() / 2];
        let a = v.iter().filter(|x| within_bps(**x, m, p.agree_bps)).count();
        if a < quorum {
            continue; // DISAGREE
        }
        out.push((*mid, m));
    }
    Ok((out, round_start))
}

#[cfg(test)]
mod tests {
    use super::*;
    fn params() -> OracleParams {
        OracleParams { min_sources: 3, agree_bps: 100, jump_bps: 500, max_staleness_ms: 10_000, confirm_rounds: 3, exit_haircut_bps: 500, exit_haircut_per_min_bps: 100, exit_max_haircut_bps: 5000, max_halt_ms: 3_600_000 }
    }
    #[test]
    fn genesis_needs_confirmation_then_bounded_steps() {
        let p = params();
        let mut s = OracleState::default();
        s.consume(100_000, 3000, &p);
        s.consume(100_100, 6000, &p);
        assert_eq!(s.status, Status::Halted(HaltReason::Stale));
        s.consume(100_050, 9000, &p);
        assert_eq!(s.status, Status::Active);
        assert_eq!(s.index, 100_050);
        // 6% jump halts, index does not move
        s.consume(106_100, 12000, &p);
        assert_eq!(s.status, Status::Halted(HaltReason::Jump));
        assert_eq!(s.index, 100_050);
        // reverts while fresh: fast return
        s.consume(100_200, 15000, &p);
        assert_eq!(s.status, Status::Active);
        assert_eq!(s.index, 100_200);
    }
    #[test]
    fn stale_halts() {
        let p = params();
        let mut s = OracleState { index: 1, index_time: 0, status: Status::Active, run: None, halt_low: 0, halt_high: 0 };
        s.h1(10_000, &p);
        assert_eq!(s.status, Status::Active);
        s.h1(10_001, &p);
        assert_eq!(s.status, Status::Halted(HaltReason::Stale));
    }
}

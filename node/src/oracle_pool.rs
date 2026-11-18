//! Oracle ingest: signed observations from the committee's feeders (agent 432's `node/oracle/feeder`), gathered
//! per round and handed to the sequencer as one `OracleUpdate` input. Wire form agreed with 432 in the cross-area
//! log (2026-11-18): `POST /v1/oracle/observations` (one signer) and `POST /v1/oracle/updates` (a whole update),
//! `Authorization: Bearer <token>` from a token FILE.
//!
//! The node RELAYS; the ENGINE decides (oracle/README rules C1 to C5, SPEC S-5.1). What the pool does on its own:
//!
//! - pre-filter, so garbage never occupies a slot or a block: `round == timeMs / 3000`, the shape rules of C2
//!   (1 to 256 prices, market ids strictly ascending, every price > 0), `signer < n`, and the signature recovers
//!   to `oracleCommittee[signer]` over the ENGINE crate's own observation digest under the engine domain
//!   `{PosternPerps, 1, 700770, vault}`. A pre-filter can only refuse: an observation it passes is still verified
//!   by every validator's engine;
//! - one observation per (round, signer): the first is kept; the identical one again is idempotent; a DIFFERENT
//!   second one is refused (`ORACLE_CONFLICT`), counted and logged (a signer that equivocates inside a round);
//! - no selection (R395-9): a round goes into a block with EVERY observation held for it, signer ascending. It is
//!   ready when all `n` signers arrived, or at least `q` arrived and `gather_ms` passed since the q-th (so the
//!   sequencer does not cut the round at the first quorum it likes). The residual bias is the sequencer's choice
//!   of WHEN inside that window to build the block, bounded by `gather_ms`; an observation the sequencer claims
//!   it never received cannot be told from a late one by this node (432's round digest is the stronger answer);
//! - time (R436-12): the engine refuses a whole update when ONE observation is outside its time bound at the
//!   block's time, so the pool hands over only observations the engine accepts at that time, defers a round
//!   while a held observation is still in the engine's future, and leaves out what can never be accepted
//!   (`take_ready_at`). One late or fast-clock signer delays a round by at most `3000 - maxFutureMs` ms;
//! - rounds at or below the engine's last accepted round are refused (`ORACLE_ROUND_NOT_NEW`), rounds further
//!   than one ahead of the wall clock are refused, and at most `MAX_ROUNDS` rounds are held.
//!
//! UNAUDITED PROTOTYPE.

use crate::genesis::Genesis;
use crate::types::{OraclePrice, OracleUpdate, SignedObservation};
use crate::util::{parse_hex_fixed, to_hex};
use postern_perps_engine as pe;
use std::collections::BTreeMap;

pub const ROUND_MS: u64 = 3000;
pub const MAX_ROUNDS: usize = 8;
pub const DEFAULT_GATHER_MS: u64 = 500;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Refusal {
    pub http: u16,
    pub code: &'static str,
    pub message: String,
}

fn refuse(http: u16, code: &'static str, message: impl Into<String>) -> Refusal {
    Refusal { http, code, message: message.into() }
}

#[derive(Clone, Debug, Default)]
struct Round {
    obs: BTreeMap<u8, SignedObservation>,
    /// wall-clock ms at which the q-th observation arrived
    quorum_at: Option<u64>,
}

#[derive(Clone, Debug)]
pub struct OraclePool {
    rounds: BTreeMap<u64, Round>,
    pub gather_ms: u64,
    pub conflicts: u64,
    pub accepted: u64,
    pub refused: u64,
    pub rounds_built: u64,
    /// observations held for a built round and not handed over, because the engine refuses them at the block's
    /// time (too old, or in its future with no time left to wait)
    pub left_out: u64,
    /// signers seen in the last round handed to the sequencer
    pub last_built: Option<(u64, Vec<u8>)>,
}

impl Default for OraclePool {
    fn default() -> Self {
        OraclePool { rounds: BTreeMap::new(), gather_ms: DEFAULT_GATHER_MS, conflicts: 0, accepted: 0, refused: 0, rounds_built: 0, left_out: 0, last_built: None }
    }
}

/// The digest an oracle signer signs, computed by the ENGINE crate (the function its acceptance rule uses).
pub fn observation_digest(g: &Genesis, round: u64, o: &SignedObservation) -> [u8; 32] {
    let prices: Vec<pe::OraclePrice> = o.prices.iter().map(|p| pe::OraclePrice { market_id: p.market_id, price: p.price, sources: p.sources }).collect();
    pe::eip712::digest(&g.engine_domain.separator(), &pe::eip712::oracle_struct_hash(round, o.time_ms, &prices))
}

fn num(v: &serde_json::Value, what: &str, max: u64) -> Result<u64, Refusal> {
    let n = match v {
        serde_json::Value::Number(n) => n.as_u64(),
        serde_json::Value::String(s) if !s.is_empty() && s.len() <= 20 && s.bytes().all(|c| c.is_ascii_digit()) && (s == "0" || !s.starts_with('0')) => s.parse::<u64>().ok(),
        _ => None,
    };
    match n {
        Some(n) if n <= max => Ok(n),
        _ => Err(refuse(400, "ORACLE_MALFORMED", format!("{what}: an unsigned integer up to {max} (JSON number or canonical decimal string)"))),
    }
}

fn only_keys(v: &serde_json::Value, allowed: &[&str], what: &str) -> Result<(), Refusal> {
    let o = v.as_object().ok_or_else(|| refuse(400, "ORACLE_MALFORMED", format!("{what}: an object")))?;
    for k in o.keys() {
        if !allowed.contains(&k.as_str()) {
            return Err(refuse(400, "ORACLE_MALFORMED", format!("{what}: unknown field {k:?}")));
        }
    }
    for k in allowed {
        if !o.contains_key(*k) {
            return Err(refuse(400, "ORACLE_MALFORMED", format!("{what}: missing field {k:?}")));
        }
    }
    Ok(())
}

/// Parses one observation of oracle/README section 4's JSON form (strict: unknown or missing fields refused).
pub fn parse_observation(v: &serde_json::Value) -> Result<SignedObservation, Refusal> {
    only_keys(v, &["signer", "timeMs", "prices", "signature"], "observation")?;
    let signer = num(&v["signer"], "signer", 254)? as u8;
    let time_ms = num(&v["timeMs"], "timeMs", u64::MAX)?;
    let arr = v["prices"].as_array().ok_or_else(|| refuse(400, "ORACLE_MALFORMED", "prices: an array"))?;
    if arr.is_empty() || arr.len() > 256 {
        return Err(refuse(400, "ORACLE_MALFORMED", "prices: 1 to 256 entries"));
    }
    let mut prices = Vec::with_capacity(arr.len());
    let mut prev: Option<u32> = None;
    for p in arr {
        only_keys(p, &["marketId", "price", "sources"], "price")?;
        let market_id = num(&p["marketId"], "marketId", u32::MAX as u64)? as u32;
        let price = num(&p["price"], "price", u64::MAX)?;
        let sources = num(&p["sources"], "sources", 255)? as u8;
        if price == 0 {
            return Err(refuse(400, "ORACLE_MALFORMED", "price: never 0"));
        }
        if prev.is_some_and(|x| market_id <= x) {
            return Err(refuse(400, "ORACLE_MALFORMED", "prices: marketId strictly ascending"));
        }
        prev = Some(market_id);
        prices.push(OraclePrice { market_id, price, sources });
    }
    let signature = v["signature"].as_str().map(parse_hex_fixed::<65>).and_then(|r| r.ok()).ok_or_else(|| refuse(400, "ORACLE_MALFORMED", "signature: 65 bytes of 0x hex"))?;
    Ok(SignedObservation { signer, time_ms, prices, signature })
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Held {
    /// stored; the round is not ready yet
    Held(usize),
    /// stored (or already held); the round can go into the next block
    Ready(usize),
}

impl OraclePool {
    fn is_ready(g: &Genesis, r: &Round, gather_ms: u64, now_ms: u64) -> bool {
        r.obs.len() >= g.oracle_committee.len() || r.quorum_at.is_some_and(|t| now_ms >= t.saturating_add(gather_ms))
    }

    /// Offers one observation of `round`. `last_round` is the engine's last accepted round.
    pub fn offer(&mut self, g: &Genesis, round: u64, o: SignedObservation, last_round: u64, now_ms: u64) -> Result<Held, Refusal> {
        let r = self.offer_inner(g, round, o, last_round, now_ms);
        match &r {
            Ok(_) => {}
            Err(e) if e.code == "ORACLE_CONFLICT" => self.conflicts += 1,
            Err(_) => self.refused += 1,
        }
        r
    }

    fn offer_inner(&mut self, g: &Genesis, round: u64, o: SignedObservation, last_round: u64, now_ms: u64) -> Result<Held, Refusal> {
        if o.time_ms / ROUND_MS != round {
            return Err(refuse(400, "ORACLE_MALFORMED", "round must be timeMs / 3000"));
        }
        if round <= last_round {
            return Err(refuse(409, "ORACLE_ROUND_NOT_NEW", format!("round {round} is not above the engine's last accepted round {last_round}")));
        }
        if round > now_ms / ROUND_MS + 1 {
            return Err(refuse(400, "ORACLE_TIME", "round is more than one round ahead of this node's clock"));
        }
        let n = g.oracle_committee.len();
        if o.signer as usize >= n {
            return Err(refuse(400, "ORACLE_MALFORMED", format!("signer index must be below {n}")));
        }
        if o.prices.is_empty() || o.prices.len() > 256 || o.prices.iter().any(|p| p.price == 0) || o.prices.windows(2).any(|w| w[1].market_id <= w[0].market_id) {
            return Err(refuse(400, "ORACLE_MALFORMED", "prices: 1 to 256 entries, marketId strictly ascending, price > 0"));
        }
        let d = observation_digest(g, round, &o);
        match crate::sig::recover(&d, &o.signature) {
            Ok(a) if a == g.oracle_committee[o.signer as usize] => {}
            Ok(_) => return Err(refuse(400, "ORACLE_BAD_SIGNATURE", "the signature does not recover to oracleCommittee[signer] under this deployment's engine domain")),
            Err(e) => return Err(refuse(400, "ORACLE_BAD_SIGNATURE", e.to_string())),
        }
        if !self.rounds.contains_key(&round) && self.rounds.len() >= MAX_ROUNDS {
            // keep the newest rounds; an older round than everything held is not stored
            let oldest = *self.rounds.keys().next().unwrap();
            if round < oldest {
                return Err(refuse(409, "ORACLE_ROUND_NOT_NEW", "older than every round held"));
            }
            self.rounds.remove(&oldest);
        }
        let q = g.oracle_quorum;
        let gather = self.gather_ms;
        let r = self.rounds.entry(round).or_default();
        if let Some(have) = r.obs.get(&o.signer) {
            if *have != o {
                eprintln!("oracle ingest: CONFLICT: signer {} sent two different observations for round {round} (first kept)", o.signer);
                return Err(refuse(409, "ORACLE_CONFLICT", "a different observation of this signer is already held for this round (the first is kept)"));
            }
        } else {
            r.obs.insert(o.signer, o);
            self.accepted += 1;
            if r.obs.len() >= q && r.quorum_at.is_none() {
                r.quorum_at = Some(now_ms);
            }
        }
        let held = r.obs.len();
        Ok(if Self::is_ready(g, r, gather, now_ms) { Held::Ready(held) } else { Held::Held(held) })
    }

    /// The newest ready round above `last_round`, with every observation held for it (signer ascending). The
    /// round and everything older leave the pool.
    pub fn take_ready(&mut self, g: &Genesis, last_round: u64, now_ms: u64) -> Option<OracleUpdate> {
        self.rounds.retain(|r, _| *r > last_round);
        let gather = self.gather_ms;
        let round = *self.rounds.iter().rev().find(|(_, r)| Self::is_ready(g, r, gather, now_ms)).map(|(k, _)| k)?;
        let r = self.rounds.remove(&round)?;
        self.rounds.retain(|k, _| *k > round);
        self.rounds_built += 1;
        self.last_built = Some((round, r.obs.keys().copied().collect()));
        Some(OracleUpdate { round, observations: r.obs.into_values().collect() })
    }

    /// As `take_ready`, for a block whose engine time will be `block_time`, under the engine's time bounds
    /// (oracle/README C2c, the engine's own test: an observation is refused when `timeMs > T + maxFutureMs` or
    /// `T - timeMs > maxAgeMs`, and ONE refused observation refuses the whole update). R436-12:
    ///
    /// - only observations the engine accepts at `block_time` are handed over. This is not selection: the rule is
    ///   the engine's, a function of the observation's own signed time and the block time;
    /// - an observation still in the engine's future DEFERS the round until it is acceptable (at most until the
    ///   round's last `maxFutureMs`), so a late signer costs a delay, never the round;
    /// - unless waiting would let an acceptable one expire first: then the round is built now without the future
    ///   ones; observations already too old are left out. Both are counted in `left_out`.
    pub fn take_ready_at(&mut self, g: &Genesis, last_round: u64, now_ms: u64, block_time: u64, max_future_ms: u64, max_age_ms: u64) -> Option<OracleUpdate> {
        self.rounds.retain(|r, _| *r > last_round);
        let gather = self.gather_ms;
        let (n, q) = (g.oracle_committee.len(), g.oracle_quorum);
        let t = block_time;
        let in_future = |o: &SignedObservation| o.time_ms > t.saturating_add(max_future_ms);
        let too_old = |o: &SignedObservation| o.time_ms <= t && t - o.time_ms > max_age_ms;
        let mut chosen: Option<(u64, Vec<u8>)> = None;
        for (round, r) in self.rounds.iter().rev() {
            let gathered = r.obs.len() >= n || r.quorum_at.is_some_and(|x| now_ms >= x.saturating_add(gather));
            if !gathered {
                continue;
            }
            let valid: Vec<&SignedObservation> = r.obs.values().filter(|o| !in_future(o) && !too_old(o)).collect();
            if valid.len() < q {
                continue;
            }
            let latest_future = r.obs.values().filter(|o| in_future(o)).map(|o| o.time_ms).max();
            if let Some(lf) = latest_future {
                // every held observation is acceptable from T' = lf - maxFuture on; the first acceptable one to
                // expire does so after min(timeMs) + maxAge
                let all_acceptable_from = lf.saturating_sub(max_future_ms);
                let first_expiry = valid.iter().map(|o| o.time_ms).min().unwrap_or(0).saturating_add(max_age_ms);
                if all_acceptable_from <= first_expiry {
                    continue; // wait: nothing is lost by waiting
                }
            }
            chosen = Some((*round, valid.iter().map(|o| o.signer).collect()));
            break;
        }
        let (round, signers) = chosen?;
        let r = self.rounds.remove(&round)?;
        self.rounds.retain(|k, _| *k > round);
        self.rounds_built += 1;
        self.left_out += (r.obs.len() - signers.len()) as u64;
        if r.obs.len() != signers.len() {
            eprintln!("oracle ingest: round {round}: {} of {} held observations left out (outside the engine's time bound at block time {t})", r.obs.len() - signers.len(), r.obs.len());
        }
        self.last_built = Some((round, signers.clone()));
        Some(OracleUpdate { round, observations: r.obs.into_iter().filter(|(s, _)| signers.contains(s)).map(|(_, o)| o).collect() })
    }

    /// Signers held for the newest round (for `OracleStatus.signers`).
    pub fn newest_held(&self) -> usize {
        self.rounds.values().next_back().map(|r| r.obs.len()).unwrap_or(0)
    }

    pub fn to_json(&self, g: &Genesis) -> serde_json::Value {
        serde_json::json!({
            "quorum": g.oracle_quorum, "signers": g.oracle_committee.iter().map(|a| to_hex(a)).collect::<Vec<_>>(),
            "gatherMs": self.gather_ms, "accepted": self.accepted, "refused": self.refused, "conflicts": self.conflicts,
            "roundsBuilt": self.rounds_built, "leftOut": self.left_out,
            "lastBuilt": self.last_built.as_ref().map(|(r, s)| serde_json::json!({"round": r.to_string(), "signers": s})),
            "held": self.rounds.iter().rev().map(|(r, x)| serde_json::json!({"round": r.to_string(), "signers": x.obs.keys().collect::<Vec<_>>(),
                "quorumAtMs": x.quorum_at})).collect::<Vec<_>>(),
        })
    }
}

/// Constant-time comparison of the presented bearer token with the configured one.
pub fn token_ok(configured: &[u8], presented: &[u8]) -> bool {
    // compare keccak digests: equal length, and no early exit on the first differing byte of the token itself
    let (a, b) = (crate::util::keccak(configured), crate::util::keccak(presented));
    let mut diff = 0u8;
    for i in 0..32 {
        diff |= a[i] ^ b[i];
    }
    diff == 0
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn observation_json_is_strict() {
        let sig = format!("0x{}", "11".repeat(65));
        let ok = serde_json::json!({"signer": 0, "timeMs": 1794887290471u64, "prices": [{"marketId": 1, "price": "6499240000000", "sources": 7}], "signature": sig});
        let o = parse_observation(&ok).unwrap();
        assert_eq!((o.signer, o.time_ms, o.prices[0].price, o.prices[0].sources), (0, 1794887290471, 6_499_240_000_000, 7));
        // a JSON number price is accepted too
        let mut n = ok.clone();
        n["prices"][0]["price"] = serde_json::json!(6499240000000u64);
        assert_eq!(parse_observation(&n).unwrap(), o);
        for (path, bad) in [
            ("signer", serde_json::json!(255)),
            ("signer", serde_json::json!(-1)),
            ("signer", serde_json::json!(0.5)),
            ("timeMs", serde_json::json!("01")),
            ("timeMs", serde_json::json!("1e3")),
            ("prices", serde_json::json!([])),
            ("signature", serde_json::json!("0x11")),
        ] {
            let mut v = ok.clone();
            v[path] = bad.clone();
            assert!(parse_observation(&v).is_err(), "{path} = {bad} must be refused");
        }
        let mut extra = ok.clone();
        extra["note"] = "x".into();
        assert!(parse_observation(&extra).is_err(), "unknown field refused");
        let mut zero = ok.clone();
        zero["prices"][0]["price"] = "0".into();
        assert!(parse_observation(&zero).is_err());
        let mut order = ok.clone();
        order["prices"] = serde_json::json!([{"marketId": 2, "price": "1", "sources": 1}, {"marketId": 2, "price": "1", "sources": 1}]);
        assert!(parse_observation(&order).is_err(), "market ids strictly ascending");
        let mut missing = ok.clone();
        missing.as_object_mut().unwrap().remove("timeMs");
        assert!(parse_observation(&missing).is_err());
    }

    #[test]
    fn token_comparison() {
        assert!(token_ok(b"secret-token", b"secret-token"));
        assert!(!token_ok(b"secret-token", b"secret-tokem"));
        assert!(!token_ok(b"secret-token", b""));
        assert!(!token_ok(b"secret-token", b"secret-token "));
    }
}

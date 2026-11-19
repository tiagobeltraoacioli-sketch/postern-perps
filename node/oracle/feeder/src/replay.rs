//! `replay-feed`: the deterministic replay source (README section 7b). It plays a tape (recorded venue data
//! converted by the feeder pipeline, or an authored script) into the node's oracle ingest as rounds signed by a
//! dev committee, with NO network access beyond the loopback socket of the node. UNAUDITED PROTOTYPE; development
//! and rehearsals only (one process holding a whole committee is exactly what production must never have).
//!
//! What is deterministic: the sequence of prices, sources, signers and overrides (a pure function of the tape text
//! and the options). What is not: `round` and `timeMs`, which are the wall clock of the run (the engine accepts an
//! observation only inside its own round and close to the block time: rules C2c), and therefore the signatures.
//! One tape step is played per oracle round (3 s of real time); `--speed K` plays every K-th step.

use std::collections::BTreeMap;
use std::io::Write;

use k256::ecdsa::SigningKey;
use perps_oracle::types::{round_of, Observation, OraclePrice, OracleUpdate, SignedObservation};
use perps_oracle::{eip712, sig, ROUND_MS};
use serde_json::{json, Value};

use crate::json as j;
use crate::submit::{NodeClient, OBSERVATIONS_PATH, UPDATES_PATH};
use crate::tape::{index_at, Step};

/// The signed round of one tape step at time `time_ms`: one observation per signing committee index, each over
/// the step's prices with that signer's overrides. `None` for a silent step, or when no signer has a price.
pub fn build_update(
    step: &Step,
    keys: &[SigningKey],
    vault: &[u8; 20],
    time_ms: u64,
) -> Option<OracleUpdate> {
    if step.silent {
        return None;
    }
    let domain = eip712::domain_separator(vault);
    let round = round_of(time_ms);
    let mut observations = Vec::new();
    for (i, key) in keys.iter().enumerate() {
        let signer = i as u8;
        if step.signers.as_ref().is_some_and(|s| !s.contains(&signer)) {
            continue;
        }
        let mut prices: BTreeMap<u32, (u64, u8)> = step.prices.clone();
        if let Some(ov) = step.overrides.get(&signer) {
            for (m, p) in ov {
                let s = prices.get(m).map_or(crate::tape::DEFAULT_SOURCES, |x| x.1);
                prices.insert(*m, (*p, s));
            }
        }
        if prices.is_empty() {
            continue;
        }
        let o = Observation {
            round,
            time_ms,
            prices: prices
                .iter()
                .map(|(m, (p, s))| OraclePrice {
                    market_id: *m,
                    price: *p,
                    sources: *s,
                })
                .collect(),
        };
        observations.push(SignedObservation {
            signer,
            time_ms,
            signature: sig::sign(key, &eip712::digest(&domain, &o)),
            prices: o.prices,
        });
    }
    if observations.is_empty() {
        return None;
    }
    Some(OracleUpdate {
        round,
        observations,
    })
}

/// The run's clock: the real one in the binary, a scripted one in tests.
pub trait Clock {
    fn now_ms(&mut self) -> u64;
    fn sleep_until_ms(&mut self, t: u64);
}

pub struct SystemClock;

impl Clock for SystemClock {
    fn now_ms(&mut self) -> u64 {
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("clock before 1970")
            .as_millis() as u64
    }
    fn sleep_until_ms(&mut self, t: u64) {
        let n = self.now_ms();
        if t > n {
            std::thread::sleep(std::time::Duration::from_millis(t - n));
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    /// One `POST /v1/oracle/updates` per round (the whole committee's update).
    Updates,
    /// One `POST /v1/oracle/observations` per signer per round (the production shape: each feeder on its own).
    Observations,
}

pub struct Options {
    pub speed: usize,
    pub looped: bool,
    pub mode: Mode,
    /// Milliseconds after the start of a round at which it is sampled and signed.
    pub offset_ms: u64,
    /// Stop after this many rounds (played or silent); `None` = to the end of the tape (never, when looped).
    pub max_rounds: Option<u64>,
}

#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct Summary {
    pub rounds: u64,
    pub silent: u64,
    /// Rounds whose every POST was answered 2xx.
    pub accepted: u64,
    /// Rounds with at least one POST answered by another status.
    pub refused: u64,
    /// Rounds with at least one POST that got no HTTP answer.
    pub unreachable: u64,
}

/// Plays the tape. One log line per round goes to `log`. Returns when the tape ends (or `max_rounds`): a node that
/// is down or refuses a round does not stop the run (a restart of the sequencer in the middle of a rehearsal must
/// not kill the feeder); the summary says what happened.
pub fn run(
    steps: &[Step],
    keys: &[SigningKey],
    vault: &[u8; 20],
    node: Option<&NodeClient>,
    opt: &Options,
    clock: &mut dyn Clock,
    log: &mut dyn Write,
) -> Result<Summary, String> {
    let mut sum = Summary::default();
    let mut last_round = 0u64;
    let mut k = 0u64;
    while let Some(i) = index_at(steps.len(), opt.speed, opt.looped, k) {
        if opt.max_rounds.is_some_and(|m| k >= m) {
            break;
        }
        // The next round not yet used, sampled `offset_ms` after its start.
        let now = clock.now_ms();
        let mut round = round_of(now);
        if round <= last_round || now % ROUND_MS > opt.offset_ms.max(ROUND_MS / 2) {
            round = round.max(last_round) + 1;
        }
        clock.sleep_until_ms(round * ROUND_MS + opt.offset_ms);
        let t = clock.now_ms();
        if round_of(t) != round {
            // Overslept (a stopped process, a loaded host): this round is lost; the step is played in the next.
            last_round = round_of(t).saturating_sub(1).max(round);
            writeln!(
                log,
                "{}",
                json!({"step": i, "missedRound": round, "nowMs": t})
            )
            .map_err(|e| e.to_string())?;
            continue;
        }
        last_round = round;
        k += 1;
        sum.rounds += 1;
        let Some(u) = build_update(&steps[i], keys, vault, t) else {
            sum.silent += 1;
            writeln!(
                log,
                "{}",
                json!({"step": i, "round": round, "timeMs": t, "silent": true})
            )
            .map_err(|e| e.to_string())?;
            continue;
        };
        let mut posts: Vec<Value> = Vec::new();
        let (mut ok, mut refused, mut unreachable) = (true, false, false);
        if let Some(n) = node {
            let bodies: Vec<(&str, String)> = match opt.mode {
                Mode::Updates => vec![(UPDATES_PATH, j::update_to_json(&u).to_string())],
                Mode::Observations => u
                    .observations
                    .iter()
                    .map(|o| {
                        let mut ob = j::signed_to_json(o, u.round);
                        ob.as_object_mut().unwrap().remove("round");
                        (
                            OBSERVATIONS_PATH,
                            json!({"round": u.round, "observation": ob}).to_string(),
                        )
                    })
                    .collect(),
            };
            for (path, body) in bodies {
                match n.post_json(path, &body) {
                    Ok((status, reply)) => {
                        if !(200..300).contains(&status) {
                            ok = false;
                            refused = true;
                        }
                        let reply: Value =
                            serde_json::from_str(&reply).unwrap_or(Value::String(reply));
                        posts.push(json!({"status": status, "reply": reply}));
                    }
                    Err(e) => {
                        ok = false;
                        unreachable = true;
                        posts.push(json!({"error": e}));
                    }
                }
            }
            sum.accepted += u64::from(ok);
            sum.refused += u64::from(refused);
            sum.unreachable += u64::from(unreachable);
        }
        let line = json!({
            "step": i, "round": round, "timeMs": t,
            "signers": u.observations.iter().map(|o| o.signer).collect::<Vec<_>>(),
            "prices": steps[i].prices.iter().map(|(m, (p, s))| json!({"marketId": m, "price": p.to_string(), "sources": s})).collect::<Vec<_>>(),
            "posts": posts,
            "update": if node.is_none() { j::update_to_json(&u) } else { Value::Null },
        });
        writeln!(log, "{line}").map_err(|e| e.to_string())?;
    }
    Ok(sum)
}

pub fn summary_json(s: &Summary) -> Value {
    json!({"rounds": s.rounds, "silent": s.silent, "accepted": s.accepted, "refused": s.refused, "unreachable": s.unreachable})
}

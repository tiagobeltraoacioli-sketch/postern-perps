//! Live fetching (feature `net`): public, anonymous HTTPS GETs of venue tickers, one thread per source, a
//! global timeout per request, a generic client name. UNAUDITED PROTOTYPE.

use std::collections::BTreeMap;
use std::fs;
use std::io::Write;
use std::path::Path;
use std::thread;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use perps_oracle::types::round_of;
use perps_oracle::{eip712, sig, ROUND_MS};
use serde_json::json;

use crate::health::{classify, Health};
use crate::json;
use crate::keyfile;
use crate::pipeline::{default_config, Feeder, FeederConfig, Response};
use crate::recording::{round_line, Fetch};
use crate::venues::Source;

pub const FETCH_TIMEOUT_MS: u64 = 1500;
pub const MAX_CLOCK_SKEW_MS: u64 = 1000;

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_millis() as u64
}

fn all_sources(c: &FeederConfig) -> Vec<Source> {
    let mut v: Vec<Source> = c.usdt_sources.clone();
    for m in &c.markets {
        v.extend(m.sources.iter().cloned());
    }
    v
}

/// The generic client name sent to the venues (nothing identifying an operator or a project).
pub const USER_AGENT: &str = "ticker-client/1.0";

/// The requests of one round: one per distinct URL (a batched request serves several sources), in first-use order.
pub fn fetch_units(sources: &[Source]) -> Vec<(String, Vec<String>)> {
    let mut units: Vec<(String, Vec<String>)> = Vec::new();
    for s in sources {
        match units.iter_mut().find(|(u, _)| *u == s.url) {
            Some((_, names)) => {
                if !names.contains(&s.name) {
                    names.push(s.name.clone());
                }
            }
            None => units.push((s.url.clone(), vec![s.name.clone()])),
        }
    }
    units
}

/// One GET per unit, concurrently. A unit for which `skip` is true is not requested (drop rule D2).
pub fn fetch_all(units: &[(String, Vec<String>)], skip: &dyn Fn(&str) -> bool) -> Vec<Fetch> {
    let agent: ureq::Agent = ureq::Agent::config_builder()
        .timeout_global(Some(Duration::from_millis(FETCH_TIMEOUT_MS)))
        .user_agent(USER_AGENT)
        .http_status_as_error(false)
        .build()
        .into();
    let handles: Vec<_> = units
        .iter()
        .map(|(url, names)| {
            let a = agent.clone();
            let (url, sources) = (url.clone(), names.clone());
            let dropped = skip(&url);
            thread::spawn(move || {
                let mut f = Fetch {
                    url: url.clone(),
                    sources,
                    status: 0,
                    received_ms: 0,
                    http_date_ms: None,
                    error: None,
                    body: None,
                    dropped,
                };
                if dropped {
                    return f;
                }
                let res = a.get(&url).call();
                f.received_ms = now_ms();
                match res {
                    Ok(mut r) => {
                        f.status = r.status().as_u16();
                        f.http_date_ms = r
                            .headers()
                            .get("date")
                            .and_then(|d| d.to_str().ok())
                            .and_then(parse_http_date_ms);
                        match r.body_mut().with_config().limit(64 * 1024).read_to_string() {
                            Ok(b) => f.body = Some(b),
                            Err(e) => f.error = Some(e.to_string()),
                        }
                        if f.status != 200 && f.error.is_none() {
                            f.error = Some(format!("http-status-{}", f.status));
                        }
                    }
                    Err(e) => f.error = Some(e.to_string()),
                }
                f
            })
        })
        .collect();
    handles
        .into_iter()
        .map(|h| h.join().expect("fetch thread"))
        .collect()
}

/// `Tue, 17 Nov 2026 04:06:08 GMT` (RFC 9110 IMF-fixdate) to Unix ms.
pub fn parse_http_date_ms(s: &str) -> Option<u64> {
    let p: Vec<&str> = s.split_whitespace().collect();
    if p.len() != 6 || p[5] != "GMT" {
        return None;
    }
    let mon = [
        "Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec",
    ]
    .iter()
    .position(|m| *m == p[2])?
        + 1;
    let iso = format!(
        "{}-{:02}-{:02}T{}Z",
        p[3],
        mon,
        p[1].parse::<u32>().ok()?,
        p[4]
    );
    crate::venues::parse_rfc3339_ms(&iso).ok()
}

/// The local clock is usable when the median venue `Date` is within `MAX_CLOCK_SKEW_MS` plus the header's
/// one-second resolution of the receive time.
pub fn clock_ok(dates: &[(u64, u64)]) -> Result<(), String> {
    if dates.len() < 3 {
        return Err(format!(
            "clock check: only {} venue Date headers",
            dates.len()
        ));
    }
    let mut skews: Vec<i64> = dates
        .iter()
        .map(|&(date, recv)| recv as i64 - date as i64)
        .collect();
    skews.sort_unstable();
    let med = skews[skews.len() / 2];
    // Date is truncated to the second: an exact clock shows a skew in [0, 1000).
    if med < -(MAX_CLOCK_SKEW_MS as i64) || med >= 1000 + MAX_CLOCK_SKEW_MS as i64 {
        return Err(format!(
            "clock check: local clock off by {med} ms from the median venue Date"
        ));
    }
    Ok(())
}

fn wait_for_round_start() -> u64 {
    let n = now_ms();
    let next = (n / ROUND_MS + 1) * ROUND_MS;
    thread::sleep(Duration::from_millis(next - n));
    now_ms()
}

fn rounds_arg(m: &BTreeMap<String, Vec<String>>) -> Result<u64, String> {
    match m.get("rounds").map(|v| v.as_slice()) {
        None => Ok(1),
        Some([n]) => n.parse().map_err(|_| "--rounds: a number".into()),
        _ => Err("--rounds takes one value".into()),
    }
}

fn one<'a>(m: &'a BTreeMap<String, Vec<String>>, k: &str) -> Result<&'a str, String> {
    match m.get(k).map(|v| v.as_slice()) {
        Some([v]) => Ok(v),
        _ => Err(format!("--{k} takes exactly one value")),
    }
}

/// Per-unit health (drop rule D1 to D3) across the rounds of one run.
#[derive(Default)]
pub struct Healths(pub BTreeMap<String, Health>);

impl Healths {
    fn skip(&self, round: u64) -> impl Fn(&str) -> bool + '_ {
        move |url| self.0.get(url).is_some_and(|h| !h.should_fetch(round))
    }

    /// D1: a unit is OK when it answered 200 and at least one of its sources got a reading out of the body.
    fn record(
        &mut self,
        round: u64,
        fetches: &[Fetch],
        rep: &crate::pipeline::RoundReport,
    ) -> Vec<serde_json::Value> {
        let parsed = |name: &str| {
            rep.usdt_sources
                .iter()
                .chain(rep.markets.iter().flat_map(|m| m.sources.iter()))
                .any(|s| {
                    s.name == name
                        && !s.reason.as_deref().is_some_and(|r| {
                            r.starts_with("parse")
                                || r.starts_with("fetch-failed")
                                || r == "no-response"
                        })
                })
        };
        let mut events = Vec::new();
        for f in fetches.iter().filter(|f| !f.dropped) {
            let any = f.sources.iter().any(|n| parsed(n));
            let h = self.0.entry(f.url.clone()).or_default();
            if let Some(resume) = h.record(round, classify(f.status, any)) {
                events.push(json!({"dropped": f.url, "status": f.status, "error": f.error, "resumeRound": resume}));
            }
        }
        events
    }
}

fn open_append(path: &str) -> Result<fs::File, String> {
    fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
        .map_err(|e| format!("{path}: {e}"))
}

/// `record --out FILE.jsonl [--rounds N]`: appends one line per round in the line form of [`crate::recording`]
/// (raw venue responses, Kraken batched, drop rule applied).
pub fn record(m: &BTreeMap<String, Vec<String>>) -> Result<(), String> {
    let mut out = open_append(one(m, "out")?)?;
    let mut feeder =
        Feeder::new(default_config().with_kraken_batch()).map_err(|e| format!("{e:?}"))?;
    let units = fetch_units(&all_sources(&feeder.config));
    let mut health = Healths::default();
    for _ in 0..rounds_arg(m)? {
        let sent = wait_for_round_start();
        let round = round_of(sent);
        let fetches = fetch_all(&units, &health.skip(round));
        let resp: Vec<Response> = fetches.iter().flat_map(|f| f.responses(sent)).collect();
        let (_, rep) = feeder.observe(round, sent, &resp);
        health.record(round, &fetches, &rep);
        writeln!(out, "{}", round_line(round, sent, &fetches)).map_err(|e| e.to_string())?;
    }
    Ok(())
}

/// `run`: the live feeder. Each round: fetch (one request per unit, drop rule), clock check, observe, sign,
/// append `{observation(s), report, events}` to `--out`; with `--record` the raw responses too; with `--node` the
/// signed round is posted to the node's ingest (loopback only, see [`crate::submit`]).
///
/// Signing: `--signer I --key-file F` (one validator's feeder: posts its observation to
/// `/v1/oracle/observations`), or `--key-dir DIR` (a dev committee in one process, development only: posts the
/// whole update to `/v1/oracle/updates`).
pub fn run_live(m: &BTreeMap<String, Vec<String>>) -> Result<(), String> {
    let vault = json::hex_bytes::<20>(&one(m, "vault")?.to_lowercase(), "--vault")?;
    if vault == [0u8; 20] {
        return Err("--vault: the zero address".into());
    }
    let keys: Vec<(u8, k256::ecdsa::SigningKey)> = if m.contains_key("key-dir") {
        keyfile::DevCommittee::read_dir(Path::new(one(m, "key-dir")?))?
            .keys
            .into_iter()
            .enumerate()
            .map(|(i, k)| (i as u8, k))
            .collect()
    } else {
        let signer: u8 = one(m, "signer")?.parse().map_err(|_| "--signer: 0..255")?;
        vec![(signer, keyfile::read_key(Path::new(one(m, "key-file")?))?)]
    };
    let committee_mode = m.contains_key("key-dir");
    let node = match m.get("node") {
        None => None,
        Some(_) => {
            let token = match m.get("token-file") {
                Some(_) => Some(crate::submit::read_token(Path::new(one(m, "token-file")?))?),
                None => None,
            };
            Some(crate::submit::NodeClient::new(one(m, "node")?, token)?)
        }
    };
    let domain = eip712::domain_separator(&vault);
    let mut out = open_append(one(m, "out")?)?;
    let mut rec = match m.get("record") {
        Some(_) => Some(open_append(one(m, "record")?)?),
        None => None,
    };
    let mut feeder =
        Feeder::new(default_config().with_kraken_batch()).map_err(|e| format!("{e:?}"))?;
    let units = fetch_units(&all_sources(&feeder.config));
    let mut health = Healths::default();
    for _ in 0..rounds_arg(m)? {
        let t0 = wait_for_round_start();
        let round = round_of(t0);
        let fetches = fetch_all(&units, &health.skip(round));
        let dates: Vec<(u64, u64)> = fetches
            .iter()
            .filter_map(|f| f.http_date_ms.map(|d| (d, f.received_ms)))
            .collect();
        let resp: Vec<Response> = fetches.iter().flat_map(|f| f.responses(t0)).collect();
        let (obs, rep) = feeder.observe(round, t0, &resp);
        let events = health.record(round, &fetches, &rep);
        if let Some(r) = rec.as_mut() {
            writeln!(r, "{}", round_line(round, t0, &fetches)).map_err(|e| e.to_string())?;
        }
        let fetch_ms: Vec<serde_json::Value> = fetches
            .iter()
            .filter(|f| !f.dropped)
            .map(|f| json!({"url": f.url, "status": f.status, "ms": f.received_ms.saturating_sub(t0), "error": f.error}))
            .collect();
        let report = json::round_report_to_json(&rep);
        let line = match (clock_ok(&dates), obs) {
            (Err(e), _) => {
                json!({"round": round, "refused": e, "report": report, "events": events, "fetches": fetch_ms})
            }
            (Ok(()), None) => {
                json!({"round": round, "observation": null, "report": report, "events": events, "fetches": fetch_ms})
            }
            (Ok(()), Some(o)) => {
                let d = eip712::digest(&domain, &o);
                let signed: Vec<perps_oracle::types::SignedObservation> = keys
                    .iter()
                    .map(|(signer, key)| perps_oracle::types::SignedObservation {
                        signer: *signer,
                        time_ms: o.time_ms,
                        prices: o.prices.clone(),
                        signature: sig::sign(key, &d),
                    })
                    .collect();
                let posts = match &node {
                    Some(n) => {
                        let u = perps_oracle::types::OracleUpdate {
                            round: o.round,
                            observations: signed.clone(),
                        };
                        let mode = if committee_mode {
                            crate::submit::Mode::Updates
                        } else {
                            crate::submit::Mode::Observations
                        };
                        n.post_round(&u, mode).log
                    }
                    None => Vec::new(),
                };
                json!({"observation": json::signed_to_json(&signed[0], o.round),
                       "signers": signed.iter().map(|s| s.signer).collect::<Vec<_>>(),
                       "posts": posts, "report": report, "events": events, "fetches": fetch_ms})
            }
        };
        writeln!(out, "{}", serde_json::to_string(&line).unwrap()).map_err(|e| e.to_string())?;
    }
    let h: Vec<serde_json::Value> = health
        .0
        .iter()
        .map(|(u, h)| json!({"url": u, "ok": h.successes, "failed": h.failures, "drops": h.drops}))
        .collect();
    writeln!(out, "{}", json!({"health": h})).map_err(|e| e.to_string())?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn http_date_and_clock() {
        assert_eq!(
            parse_http_date_ms("Tue, 17 Nov 2026 04:06:08 GMT"),
            Some(1_794_888_368_000)
        );
        assert_eq!(parse_http_date_ms("Tue, 17 Nov 2026 04:06:08 UTC"), None);
        let d = 1_794_888_368_000;
        assert!(clock_ok(&[(d, d + 400), (d, d + 900), (d, d + 100)]).is_ok());
        assert!(clock_ok(&[(d, d + 2500), (d, d + 2400), (d, d + 2100)]).is_err());
        assert!(clock_ok(&[(d, d - 1500), (d, d - 1200), (d, d - 1100)]).is_err());
        assert!(clock_ok(&[(d, d)]).is_err());
    }

    #[test]
    fn one_request_per_unit_and_the_batched_unit_serves_four_sources() {
        let c = default_config().with_kraken_batch();
        let units = fetch_units(&all_sources(&c));
        assert_eq!(units.len(), 22);
        let kraken: Vec<_> = units.iter().filter(|(u, _)| u.contains("kraken")).collect();
        assert_eq!(kraken.len(), 1);
        assert_eq!(
            kraken[0].1,
            [
                "kraken-USDT-USD",
                "kraken-BTC-USD",
                "kraken-ETH-USD",
                "kraken-SOL-USD"
            ]
        );
        assert!(units
            .iter()
            .filter(|(u, _)| !u.contains("kraken"))
            .all(|(_, n)| n.len() == 1));
        // Without batching: 25 requests, as in round 1.
        assert_eq!(fetch_units(&all_sources(&default_config())).len(), 25);
    }

    /// Drop rule over the live loop's own bookkeeping: a unit that times out five rounds in a row is skipped for
    /// the cooldown, a 429 is skipped at once, a unit that answers 200 with a body no adapter accepts counts as
    /// failed, and the others are never touched.
    #[test]
    fn health_bookkeeping_follows_the_drop_rule() {
        let mut feeder = Feeder::new(default_config().with_kraken_batch()).unwrap();
        let units = fetch_units(&all_sources(&feeder.config));
        let mut health = Healths::default();
        let gemini = "https://api.gemini.com/v1/pubticker/btcusd";
        let okx = "https://www.okx.com/api/v5/market/ticker?instId=BTC-USDT";
        let bitfinex = "https://api-pub.bitfinex.com/v2/ticker/tBTCUSD";
        let mut skipped: Vec<(u64, String)> = Vec::new();
        for round in 100..140u64 {
            let t0 = round * ROUND_MS;
            let fetches: Vec<Fetch> = units
                .iter()
                .map(|(url, sources)| {
                    let dropped = health.skip(round)(url);
                    if dropped {
                        skipped.push((round, url.clone()));
                    }
                    let (status, error, body) = if url == gemini {
                        (0, Some("timeout: global".to_string()), None)
                    } else if url == okx && round == 100 {
                        (
                            429,
                            Some("http-status-429".to_string()),
                            Some("{}".to_string()),
                        )
                    } else if url == bitfinex {
                        (
                            200,
                            None,
                            Some("[\"error\",10020,\"symbol: invalid\"]".to_string()),
                        )
                    } else {
                        // No body the adapters accept either, but these units are not under test: mark them OK
                        // by leaving them out of the bookkeeping below.
                        (200, None, None)
                    };
                    Fetch {
                        url: url.clone(),
                        sources: sources.clone(),
                        status,
                        received_ms: t0 + 50,
                        http_date_ms: None,
                        error,
                        body,
                        dropped,
                    }
                })
                .filter(|f| [gemini, okx, bitfinex].contains(&f.url.as_str()))
                .collect();
            let resp: Vec<Response> = fetches.iter().flat_map(|f| f.responses(t0)).collect();
            let (_, rep) = feeder.observe(round, t0, &resp);
            health.record(round, &fetches, &rep);
        }
        let of = |u: &str| -> Vec<u64> {
            skipped
                .iter()
                .filter(|(_, x)| x == u)
                .map(|(r, _)| *r)
                .collect()
        };
        // OKX: refused in round 100 -> skipped rounds 101..=120, probed at 121 (again a failure: no body) ...
        assert_eq!(of(okx)[..20], (101..=120).collect::<Vec<_>>()[..]);
        assert!(!of(okx).contains(&121));
        // Gemini: five timeouts (100..=104), skipped 105..=124, probed at 125 (fails: dropped again, 40 rounds).
        assert_eq!(of(gemini), (105..=124).chain(126..140).collect::<Vec<_>>());
        // Bitfinex: a 200 with an error body is a failure too.
        assert_eq!(
            of(bitfinex),
            (105..=124).chain(126..140).collect::<Vec<_>>()
        );
        assert_eq!(health.0[gemini].drops, 2);
        assert_eq!(health.0[gemini].successes, 0);
    }
}

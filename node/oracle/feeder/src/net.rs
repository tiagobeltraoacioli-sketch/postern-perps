//! Live fetching (feature `net`): public, anonymous HTTPS GETs of venue tickers, one thread per source, a
//! global timeout per request, a generic client name. UNAUDITED PROTOTYPE.

use std::collections::BTreeMap;
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::thread;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use perps_oracle::types::round_of;
use perps_oracle::{eip712, sig, ROUND_MS};
use serde_json::json;

use crate::json;
use crate::keyfile;
use crate::pipeline::{default_config, Feeder, FeederConfig, Response};
use crate::venues::Source;

pub const FETCH_TIMEOUT_MS: u64 = 1500;
pub const MAX_CLOCK_SKEW_MS: u64 = 1000;

fn now_ms() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_millis() as u64
}

fn all_sources(c: &FeederConfig) -> Vec<Source> {
    let mut v: Vec<Source> = c.usdt_sources.clone();
    for m in &c.markets {
        v.extend(m.sources.iter().cloned());
    }
    v
}

/// One fetch per source, concurrently. Returns the responses and each response's HTTP `Date` (ms) if present.
pub fn fetch_all(sources: &[Source]) -> Vec<(Response, Option<u64>, String)> {
    let agent: ureq::Agent = ureq::Agent::config_builder()
        .timeout_global(Some(Duration::from_millis(FETCH_TIMEOUT_MS)))
        .user_agent("perps-oracle/0.1")
        .build()
        .into();
    let handles: Vec<_> = sources
        .iter()
        .map(|s| {
            let a = agent.clone();
            let (name, url) = (s.name.clone(), s.url.clone());
            thread::spawn(move || {
                let res = a.get(&url).call();
                let received = now_ms();
                match res {
                    Ok(mut r) => {
                        let date = r.headers().get("date").and_then(|d| d.to_str().ok()).and_then(parse_http_date_ms);
                        match r.body_mut().with_config().limit(64 * 1024).read_to_string() {
                            Ok(b) => (Response { source: name, body: Some(b), received_ms: received, error: None }, date, url),
                            Err(e) => (Response { source: name, body: None, received_ms: received, error: Some(e.to_string()) }, date, url),
                        }
                    }
                    Err(e) => (Response { source: name, body: None, received_ms: received, error: Some(e.to_string()) }, None, url),
                }
            })
        })
        .collect();
    handles.into_iter().map(|h| h.join().expect("fetch thread")).collect()
}

/// `Tue, 17 Nov 2026 04:06:08 GMT` (RFC 9110 IMF-fixdate) to Unix ms.
pub fn parse_http_date_ms(s: &str) -> Option<u64> {
    let p: Vec<&str> = s.split_whitespace().collect();
    if p.len() != 6 || p[5] != "GMT" {
        return None;
    }
    let mon = ["Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec"].iter().position(|m| *m == p[2])? + 1;
    let iso = format!("{}-{:02}-{:02}T{}Z", p[3], mon, p[1].parse::<u32>().ok()?, p[4]);
    crate::venues::parse_rfc3339_ms(&iso).ok()
}

/// The local clock is usable when the median venue `Date` is within `MAX_CLOCK_SKEW_MS` plus the header's
/// one-second resolution of the receive time.
pub fn clock_ok(dates: &[(u64, u64)]) -> Result<(), String> {
    if dates.len() < 3 {
        return Err(format!("clock check: only {} venue Date headers", dates.len()));
    }
    let mut skews: Vec<i64> = dates.iter().map(|&(date, recv)| recv as i64 - date as i64).collect();
    skews.sort_unstable();
    let med = skews[skews.len() / 2];
    // Date is truncated to the second: an exact clock shows a skew in [0, 1000).
    if med < -(MAX_CLOCK_SKEW_MS as i64) || med >= 1000 + MAX_CLOCK_SKEW_MS as i64 {
        return Err(format!("clock check: local clock off by {med} ms from the median venue Date"));
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

/// `record --out DIR [--rounds N]`: writes `DIR/round-<r>/<source>.json` in the fixture format.
pub fn record(m: &BTreeMap<String, Vec<String>>) -> Result<(), String> {
    let out = PathBuf::from(one(m, "out")?);
    let sources = all_sources(&default_config());
    for _ in 0..rounds_arg(m)? {
        let sent = wait_for_round_start();
        let dir = out.join(format!("round-{}", round_of(sent)));
        fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
        for (r, date, url) in fetch_all(&sources) {
            let v = json!({"venue": r.source, "url": url, "sentMs": sent, "receivedMs": r.received_ms,
                           "httpDateMs": date, "error": r.error, "body": r.body.unwrap_or_default()});
            fs::write(dir.join(format!("{}.json", v["venue"].as_str().unwrap())), serde_json::to_string(&v).unwrap()).map_err(|e| e.to_string())?;
        }
        eprintln!("recorded {}", dir.display());
    }
    Ok(())
}

/// `run --signer I --key-file F --out JSONL [--rounds N]`: the live feeder. Each round: fetch, clock check,
/// observe, sign, append `{observation, report}`. Submission to the sequencer is not built yet (389's ingest
/// endpoint is not fixed); the JSONL is the hand-off.
pub fn run_live(m: &BTreeMap<String, Vec<String>>) -> Result<(), String> {
    let signer: u8 = one(m, "signer")?.parse().map_err(|_| "--signer: 0..255")?;
    let key = keyfile::read_key(Path::new(one(m, "key-file")?))?;
    let vault = json::hex_bytes::<20>(&one(m, "vault")?.to_lowercase(), "--vault")?;
    if vault == [0u8; 20] {
        return Err("--vault: the zero address".into());
    }
    let domain = eip712::domain_separator(&vault);
    let mut out = fs::OpenOptions::new().create(true).append(true).open(one(m, "out")?).map_err(|e| e.to_string())?;
    let mut feeder = Feeder::new(default_config()).map_err(|e| format!("{e:?}"))?;
    let sources = all_sources(&feeder.config);
    for _ in 0..rounds_arg(m)? {
        let t0 = wait_for_round_start();
        let fetched = fetch_all(&sources);
        let dates: Vec<(u64, u64)> = fetched.iter().filter_map(|(r, d, _)| d.map(|d| (d, r.received_ms))).collect();
        let resp: Vec<Response> = fetched.into_iter().map(|(r, _, _)| r).collect();
        let (obs, rep) = feeder.observe(round_of(t0), t0, &resp);
        let line = match (clock_ok(&dates), obs) {
            (Err(e), _) => json!({"round": round_of(t0), "refused": e, "report": json::round_report_to_json(&rep)}),
            (Ok(()), None) => json!({"round": round_of(t0), "observation": null, "report": json::round_report_to_json(&rep)}),
            (Ok(()), Some(o)) => {
                let s = sig::sign(&key, &eip712::digest(&domain, &o));
                let so = perps_oracle::types::SignedObservation { signer, time_ms: o.time_ms, prices: o.prices.clone(), signature: s };
                json!({"observation": json::signed_to_json(&so, o.round), "report": json::round_report_to_json(&rep)})
            }
        };
        writeln!(out, "{}", serde_json::to_string(&line).unwrap()).map_err(|e| e.to_string())?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn http_date_and_clock() {
        assert_eq!(parse_http_date_ms("Tue, 17 Nov 2026 04:06:08 GMT"), Some(1_794_888_368_000));
        assert_eq!(parse_http_date_ms("Tue, 17 Nov 2026 04:06:08 UTC"), None);
        let d = 1_794_888_368_000;
        assert!(clock_ok(&[(d, d + 400), (d, d + 900), (d, d + 100)]).is_ok());
        assert!(clock_ok(&[(d, d + 2500), (d, d + 2400), (d, d + 2100)]).is_err());
        assert!(clock_ok(&[(d, d - 1500), (d, d - 1200), (d, d - 1100)]).is_err());
        assert!(clock_ok(&[(d, d)]).is_err());
    }
}

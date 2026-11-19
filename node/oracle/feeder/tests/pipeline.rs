//! Feeder pipeline on recorded venue responses and on adversarial variants, and the path from feeders to the
//! engine rule (README sections 5 to 7). Throwaway keys from public labels only.

use std::fs;
use std::path::{Path, PathBuf};

use perps_oracle::committee::{
    apply_update, begin_block, CommitteeParams, MarketOutcome, MarketParams, OracleState,
};
use perps_oracle::decimal::D18;
use perps_oracle::types::{round_of, OracleUpdate, SignedObservation};
use perps_oracle::{eip712, encoding, sig, ROUND_MS};
use perps_oracle_feeder::json;
use perps_oracle_feeder::keyfile::dev_key;
use perps_oracle_feeder::pipeline::{Feeder, FeederConfig, Response};
use perps_oracle_feeder::venues::default_sources;
use serde_json::Value;

const VAULT: [u8; 20] = [0xdb; 20];

/// Round 2 (R432-1): the crate's default configuration now signs at the engine's price scale (1e-8 USD). Every
/// test of this file was written in round 1 with expected prices at the display decimals (BTC 1, ETH 2, SOL 3) and
/// keeps them unchanged through this configuration: the rules under test (F1 to F6, C1 to C5, H1 to H3) do not
/// depend on the scale. The engine-scale configuration is tested in `tests/r2.rs`, including the equality of the
/// two golden files up to rounding.
fn default_config() -> FeederConfig {
    let mut c = perps_oracle_feeder::pipeline::default_config();
    for (m, d) in c.markets.iter_mut().zip([1u8, 2, 3]) {
        m.price_decimals = d;
    }
    c
}

fn mp(id: u32, jump: u32) -> MarketParams {
    MarketParams {
        market_id: id,
        min_sources: 3,
        agree_bps: 100,
        jump_bps: jump,
        max_staleness_ms: 10_000,
        confirm_rounds: 3,
        exit_haircut_bps: jump,
        exit_haircut_per_min_bps: 100,
        exit_max_haircut_bps: 5000,
        max_halt_ms: 3_600_000,
    }
}

fn fixtures() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../fixtures")
}

fn load_dir(dir: &Path) -> (u64, Vec<Response>) {
    let mut files: Vec<PathBuf> = fs::read_dir(dir)
        .unwrap()
        .map(|e| e.unwrap().path())
        .filter(|p| p.extension().is_some_and(|x| x == "json"))
        .collect();
    files.sort();
    let mut t0 = u64::MAX;
    let mut out = Vec::new();
    for f in files {
        let v: Value = serde_json::from_str(&fs::read_to_string(&f).unwrap()).unwrap();
        t0 = t0.min(v["sentMs"].as_u64().unwrap());
        let failed = v.get("error").is_some_and(|e| !e.is_null())
            || v.get("curlExit")
                .and_then(Value::as_i64)
                .is_some_and(|c| c != 0);
        out.push(Response {
            source: v["venue"].as_str().unwrap().into(),
            body: if failed {
                None
            } else {
                Some(v["body"].as_str().unwrap().into())
            },
            received_ms: v["receivedMs"].as_u64().unwrap(),
            error: None,
        });
    }
    (t0, out)
}

fn round_dirs() -> Vec<PathBuf> {
    let mut d: Vec<PathBuf> = fs::read_dir(fixtures().join("rounds"))
        .unwrap()
        .map(|e| e.unwrap().path())
        .filter(|p| p.is_dir())
        .collect();
    d.sort();
    d
}

#[test]
fn every_recorded_response_parses() {
    let (_, resp) = load_dir(&fixtures().join("recorded"));
    assert_eq!(resp.len(), 26);
    let mut all = default_sources("USDT");
    for b in ["BTC", "ETH", "SOL"] {
        all.extend(default_sources(b));
    }
    // Bitfinex USDT/USD is recorded but deliberately not a source of the rate (SOURCES.md); its parser is the
    // same as for the other Bitfinex pairs.
    all.push(perps_oracle_feeder::venues::Source::new(
        perps_oracle_feeder::venues::Adapter::Bitfinex,
        "USDT",
        perps_oracle::feeder_rule::Quote::Usd,
    ));
    let mut parsed = 0;
    for r in &resp {
        let s = all
            .iter()
            .find(|s| s.name == r.source)
            .unwrap_or_else(|| panic!("{} is not a configured source", r.source));
        let x = s
            .parse(r.body.as_deref().unwrap(), r.received_ms)
            .unwrap_or_else(|e| panic!("{}: {e}", r.source));
        assert!(x.bid > 0 && x.bid <= x.ask, "{}", r.source);
        parsed += 1;
    }
    assert_eq!(parsed, 26);
}

/// Recorded rounds replay to the committed golden observations (determinism across machines and versions).
#[test]
fn recorded_rounds_replay_to_the_golden_file() {
    let golden: Vec<Value> = fs::read_to_string(fixtures().join("rounds/expected.jsonl"))
        .unwrap()
        .lines()
        .map(|l| serde_json::from_str(l).unwrap())
        .collect();
    let mut f = Feeder::new(default_config()).unwrap();
    let dirs = round_dirs();
    assert_eq!(dirs.len(), golden.len());
    for (d, g) in dirs.iter().zip(&golden) {
        let (t0, resp) = load_dir(d);
        let (obs, rep) = f.observe(round_of(t0), t0, &resp);
        let obs = obs.expect("every recorded round prices something");
        assert_eq!(
            obs.prices.len(),
            3,
            "{}: {:?}",
            d.display(),
            rep.markets.iter().map(|m| &m.result).collect::<Vec<_>>()
        );
        let got: Vec<Value> = obs.prices.iter().map(json::price_to_json).collect();
        assert_eq!(Value::Array(got), g["prices"], "{}", d.display());
        assert_eq!(rep.round, g["round"].as_u64().unwrap());
        for m in &rep.markets {
            assert!(
                m.result.as_ref().unwrap().sources >= 5,
                "{}: fewer than 5 agreeing venues in a calm market",
                m.symbol
            );
        }
    }
}

// ---- synthetic venue bodies, for adversarial cases ----

fn body(source: &str, bid: &str, ask: &str, last: &str, t_ms: u64) -> String {
    let op = source.split('-').next().unwrap();
    let pair_parts: Vec<&str> = source.split('-').collect();
    let (base, quote) = (pair_parts[1], pair_parts[2]);
    match op {
        "coinbase" => {
            let secs = t_ms / 1000;
            let (d, rem) = (secs / 86_400, secs % 86_400);
            // civil from days (Hinnant)
            let z = d as i64 + 719_468;
            let era = z.div_euclid(146_097);
            let doe = z - era * 146_097;
            let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
            let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
            let mp = (5 * doy + 2) / 153;
            let day = doy - (153 * mp + 2) / 5 + 1;
            let mo = if mp < 10 { mp + 3 } else { mp - 9 };
            let y = yoe + era * 400 + i64::from(mo <= 2);
            format!(
                r#"{{"ask":"{ask}","bid":"{bid}","price":"{last}","time":"{y:04}-{mo:02}-{day:02}T{:02}:{:02}:{:02}.{:03}Z"}}"#,
                rem / 3600,
                rem / 60 % 60,
                rem % 60,
                t_ms % 1000
            )
        }
        "kraken" => {
            let key = match (base, quote) {
                ("BTC", "USD") => "XXBTZUSD".to_string(),
                ("ETH", "USD") => "XETHZUSD".to_string(),
                ("USDT", "USD") => "USDTZUSD".to_string(),
                _ => format!("{base}{quote}"),
            };
            format!(
                r#"{{"error":[],"result":{{"{key}":{{"a":["{ask}","1","1.000"],"b":["{bid}","1","1.000"],"c":["{last}","0.1"]}}}}}}"#
            )
        }
        "bitstamp" => format!(
            r#"{{"timestamp":"{}","bid":"{bid}","ask":"{ask}","last":"{last}"}}"#,
            t_ms / 1000
        ),
        "gemini" => format!(
            r#"{{"bid":"{bid}","ask":"{ask}","last":"{last}","volume":{{"timestamp":{t_ms}}}}}"#
        ),
        "bitfinex" => format!("[{bid},1,{ask},1,0,0,{last},1,1,1,1]"),
        "okx" => format!(
            r#"{{"code":"0","msg":"","data":[{{"instId":"{base}-{quote}","bidPx":"{bid}","askPx":"{ask}","last":"{last}","ts":"{t_ms}"}}]}}"#
        ),
        "binance" => {
            format!(r#"{{"symbol":"{base}{quote}","bidPrice":"{bid}","askPrice":"{ask}"}}"#)
        }
        _ => unreachable!(),
    }
}

fn fmt(v: f64, dp: usize) -> String {
    // Test-side text generation only; the code under test never sees a float.
    format!("{v:.dp$}")
}

/// Responses for every configured source at `t`: USD venues at `btc` (BTC; ETH and SOL proportional), USDT venues at
/// `btc / usdt`, USDT/USD at `usdt`.
fn world(t: u64, btc: f64, usdt: f64) -> Vec<Response> {
    let mut out = Vec::new();
    let cfg = default_config();
    let mut push = |name: &str, mid: f64, dp: usize| {
        let (b, a) = (fmt(mid * 0.99995, dp), fmt(mid * 1.00005, dp));
        out.push(Response {
            source: name.into(),
            body: Some(body(name, &b, &a, &fmt(mid, dp), t - 50)),
            received_ms: t + 120,
            error: None,
        });
    };
    for s in &cfg.usdt_sources {
        push(&s.name, usdt, 5);
    }
    for (m, k) in cfg.markets.iter().zip([1.0, 0.0282, 0.00112]) {
        for s in &m.sources {
            let usd = btc * k;
            let p = if s.name.ends_with("-USDT") {
                usd / usdt
            } else {
                usd
            };
            push(&s.name, p, 4);
        }
    }
    out
}

fn set(resp: &mut [Response], name: &str, b: String) {
    resp.iter_mut().find(|r| r.source == name).unwrap().body = Some(b);
}

const T: u64 = 1_794_888_000_000;

fn btc_of(
    f: &mut Feeder,
    resp: &[Response],
    t: u64,
) -> (Option<u64>, perps_oracle_feeder::pipeline::RoundReport) {
    let (o, rep) = f.observe(round_of(t), t, resp);
    (
        o.and_then(|o| o.prices.iter().find(|p| p.market_id == 1).map(|p| p.price)),
        rep,
    )
}

fn reason<'a>(
    rep: &'a perps_oracle_feeder::pipeline::RoundReport,
    market: u32,
    src: &str,
) -> Option<&'a str> {
    rep.markets
        .iter()
        .find(|m| m.market_id == market)
        .unwrap()
        .sources
        .iter()
        .find(|s| s.name == src)
        .unwrap()
        .reason
        .as_deref()
}

#[test]
fn calm_world_prices_everything_from_seven_venues() {
    let mut f = Feeder::new(default_config()).unwrap();
    let (o, rep) = f.observe(round_of(T), T, &world(T, 65_000.0, 0.999));
    let o = o.unwrap();
    assert_eq!(o.prices.len(), 3);
    assert!(o.prices.iter().all(|p| p.sources == 7), "{:?}", o.prices);
    assert_eq!(o.prices[0].price, 650_000);
    assert!(rep.usdt_rate18.unwrap().abs_diff(999 * D18 / 1000) < D18 / 1_000_000);
}

#[test]
fn one_and_two_lying_venues_are_cut() {
    let mut f = Feeder::new(default_config()).unwrap();
    let mut w = world(T, 65_000.0, 0.999);
    set(
        &mut w,
        "coinbase-BTC-USD",
        body("coinbase-BTC-USD", "97500", "97501", "97500", T),
    );
    let (p, rep) = btc_of(&mut f, &w, T);
    assert_eq!(p, Some(650_000));
    assert_eq!(reason(&rep, 1, "coinbase-BTC-USD"), Some("outlier"));
    set(
        &mut w,
        "kraken-BTC-USD",
        body("kraken-BTC-USD", "1", "1.00001", "1", T),
    );
    let (p, rep) = btc_of(&mut f, &w, T + ROUND_MS);
    assert_eq!(p, Some(650_000));
    assert_eq!(rep.markets[0].result.as_ref().unwrap().sources, 5);
}

/// A majority of venues moved together is indistinguishable from a real move for one feeder (the stated bound
/// of README section 1); the engine's jump rule (H2) is what halts the market.
#[test]
fn a_venue_majority_moves_the_feeder_and_the_engine_halts() {
    let mut f = Feeder::new(default_config()).unwrap();
    let mut w = world(T, 65_000.0, 0.999);
    for s in [
        "coinbase-BTC-USD",
        "kraken-BTC-USD",
        "bitstamp-BTC-USD",
        "gemini-BTC-USD",
    ] {
        set(&mut w, s, body(s, "78000", "78001", "78000", T));
    }
    let (p, _) = btc_of(&mut f, &w, T);
    assert_eq!(p, Some(780_000));
    // Engine side: from an active index at 650000, every honest signer reports 780000 (+20%): halted, index kept.
    let prm = CommitteeParams {
        verifying_contract: VAULT,
        signers: (0..7)
            .map(|i| sig::address_of(dev_key(&format!("s{i}")).verifying_key()))
            .collect(),
        quorum: 5,
        max_future_ms: 1000,
        max_age_ms: 6000,
        markets: vec![mp(1, 500)],
    };
    let mut st = OracleState::genesis(&prm).unwrap();
    let mk = |r: u64, p: u64| OracleUpdate {
        round: r,
        observations: (0..7u8)
            .map(|i| {
                let o = perps_oracle::types::Observation {
                    round: r,
                    time_ms: r * ROUND_MS + 10,
                    prices: vec![perps_oracle::types::OraclePrice {
                        market_id: 1,
                        price: p,
                        sources: 7,
                    }],
                };
                SignedObservation {
                    signer: i,
                    time_ms: o.time_ms,
                    prices: o.prices.clone(),
                    signature: sig::sign(
                        &dev_key(&format!("s{i}")),
                        &eip712::digest(&eip712::domain_separator(&VAULT), &o),
                    ),
                }
            })
            .collect(),
    };
    let r0 = round_of(T);
    for k in 0..3 {
        apply_update(
            &prm,
            &mut st,
            &mk(r0 + k, 650_000),
            (r0 + k) * ROUND_MS + 500,
        )
        .unwrap();
    }
    let out = apply_update(
        &prm,
        &mut st,
        &mk(r0 + 3, 780_000),
        (r0 + 3) * ROUND_MS + 500,
    )
    .unwrap();
    assert_eq!(
        out.outcomes[0].1,
        MarketOutcome::HaltedJump { price: 780_000 }
    );
    assert_eq!(st.safe_index(1), None);
}

#[test]
fn four_venues_split_two_two_abstains() {
    let mut cfg = default_config();
    for m in &mut cfg.markets {
        m.sources.truncate(4);
        // Below the default of 3 so that the strict-majority rule, not the source minimum, is what refuses.
        m.min_sources = 2;
    }
    let mut f = Feeder::new(cfg).unwrap();
    let mut w = world(T, 65_000.0, 0.999);
    for s in ["coinbase-BTC-USD", "kraken-BTC-USD"] {
        set(&mut w, s, body(s, "70000", "70001", "70000", T));
    }
    let (p, rep) = btc_of(&mut f, &w, T);
    assert_eq!(p, None);
    assert!(
        rep.markets[0]
            .result
            .as_ref()
            .unwrap_err()
            .starts_with("no majority"),
        "{:?}",
        rep.markets[0].result
    );
}

#[test]
fn usdt_depeg_drops_usdt_quoted_venues() {
    let mut f = Feeder::new(default_config()).unwrap();
    // USDT at 0.94 on every USD venue: outside the 5% bound.
    let (o, rep) = f.observe(round_of(T), T, &world(T, 65_000.0, 0.94));
    assert_eq!(rep.usdt_rate18, None);
    assert_eq!(reason(&rep, 1, "okx-BTC-USDT"), Some("no-usdt-rate"));
    assert_eq!(reason(&rep, 1, "binance-BTC-USDT"), Some("no-usdt-rate"));
    let o = o.unwrap();
    assert!(o.prices.iter().all(|p| p.sources == 5));
    assert_eq!(o.prices[0].price, 650_000);
}

#[test]
fn usdt_rate_needs_its_own_quorum() {
    let mut f = Feeder::new(default_config()).unwrap();
    let mut w = world(T, 65_000.0, 0.999);
    for s in ["coinbase-USDT-USD", "kraken-USDT-USD"] {
        w.iter_mut().find(|r| r.source == s).unwrap().body = None;
    }
    let (_, rep) = f.observe(round_of(T), T, &w);
    assert_eq!(
        rep.usdt_rate18, None,
        "2 of 4 rate venues: below usdtMinSources 3"
    );
    assert_eq!(reason(&rep, 1, "okx-BTC-USDT"), Some("no-usdt-rate"));
}

#[test]
fn malformed_and_foreign_bodies_are_invalid_not_misread() {
    let mut f = Feeder::new(default_config()).unwrap();
    let mut w = world(T, 65_000.0, 0.999);
    set(
        &mut w,
        "okx-BTC-USDT",
        body("okx-ETH-USDT", "1800", "1800.1", "1800", T),
    );
    set(
        &mut w,
        "binance-BTC-USDT",
        r#"{"symbol":"ETHUSDT","bidPrice":"1800","askPrice":"1800.1"}"#.into(),
    );
    set(
        &mut w,
        "bitfinex-BTC-USD",
        "[6.5e4,1,6.5e4,1,0,0,6.5e4,1,1,1,1]".into(),
    );
    set(
        &mut w,
        "kraken-BTC-USD",
        r#"{"error":["EService:Unavailable"],"result":{}}"#.into(),
    );
    set(&mut w, "gemini-BTC-USD", "<html>rate limited</html>".into());
    set(
        &mut w,
        "coinbase-BTC-USD",
        r#"{"ask":"65003","bid":"64997","price":"-65000","time":"2026-11-17T00:00:00Z"}"#.into(),
    );
    let (p, rep) = btc_of(&mut f, &w, T);
    for (s, want) in [
        ("okx-BTC-USDT", "parse: response for another pair: ETH-USDT"),
        (
            "binance-BTC-USDT",
            "parse: response for another pair: ETHUSDT",
        ),
        ("bitfinex-BTC-USD", "parse: bad number in BID"),
        (
            "kraken-BTC-USD",
            "parse: venue error: [\"EService:Unavailable\"]",
        ),
        ("gemini-BTC-USD", "parse: not json"),
        ("coinbase-BTC-USD", "parse: bad number in price"),
    ] {
        assert_eq!(reason(&rep, 1, s), Some(want), "{s}");
    }
    // One venue left: abstain for BTC (minSources 3); ETH and SOL unaffected.
    assert_eq!(p, None);
    assert!(rep.markets[1].result.is_ok() && rep.markets[2].result.is_ok());
}

#[test]
fn stale_crossed_and_wide_readings_are_invalid() {
    let mut f = Feeder::new(default_config()).unwrap();
    let mut w = world(T, 65_000.0, 0.999);
    set(
        &mut w,
        "bitstamp-BTC-USD",
        body("bitstamp-BTC-USD", "65000", "65001", "65000", T - 10_000),
    );
    set(
        &mut w,
        "okx-BTC-USDT",
        body("okx-BTC-USDT", "65100", "65000", "65050", T),
    );
    set(
        &mut w,
        "coinbase-BTC-USD",
        body("coinbase-BTC-USD", "64000", "66000", "65000", T),
    );
    let (_, rep) = btc_of(&mut f, &w, T);
    assert_eq!(reason(&rep, 1, "bitstamp-BTC-USD"), Some("venue-time-old"));
    assert_eq!(reason(&rep, 1, "okx-BTC-USDT"), Some("crossed"));
    assert_eq!(reason(&rep, 1, "coinbase-BTC-USD"), Some("spread"));
    // A venue time in the future (more than 2 s after the receive time).
    let mut w = world(T, 65_000.0, 0.999);
    set(
        &mut w,
        "okx-BTC-USDT",
        body("okx-BTC-USDT", "65000", "65001", "65000", T + 5000),
    );
    let (_, rep) = btc_of(&mut f, &w, T + ROUND_MS);
    assert_eq!(reason(&rep, 1, "okx-BTC-USDT"), Some("venue-time-future"));
}

#[test]
fn a_frozen_venue_is_detected_while_the_market_moves() {
    let mut f = Feeder::new(default_config()).unwrap();
    let stuck = body("gemini-BTC-USD", "65000", "65001", "65000", T);
    let mut last = None;
    // 25 rounds (75 s): the market rises 0.5% overall; Gemini keeps serving the same quote.
    for k in 0..25u64 {
        let t = T + k * ROUND_MS;
        let mut w = world(t, 65_000.0 * (1.0 + 0.0002 * k as f64), 0.999);
        set(&mut w, "gemini-BTC-USD", stuck.clone());
        let (_, rep) = btc_of(&mut f, &w, t);
        last = reason(&rep, 1, "gemini-BTC-USD").map(str::to_string);
        if k * ROUND_MS < 60_000 {
            assert_ne!(
                last.as_deref(),
                Some("frozen"),
                "round {k}: too early to call it frozen"
            );
        }
    }
    assert_eq!(last.as_deref(), Some("frozen"));
}

#[test]
fn nothing_to_sign_when_every_fetch_fails() {
    let mut f = Feeder::new(default_config()).unwrap();
    let mut w = world(T, 65_000.0, 0.999);
    for r in &mut w {
        r.body = None;
        r.error = Some("timeout".into());
    }
    let (o, rep) = f.observe(round_of(T), T, &w);
    assert!(o.is_none());
    assert!(rep.markets.iter().all(|m| m.result.is_err()));
    assert_eq!(
        reason(&rep, 1, "coinbase-BTC-USD"),
        Some("fetch-failed: timeout")
    );
}

/// Seven feeders observe the recorded rounds, sign, the observations are assembled, encoded, decoded and applied
/// by the engine rule from genesis: BTC, ETH and SOL become active after three confirming rounds and then move.
#[test]
fn recorded_rounds_end_to_end_through_the_engine_rule() {
    let keys: Vec<_> = (0..7).map(|i| dev_key(&format!("e2e-{i}"))).collect();
    let prm = CommitteeParams {
        verifying_contract: VAULT,
        signers: keys
            .iter()
            .map(|k| sig::address_of(k.verifying_key()))
            .collect(),
        quorum: 5,
        max_future_ms: 1000,
        max_age_ms: 6000,
        markets: vec![mp(1, 500), mp(2, 500), mp(3, 800)],
    };
    let mut st = OracleState::genesis(&prm).unwrap();
    let mut feeders: Vec<Feeder> = (0..7)
        .map(|_| Feeder::new(default_config()).unwrap())
        .collect();
    let mut outcomes = Vec::new();
    for d in round_dirs() {
        let (t0, resp) = load_dir(&d);
        let r = round_of(t0);
        let mut obs = Vec::new();
        for (i, f) in feeders.iter_mut().enumerate() {
            let (o, _) = f.observe(r, t0, &resp);
            let o = o.unwrap();
            let s = SignedObservation {
                signer: i as u8,
                time_ms: o.time_ms,
                prices: o.prices.clone(),
                signature: sig::sign(
                    &keys[i],
                    &eip712::digest(&eip712::domain_separator(&VAULT), &o),
                ),
            };
            // Through the submission JSON, as the sequencer receives it.
            let (rr, back) = json::signed_from_json(&json::signed_to_json(&s, r)).unwrap();
            assert_eq!((rr, &back), (r, &s));
            obs.push(back);
        }
        let u = OracleUpdate {
            round: r,
            observations: obs,
        };
        let u = json::update_from_json(&json::update_to_json(&u)).unwrap();
        let u = encoding::decode(&encoding::encode(&u)).unwrap();
        let t = t0 + 900; // the block that carries the round
        begin_block(&prm, &mut st, t);
        outcomes.push(apply_update(&prm, &mut st, &u, t).unwrap());
    }
    assert!(
        outcomes[2].outcomes.iter().all(|(_, o)| matches!(
            o,
            MarketOutcome::Resumed {
                confirmed: true,
                ..
            }
        )),
        "{:?}",
        outcomes[2]
    );
    for o in &outcomes[3..] {
        assert!(
            o.outcomes
                .iter()
                .all(|(_, o)| matches!(o, MarketOutcome::Moved { .. })),
            "{o:?}"
        );
    }
    for m in [1, 2, 3] {
        assert!(st.safe_index(m).is_some());
    }
}

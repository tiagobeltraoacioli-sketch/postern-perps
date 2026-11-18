//! Round 2 (agent 432): the engine price scale (R432-1), Kraken batching, the replay tape and the replay feed up
//! to the engine's own acceptance rule, the dev committee files, the ingest wire form. Throwaway keys only.

use std::collections::BTreeMap;
use std::fs;
use std::io::{Read, Write};
use std::net::TcpListener;
use std::path::{Path, PathBuf};

use perps_oracle::committee::{
    apply_update, begin_block, CommitteeParams, HaltReason, MarketOutcome, MarketParams,
    OracleError, OracleState, Status,
};
use perps_oracle::types::round_of;
use perps_oracle::{eip712, sig, ROUND_MS};
use perps_oracle_feeder::json;
use perps_oracle_feeder::keyfile::{dev_key, DevCommittee};
use perps_oracle_feeder::pipeline::{default_config, Feeder};
use perps_oracle_feeder::recording::{
    load_recording, load_round_dir, parse_round_line, round_line, Fetch,
};
use perps_oracle_feeder::replay::{self, build_update, Clock, Mode, Options};
use perps_oracle_feeder::submit::NodeClient;
use perps_oracle_feeder::tape;
use perps_oracle_feeder::venues::{batch_kraken, default_sources, ParseError};
use serde_json::Value;

const VAULT: [u8; 20] = [0xdb; 20];
/// SPEC section 1: `PRICE_SCALE = 10^8` for every market.
const PRICE_SCALE: u64 = 100_000_000;

fn fixtures() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../fixtures")
}

fn keys(n: usize) -> Vec<k256::ecdsa::SigningKey> {
    let mut k: Vec<_> = (0..n).map(|i| dev_key(&format!("r2-oracle-{i}"))).collect();
    k.sort_by_key(|k| sig::address_of(k.verifying_key()));
    k
}

fn committee(keys: &[k256::ecdsa::SigningKey], q: u8) -> CommitteeParams {
    let mp = |id, jump| MarketParams {
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
    };
    let p = CommitteeParams {
        verifying_contract: VAULT,
        signers: keys
            .iter()
            .map(|k| sig::address_of(k.verifying_key()))
            .collect(),
        quorum: q,
        max_future_ms: 1000,
        max_age_ms: 6000,
        markets: vec![mp(1, 500), mp(2, 500), mp(3, 800)],
    };
    p.validate().unwrap();
    p
}

// ---- R432-1: the feeder signs prices in the engine's price scale ----

/// R432-1 (Medium). SPEC section 1 fixes ONE price scale, 1e-8 USD, for every price the engine reads; the round-1
/// feeder rounded to the market's DISPLAY decimals (BTC 64896.7 -> 648967), which the engine would read as
/// 0.00648967 USD. Recorded round of 2026-11-17: BTC ~64.9k, ETH ~1.8k, SOL ~72.5 USD.
#[test]
fn r432_1_feeder_prices_are_in_the_engine_price_scale() {
    let (t0, resp) = load_round_dir(&fixtures().join("rounds/round-598296643")).unwrap();
    let mut f = Feeder::new(default_config()).unwrap();
    let (obs, _) = f.observe(round_of(t0), t0, &resp);
    let p: BTreeMap<u32, u64> = obs
        .unwrap()
        .prices
        .iter()
        .map(|p| (p.market_id, p.price))
        .collect();
    let usd = |m: u32| p[&m] / PRICE_SCALE;
    assert!(
        (60_000..70_000).contains(&usd(1)),
        "BTC reads as {} USD",
        usd(1)
    );
    assert!(
        (1_500..2_500).contains(&usd(2)),
        "ETH reads as {} USD",
        usd(2)
    );
    assert!((50..100).contains(&usd(3)), "SOL reads as {} USD", usd(3));
    assert!(default_config()
        .markets
        .iter()
        .all(|m| m.price_decimals == 8));
}

/// The golden file of the six recorded rounds at the engine scale, and its relation to the round-1 golden file
/// (display decimals, still checked by `pipeline::recorded_rounds_replay_to_the_golden_file`): the same median,
/// so rounding the 1e-8 price half up to the display decimals gives the round-1 number.
/// Regenerate on purpose only: `ORACLE_WRITE_VECTORS=1 cargo test --test r2`.
#[test]
fn recorded_rounds_replay_to_the_engine_scale_golden_file() {
    let rounds = load_recording(&fixtures().join("rounds")).unwrap();
    let mut f = Feeder::new(default_config().with_kraken_batch()).unwrap();
    let mut lines = Vec::new();
    for (label, t0, resp) in &rounds {
        let (obs, _) = f.observe(round_of(*t0), *t0, resp);
        let prices: Vec<Value> = obs
            .unwrap()
            .prices
            .iter()
            .map(json::price_to_json)
            .collect();
        lines.push(
            serde_json::json!({"dir": label, "round": round_of(*t0), "prices": prices}).to_string(),
        );
    }
    let path = fixtures().join("rounds/expected-1e8.jsonl");
    if std::env::var_os("ORACLE_WRITE_VECTORS").is_some() {
        fs::write(&path, lines.join("\n") + "\n").unwrap();
    }
    let golden = fs::read_to_string(&path).unwrap();
    assert_eq!(golden.lines().collect::<Vec<_>>(), lines);
    let old = fs::read_to_string(fixtures().join("rounds/expected.jsonl")).unwrap();
    for (new, old) in lines.iter().zip(old.lines()) {
        let (new, old): (Value, Value) = (
            serde_json::from_str(new).unwrap(),
            serde_json::from_str(old).unwrap(),
        );
        assert_eq!(new["round"], old["round"]);
        for (i, display_decimals) in [1u32, 2, 3].iter().enumerate() {
            let n: u64 = new["prices"][i]["price"].as_str().unwrap().parse().unwrap();
            let o: u64 = old["prices"][i]["price"].as_str().unwrap().parse().unwrap();
            let div = 10u64.pow(8 - display_decimals);
            assert_eq!((n + div / 2) / div, o, "market {} of {}", i + 1, new["dir"]);
            assert_eq!(new["prices"][i]["sources"], old["prices"][i]["sources"]);
        }
    }
}

// ---- Kraken batching ----

fn kraken_bodies() -> Vec<(String, Value)> {
    ["BTC", "ETH", "SOL", "USDT"]
        .iter()
        .map(|b| {
            let v: Value = serde_json::from_str(
                &fs::read_to_string(fixtures().join(format!("recorded/kraken-{b}-USD.json")))
                    .unwrap(),
            )
            .unwrap();
            let body: Value = serde_json::from_str(v["body"].as_str().unwrap()).unwrap();
            let (k, t) = body["result"].as_object().unwrap().iter().next().unwrap();
            (k.clone(), t.clone())
        })
        .collect()
}

fn batched_sources() -> Vec<perps_oracle_feeder::venues::Source> {
    let mut all: Vec<_> = ["USDT", "BTC", "ETH", "SOL"]
        .iter()
        .flat_map(|b| default_sources(b))
        .filter(|s| s.operator == "kraken")
        .collect();
    let mut refs: Vec<&mut _> = all.iter_mut().collect();
    batch_kraken(&mut refs);
    all
}

#[test]
fn kraken_batch_one_request_and_each_source_reads_its_own_pair() {
    let srcs = batched_sources();
    assert_eq!(srcs.len(), 4);
    assert!(srcs.iter().all(
        |s| s.url == "https://api.kraken.com/0/public/Ticker?pair=USDTUSD,XBTUSD,ETHUSD,SOLUSD"
    ));
    // The whole default configuration: 22 requests per round instead of 25, one of them to Kraken.
    let c = default_config().with_kraken_batch();
    let mut urls: Vec<&str> = c
        .usdt_sources
        .iter()
        .chain(c.markets.iter().flat_map(|m| m.sources.iter()))
        .map(|s| s.url.as_str())
        .collect();
    assert_eq!(urls.len(), 25);
    urls.sort_unstable();
    urls.dedup();
    assert_eq!(urls.len(), 22);
    assert_eq!(urls.iter().filter(|u| u.contains("kraken")).count(), 1);

    // A batched body built from the four recorded single-pair bodies.
    let result: serde_json::Map<String, Value> = kraken_bodies().into_iter().collect();
    let batched = serde_json::json!({"error": [], "result": result}).to_string();
    for s in &srcs {
        let single: Value = serde_json::from_str(
            &fs::read_to_string(fixtures().join(format!("recorded/{}.json", s.name))).unwrap(),
        )
        .unwrap();
        let alone = perps_oracle_feeder::venues::Source::new(s.adapter, s.base, s.quote)
            .parse(single["body"].as_str().unwrap(), 1)
            .unwrap();
        assert_eq!(s.parse(&batched, 1).unwrap(), alone, "{}", s.name);
        // A batched source also reads a single-pair answer for its own pair (subset of what it asked for).
        assert_eq!(s.parse(single["body"].as_str().unwrap(), 1).unwrap(), alone);
    }
}

#[test]
fn kraken_batch_refuses_missing_foreign_and_ambiguous_pairs() {
    let srcs = batched_sources();
    let btc = srcs.iter().find(|s| s.base == "BTC").unwrap();
    let bodies = kraken_bodies();
    let with = |keys: &[(&str, usize)]| -> String {
        let result: serde_json::Map<String, Value> = keys
            .iter()
            .map(|(k, i)| (k.to_string(), bodies[*i].1.clone()))
            .collect();
        serde_json::json!({"error": [], "result": result}).to_string()
    };
    // Its own pair is missing.
    assert!(matches!(
        btc.parse(&with(&[("XETHZUSD", 1), ("SOLUSD", 2), ("USDTZUSD", 3)]), 1),
        Err(ParseError::WrongPair(_))
    ));
    // A pair nobody asked for rides along: the whole body is refused (the venue is not answering our question).
    assert!(matches!(
        btc.parse(&with(&[("XXBTZUSD", 0), ("XDGUSD", 1)]), 1),
        Err(ParseError::WrongPair(_))
    ));
    // The same pair under both of its names: refused (which one is the price?).
    assert!(matches!(
        btc.parse(&with(&[("XXBTZUSD", 0), ("XBTUSD", 1)]), 1),
        Err(ParseError::WrongPair(_))
    ));
    // An unbatched source still refuses an answer for another pair (round-1 property).
    let single = perps_oracle_feeder::venues::Source::new(btc.adapter, btc.base, btc.quote);
    assert!(matches!(
        single.parse(&with(&[("XETHZUSD", 1)]), 1),
        Err(ParseError::WrongPair(_))
    ));
    assert!(matches!(
        single.parse(&with(&[("XXBTZUSD", 0), ("XETHZUSD", 1)]), 1),
        Err(ParseError::WrongPair(_))
    ));
    // A Kraken error body.
    assert!(matches!(
        btc.parse("{\"error\":[\"EGeneral:Too many requests\"]}", 1),
        Err(ParseError::VenueError(_))
    ));
}

// ---- recording line form ----

#[test]
fn recording_line_form_round_trips_and_feeds_the_pipeline() {
    // The six recorded rounds rewritten in the line form (Kraken as ONE batched fetch) replay to the same prices.
    let dir_rounds = load_recording(&fixtures().join("rounds")).unwrap();
    let srcs = batched_sources();
    let mut text = String::new();
    for (_, t0, resp) in &dir_rounds {
        let mut fetches = Vec::new();
        let mut kraken: serde_json::Map<String, Value> = serde_json::Map::new();
        for r in resp {
            if r.source.starts_with("kraken") {
                let b: Value = serde_json::from_str(r.body.as_deref().unwrap()).unwrap();
                kraken.extend(b["result"].as_object().unwrap().clone());
                continue;
            }
            fetches.push(Fetch {
                url: format!("https://example.invalid/{}", r.source),
                sources: vec![r.source.clone()],
                status: 200,
                received_ms: r.received_ms,
                http_date_ms: None,
                error: None,
                body: r.body.clone(),
                dropped: false,
            });
        }
        fetches.push(Fetch {
            url: srcs[0].url.clone(),
            sources: srcs.iter().map(|s| s.name.clone()).collect(),
            status: 200,
            received_ms: *t0 + 400,
            http_date_ms: Some(*t0),
            error: None,
            body: Some(serde_json::json!({"error": [], "result": kraken}).to_string()),
            dropped: false,
        });
        let line = round_line(round_of(*t0), *t0, &fetches).to_string();
        let (sent, back) = parse_round_line(&line).unwrap();
        assert_eq!((sent, &back), (*t0, &fetches));
        text.push_str(&line);
        text.push('\n');
    }
    let tmp = std::env::temp_dir().join(format!("a432-recording-{}.jsonl", std::process::id()));
    fs::write(&tmp, &text).unwrap();
    let line_rounds = load_recording(&tmp).unwrap();
    fs::remove_file(&tmp).unwrap();
    let run = |rounds: &[(String, u64, Vec<perps_oracle_feeder::pipeline::Response>)]| {
        let mut f = Feeder::new(default_config().with_kraken_batch()).unwrap();
        rounds
            .iter()
            .map(|(_, t0, r)| f.observe(round_of(*t0), *t0, r).0.unwrap().prices)
            .collect::<Vec<_>>()
    };
    assert_eq!(run(&dir_rounds), run(&line_rounds));

    // A dropped unit and a failed unit give their sources no reading; the market still prices from the others.
    let (t0, mut fetches) = parse_round_line(text.lines().next().unwrap()).unwrap();
    let k = fetches.iter().position(|f| f.sources.len() == 4).unwrap();
    fetches[k].dropped = true;
    fetches[0].status = 429;
    fetches[0].error = Some("http-status-429".into());
    let (_, back) = parse_round_line(&round_line(round_of(t0), t0, &fetches).to_string()).unwrap();
    let resp: Vec<_> = back.iter().flat_map(|f| f.responses(t0)).collect();
    let mut f = Feeder::new(default_config().with_kraken_batch()).unwrap();
    let (obs, rep) = f.observe(round_of(t0), t0, &resp);
    assert_eq!(obs.unwrap().prices.len(), 3);
    let reasons: Vec<String> = rep.markets[0]
        .sources
        .iter()
        .filter_map(|s| s.reason.clone())
        .collect();
    assert!(
        reasons.iter().any(|r| r == "fetch-failed: dropped"),
        "{reasons:?}"
    );
    assert!(
        reasons.iter().any(|r| r == "fetch-failed: http-status-429"),
        "{reasons:?}"
    );
}

// ---- the tape, from recorded venue data to the engine rule ----

/// A scripted clock: `sleep_until` jumps; every `now` costs 1 ms.
struct FakeClock(u64);

impl Clock for FakeClock {
    fn now_ms(&mut self) -> u64 {
        self.0 += 1;
        self.0
    }
    fn sleep_until_ms(&mut self, t: u64) {
        self.0 = self.0.max(t);
    }
}

fn opts(mode: Mode) -> Options {
    Options {
        speed: 1,
        looped: false,
        mode,
        offset_ms: 150,
        max_rounds: None,
    }
}

fn recorded_tape() -> String {
    let rounds = load_recording(&fixtures().join("rounds")).unwrap();
    let mut f = Feeder::new(default_config().with_kraken_batch()).unwrap();
    rounds
        .iter()
        .map(|(label, t0, resp)| {
            let o = f.observe(round_of(*t0), *t0, resp).0.unwrap();
            let p: BTreeMap<u32, (u64, u8)> = o
                .prices
                .iter()
                .map(|p| (p.market_id, (p.price, p.sources)))
                .collect();
            tape::row(&p, &[], label)
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// Recorded venue responses -> feeder pipeline -> tape -> signed rounds at the run's clock -> the engine's own
/// acceptance rule (`core::committee`): every round is accepted, the three markets leave INIT after
/// `confirmRounds` rounds and the index is the tape's price, round for round. No network anywhere.
#[test]
fn replay_of_recorded_rounds_makes_the_markets_active_under_the_engine_rule() {
    let text = recorded_tape();
    let steps = tape::parse(&text).unwrap();
    assert_eq!(steps.len(), 6);
    let ks = keys(7);
    let prm = committee(&ks, 5);
    let mut st = OracleState::genesis(&prm).unwrap();
    let mut clock = FakeClock(1_800_000_000_000);
    let mut log = Vec::new();
    let sum = replay::run(
        &steps,
        &ks,
        &VAULT,
        None,
        &opts(Mode::Updates),
        &mut clock,
        &mut log,
    )
    .unwrap();
    assert_eq!((sum.rounds, sum.silent), (6, 0));
    let lines: Vec<Value> = String::from_utf8(log)
        .unwrap()
        .lines()
        .map(|l| serde_json::from_str(l).unwrap())
        .collect();
    let mut last_round = 0;
    for (i, l) in lines.iter().enumerate() {
        let u = json::update_from_json(&l["update"]).unwrap();
        assert_eq!(u.observations.len(), 7);
        assert_eq!(u.round, last_round.max(u.round));
        assert!(
            u.round > last_round,
            "one update per round, strictly increasing"
        );
        last_round = u.round;
        // The block that carries it: 200 ms after the sampling time.
        let t = l["timeMs"].as_u64().unwrap() + 200;
        begin_block(&prm, &mut st, t);
        let rep = apply_update(&prm, &mut st, &u, t).unwrap();
        for (m, o) in &rep.outcomes {
            let want = steps[i].prices[m].0;
            match (i, o) {
                (0 | 1, MarketOutcome::Confirming { price, .. }) => assert_eq!(*price, want),
                (
                    2,
                    MarketOutcome::Resumed {
                        price,
                        confirmed: true,
                    },
                ) => assert_eq!(*price, want),
                (3.., MarketOutcome::Moved { price }) => assert_eq!(*price, want),
                other => panic!("round {i} market {m}: {other:?}"),
            }
        }
    }
    for m in [1, 2, 3] {
        let mo = st.market(m).unwrap();
        assert_eq!(mo.status, Status::Active);
        assert_eq!(mo.index, steps[5].prices[&m].0);
        assert_eq!(st.safe_index(m), Some(steps[5].prices[&m].0));
    }
    // Deterministic: the same tape and clock give byte-identical updates (RFC 6979 signatures).
    let mut log2 = Vec::new();
    replay::run(
        &steps,
        &ks,
        &VAULT,
        None,
        &opts(Mode::Updates),
        &mut FakeClock(1_800_000_000_000),
        &mut log2,
    )
    .unwrap();
    assert_eq!(
        String::from_utf8(log2)
            .unwrap()
            .lines()
            .map(|l| serde_json::from_str::<Value>(l).unwrap())
            .collect::<Vec<_>>(),
        lines
    );
}

/// An authored tape (435's rehearsal shape): a 12% BTC drop in steps below `jumpBps` keeps the market active all
/// the way down; a gap beyond `jumpBps` halts it (JUMP) and three agreeing rounds confirm the new level; a silent
/// stretch halts it STALE; a lying signer and a five-signer round do not move the median.
#[test]
fn authored_tape_drives_drop_gap_stale_and_liar_through_the_engine_rule() {
    let text = r#"{"p":{"1":"67000","2":"3500","3":"180"},"repeat":3}
{"ramp":{"1":"58960"},"rounds":40,"note":"12% down, 0.3% per round"}
{"p":{"1":"52000"},"repeat":3,"note":"gap of 11.8%: JUMP halt, then confirmation"}
{"silent":5}
{"p":{"1":"52100"},"repeat":3}
{"p":{"1":"52200"},"override":{"6":{"1":"60000"},"5":{"1":"1"}},"note":"two liars"}
{"p":{"1":"52300"},"signers":[0,1,2,3,4]}
{"p":{"1":"52400"},"signers":[0,1,2,3]}"#;
    let steps = tape::parse(text).unwrap();
    let ks = keys(7);
    let prm = committee(&ks, 5);
    let mut st = OracleState::genesis(&prm).unwrap();
    let mut t = 1_800_000_000_150u64;
    let mut btc = Vec::new();
    for s in &steps {
        t += ROUND_MS;
        begin_block(&prm, &mut st, t + 100);
        let res = build_update(s, &ks, &VAULT, t).map(|u| apply_update(&prm, &mut st, &u, t + 100));
        let m = st.market(1).unwrap();
        btc.push((m.status, m.index, res));
    }
    let u8s = 100_000_000u64;
    // Rounds 0..2 confirm; the 40-round ramp never halts and ends exactly on the target.
    assert!(btc[2..43].iter().all(|x| x.0 == Status::Active));
    assert_eq!(btc[42].1, 58_960 * u8s);
    // The gap: halted for JUMP with the index unchanged, resumed on the third agreeing round.
    assert_eq!(
        (btc[43].0, btc[43].1),
        (Status::Halted(HaltReason::Jump), 58_960 * u8s)
    );
    assert_eq!(btc[44].0, Status::Halted(HaltReason::Jump));
    assert_eq!((btc[45].0, btc[45].1), (Status::Active, 52_000 * u8s));
    // Five silent rounds (15 s > maxStalenessMs 10 s): STALE, the index stays the last good one.
    assert!(btc[46..51].iter().all(|x| x.2.is_none()));
    assert_eq!(
        (btc[50].0, btc[50].1),
        (Status::Halted(HaltReason::Stale), 52_000 * u8s)
    );
    // Back after three rounds.
    assert_eq!((btc[53].0, btc[53].1), (Status::Active, 52_100 * u8s));
    // Two liars of seven: the median is an honest value.
    assert_eq!((btc[54].0, btc[54].1), (Status::Active, 52_200 * u8s));
    // Exactly the quorum signs: accepted.
    assert_eq!(btc[55].1, 52_300 * u8s);
    // Four signers: the engine refuses the whole input (C2), nothing moves.
    assert!(
        matches!(btc[56].2, Some(Err(OracleError::Malformed(_)))),
        "{:?}",
        btc[56].2
    );
    assert_eq!(btc[56].1, 52_300 * u8s);
}

// ---- dev committee files ----

#[test]
fn dev_committee_is_random_per_run_private_on_disk_and_checked_on_load() {
    let dir = std::env::temp_dir().join(format!("a432-committee-{}", std::process::id()));
    let _ = fs::remove_dir_all(&dir);
    let c = DevCommittee::generate(7, 5).unwrap();
    c.write_dir(&dir).unwrap();
    // Not overwritten: a second write into the same directory is refused.
    assert!(DevCommittee::generate(7, 5)
        .unwrap()
        .write_dir(&dir)
        .is_err());
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        assert_eq!(
            fs::metadata(&dir).unwrap().permissions().mode() & 0o777,
            0o700
        );
        for i in 0..7 {
            let m = fs::metadata(dir.join(format!("oracle-{i}.key"))).unwrap();
            assert_eq!(m.permissions().mode() & 0o777, 0o600);
        }
    }
    let back = DevCommittee::read_dir(&dir).unwrap();
    assert_eq!(back.addresses(), c.addresses());
    assert!(c.addresses().windows(2).all(|w| w[0] < w[1]));
    let pubj: Value =
        serde_json::from_str(&fs::read_to_string(dir.join("committee.json")).unwrap()).unwrap();
    assert_eq!(pubj["quorum"], 5);
    assert_eq!(pubj["committeeArg"].as_str().unwrap().split(',').count(), 7);
    // The public file holds no key material.
    let text = fs::read_to_string(dir.join("committee.json")).unwrap();
    for i in 0..7 {
        let k = fs::read_to_string(dir.join(format!("oracle-{i}.key"))).unwrap();
        assert!(!text.contains(k.trim()));
    }
    // Two generations never give the same committee.
    assert_ne!(
        DevCommittee::generate(7, 5).unwrap().addresses(),
        c.addresses()
    );
    // A swapped key file is refused.
    fs::rename(dir.join("oracle-0.key"), dir.join("x")).unwrap();
    fs::rename(dir.join("oracle-1.key"), dir.join("oracle-0.key")).unwrap();
    fs::rename(dir.join("x"), dir.join("oracle-1.key")).unwrap();
    assert!(DevCommittee::read_dir(&dir).is_err());
    fs::remove_dir_all(&dir).unwrap();
    // Quorum must be a strict majority.
    assert!(DevCommittee::generate(7, 3).is_err());
    assert!(DevCommittee::generate(7, 8).is_err());
}

// ---- the ingest wire form, over a loopback socket ----

/// A one-thread HTTP sink on loopback: records each request (head and body) and answers with the scripted status.
fn sink(port: u16, statuses: Vec<u16>) -> std::thread::JoinHandle<Vec<(String, String)>> {
    let l = TcpListener::bind(("127.0.0.1", port)).unwrap();
    std::thread::spawn(move || {
        let mut got = Vec::new();
        for status in statuses {
            let (mut s, _) = l.accept().unwrap();
            let mut raw = Vec::new();
            let mut buf = [0u8; 4096];
            let (head, body) = loop {
                let n = s.read(&mut buf).unwrap();
                raw.extend_from_slice(&buf[..n]);
                let text = String::from_utf8_lossy(&raw).into_owned();
                if let Some((h, b)) = text.split_once("\r\n\r\n") {
                    let len: usize = h
                        .lines()
                        .find_map(|l| l.strip_prefix("Content-Length: "))
                        .unwrap()
                        .parse()
                        .unwrap();
                    if b.len() >= len {
                        break (h.to_string(), b.to_string());
                    }
                }
                assert!(n > 0, "request cut short");
            };
            let reply = format!(
                "{{\"round\":1,\"held\":7,\"quorum\":5,\"status\":\"{}\"}}",
                if status == 200 { "ready" } else { "refused" }
            );
            write!(s, "HTTP/1.1 {status} X\r\ncontent-type: application/json\r\ncontent-length: {}\r\n\r\n{reply}", reply.len()).unwrap();
            got.push((head, body));
        }
        got
    })
}

/// The agreed ingest (cross-area log 2026-11-18, 431): `POST /v1/oracle/updates` with a README-4 `OracleUpdate`,
/// `Authorization: Bearer <token>`; and `POST /v1/oracle/observations` with `{"round","observation"}` per signer.
#[test]
fn replay_feed_posts_the_agreed_wire_form_with_the_bearer_token() {
    let ks = keys(7);
    let prm = committee(&ks, 5);
    let steps = tape::parse("{\"p\":{\"1\":\"67000.5\",\"3\":\"180.125\"},\"repeat\":2}").unwrap();
    let token = "throwaway-test-token-0123456789".to_string();

    // Mode updates: one POST per round; the second is refused by the node (409): the run goes on and reports it.
    let h = sink(53_210, vec![200, 409]);
    let node = NodeClient::new("http://127.0.0.1:53210", Some(token.clone())).unwrap();
    let mut log = Vec::new();
    let sum = replay::run(
        &steps,
        &ks,
        &VAULT,
        Some(&node),
        &opts(Mode::Updates),
        &mut FakeClock(1_800_000_000_000),
        &mut log,
    )
    .unwrap();
    assert_eq!(
        (sum.rounds, sum.accepted, sum.refused, sum.unreachable),
        (2, 1, 1, 0)
    );
    let got = h.join().unwrap();
    let dom = eip712::domain_separator(&VAULT);
    for (head, body) in &got {
        assert!(
            head.starts_with("POST /v1/oracle/updates HTTP/1.1\r\n"),
            "{head}"
        );
        assert!(head.contains("\r\nAuthorization: Bearer throwaway-test-token-0123456789"));
        assert!(head.contains("\r\nContent-Type: application/json"));
        let v: Value = serde_json::from_str(body).unwrap();
        // JSON numbers for round, signer, timeMs, marketId, sources; the price a decimal string at 1e-8 USD.
        assert!(v["round"].is_u64() && v["observations"][0]["timeMs"].is_u64());
        assert_eq!(
            v["observations"][0]["prices"][0],
            serde_json::json!({"marketId": 1, "price": "6700050000000", "sources": 5})
        );
        assert_eq!(v["observations"][0]["prices"][1]["price"], "18012500000");
        let u = json::update_from_json(&v).unwrap();
        assert_eq!(u.observations.len(), 7);
        for o in &u.observations {
            assert_eq!(round_of(o.time_ms), u.round);
            assert_eq!(
                sig::recover(&eip712::digest(&dom, &o.message(u.round)), &o.signature).unwrap(),
                prm.signers[usize::from(o.signer)]
            );
        }
        // The engine rule accepts it as it arrived.
        let mut st = OracleState::genesis(&prm).unwrap();
        apply_update(&prm, &mut st, &u, u.observations[0].time_ms + 50).unwrap();
    }
    // The token is never written to the run log.
    assert!(!String::from_utf8(log).unwrap().contains(&token));

    // Mode observations: one POST per signer.
    let h = sink(53_211, vec![200; 7]);
    let node = NodeClient::new("http://127.0.0.1:53211", Some(token)).unwrap();
    let mut o = opts(Mode::Observations);
    o.max_rounds = Some(1);
    let sum = replay::run(
        &steps,
        &ks,
        &VAULT,
        Some(&node),
        &o,
        &mut FakeClock(1_800_000_000_000),
        &mut Vec::new(),
    )
    .unwrap();
    assert_eq!((sum.rounds, sum.accepted), (1, 1));
    for (i, (head, body)) in h.join().unwrap().iter().enumerate() {
        assert!(head.starts_with("POST /v1/oracle/observations HTTP/1.1\r\n"));
        let v: Value = serde_json::from_str(body).unwrap();
        assert_eq!(v.as_object().unwrap().len(), 2);
        assert_eq!(v["observation"]["signer"], i);
        let mut flat = v["observation"].clone();
        flat["round"] = v["round"].clone();
        let (round, so) = json::signed_from_json(&flat).unwrap();
        assert_eq!(
            sig::recover(&eip712::digest(&dom, &so.message(round)), &so.signature).unwrap(),
            prm.signers[i]
        );
    }

    // Nobody listening: the round is reported unreachable, the run still ends normally.
    let node = NodeClient::new("http://127.0.0.1:53212", None).unwrap();
    let sum = replay::run(
        &steps,
        &ks,
        &VAULT,
        Some(&node),
        &opts(Mode::Updates),
        &mut FakeClock(1_800_000_000_000),
        &mut Vec::new(),
    )
    .unwrap();
    assert_eq!((sum.rounds, sum.accepted, sum.unreachable), (2, 0, 2));
}

/// The replay clock: one step per round, never two updates in a round, a lost round is skipped and its step
/// played in the next one, `--speed` and `--rounds` as documented.
#[test]
fn replay_schedule_one_step_per_round() {
    let ks = keys(3);
    let steps = tape::parse("{\"p\":{\"1\":\"1\"}}\n{\"p\":{\"1\":\"2\"}}\n{\"silent\":1}\n{\"p\":{\"1\":\"4\"}}\n{\"p\":{\"1\":\"5\"}}").unwrap();
    let run = |o: Options, start: u64| -> Vec<Value> {
        let mut log = Vec::new();
        replay::run(
            &steps,
            &ks,
            &VAULT,
            None,
            &o,
            &mut FakeClock(start),
            &mut log,
        )
        .unwrap();
        String::from_utf8(log)
            .unwrap()
            .lines()
            .map(|l| serde_json::from_str(l).unwrap())
            .collect()
    };
    // Started late in a round (2.9 s in): the first step waits for the next round.
    let start = 1_800_000_000_000u64 + 2_900;
    let l = run(opts(Mode::Updates), start);
    let rounds: Vec<u64> = l.iter().map(|x| x["round"].as_u64().unwrap()).collect();
    let r0 = round_of(start) + 1;
    assert_eq!(rounds, (r0..r0 + 5).collect::<Vec<_>>());
    assert!(l
        .iter()
        .all(|x| x["timeMs"].as_u64().unwrap() % ROUND_MS >= 150));
    assert_eq!(l[2]["silent"], true);
    assert_eq!(
        l.iter()
            .map(|x| x["step"].as_u64().unwrap())
            .collect::<Vec<_>>(),
        [0, 1, 2, 3, 4]
    );
    // Speed 2: steps 0, 2, 4. Rounds 2: stops after two.
    let mut o = opts(Mode::Updates);
    o.speed = 2;
    assert_eq!(
        run(o, start)
            .iter()
            .map(|x| x["step"].as_u64().unwrap())
            .collect::<Vec<_>>(),
        [0, 2, 4]
    );
    let mut o = opts(Mode::Updates);
    o.max_rounds = Some(2);
    assert_eq!(run(o, start).len(), 2);
    // Looped: ping-pong 0 1 2 3 4 3 2 1 0 1.
    let mut o = opts(Mode::Updates);
    o.looped = true;
    o.max_rounds = Some(10);
    assert_eq!(
        run(o, start)
            .iter()
            .map(|x| x["step"].as_u64().unwrap())
            .collect::<Vec<_>>(),
        [0, 1, 2, 3, 4, 3, 2, 1, 0, 1]
    );
}

/// The node's dev oracle committee for seed `dev` (`node/src/dev.rs`), re-derived here from the same public label.
/// Expected addresses computed independently with Foundry: `cast wallet address --private-key
/// $(cast keccak "postern-perps THROWAWAY dev key | dev/oracle | <i> | 0")`, sorted.
#[test]
fn node_dev_seed_committee_matches_the_independent_derivation() {
    let c = DevCommittee::node_dev("dev", 7, 5).unwrap();
    let got: Vec<String> = c
        .addresses()
        .iter()
        .map(|a| format!("0x{}", hex::encode(a)))
        .collect();
    assert_eq!(
        got,
        [
            "0x008b32d0d6800a94384dfcea267474534303cea4",
            "0x3e0acedd937c6f4c0cb776c5bb1296144cc0a793",
            "0x50693116acb0e3b9572f1df8145b77d39afe4a5f",
            "0x70d215ae79a6a72106f21d2ebcf3fe1f514fad45",
            "0xc04208e60e4fb1afc65177f4ceaa855a8b414da1",
            "0xcc023509057ad5f2d6bb12144c5cbea0b13af90f",
            "0xebc1490fff441f0ca483b10c59ae051fd81f9440",
        ]
    );
    assert_ne!(
        DevCommittee::node_dev("other", 7, 5).unwrap().addresses(),
        c.addresses()
    );
}

/// Per-round view of one market after a tape is played through the engine rule with 3 s rounds and a block
/// 100 ms after each sampling time.
fn play(text: &str, market: u32) -> Vec<(Status, u64)> {
    let steps = tape::parse(text).unwrap();
    let ks = keys(7);
    let prm = committee(&ks, 5);
    let mut st = OracleState::genesis(&prm).unwrap();
    let mut t = 1_800_000_000_150u64;
    let mut out = Vec::new();
    for s in &steps {
        t += ROUND_MS;
        begin_block(&prm, &mut st, t + 100);
        if let Some(u) = build_update(s, &ks, &VAULT, t) {
            apply_update(&prm, &mut st, &u, t + 100).unwrap();
        }
        let m = st.market(market).unwrap();
        out.push((m.status, m.index));
    }
    out
}

/// Every committed tape parses and does what its notes say under the engine rule.
#[test]
fn committed_tapes_do_what_their_notes_say() {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("../tapes");
    let read = |n: &str| fs::read_to_string(dir.join(n)).unwrap();
    let u = PRICE_SCALE;

    let flat = play(&read("flat-dev-prices.jsonl"), 1);
    assert_eq!(flat.len(), 1200);
    assert!(flat[2..].iter().all(|x| *x == (Status::Active, 67_250 * u)));

    let text = read("rehearsal-example.jsonl");
    let (btc, eth) = (play(&text, 1), play(&text, 2));
    assert_eq!(btc.len(), 10 + 40 + 10 + 4 + 3 + 3 + 5 + 10);
    // BTC: active from round 2 through the whole 12% ramp, at the target afterwards.
    assert!(btc[2..50].iter().all(|x| x.0 == Status::Active));
    assert_eq!(btc[9].1, 67_250 * u);
    assert_eq!(btc[49].1, 59_180 * u);
    assert!(
        btc[10..50].windows(2).all(|w| w[1].1 < w[0].1),
        "strictly down during the ramp"
    );
    // ETH gap at round 60: JUMP halt, the index unchanged for two rounds, resumed at 3100 on the third.
    assert_eq!(eth[59], (Status::Active, 3_480 * u));
    assert_eq!(eth[60], (Status::Halted(HaltReason::Jump), 3_480 * u));
    assert_eq!(eth[61], (Status::Halted(HaltReason::Jump), 3_480 * u));
    assert_eq!(eth[62], (Status::Active, 3_100 * u));
    // Liars (rounds 64..66) and quorum-only rounds (67..69): BTC follows the honest price.
    assert!(btc[64..67]
        .iter()
        .all(|x| *x == (Status::Active, 59_200 * u)));
    assert!(btc[67..70]
        .iter()
        .all(|x| *x == (Status::Active, 59_210 * u)));
    // Silent rounds 70..74: STALE by the fourth (12 s without an update), for every market.
    assert_eq!(btc[74], (Status::Halted(HaltReason::Stale), 59_210 * u));
    assert_eq!(eth[74].0, Status::Halted(HaltReason::Stale));
    // Back on the third round after the silence.
    assert_eq!(btc[76].0, Status::Halted(HaltReason::Stale));
    assert_eq!(btc[77], (Status::Active, 59_220 * u));
    assert_eq!(btc[84], (Status::Active, 59_220 * u));

    // Every tape in the directory parses.
    for e in fs::read_dir(&dir).unwrap() {
        let p = e.unwrap().path();
        if p.extension().is_some_and(|x| x == "jsonl") {
            tape::parse(&fs::read_to_string(&p).unwrap())
                .unwrap_or_else(|e| panic!("{}: {e}", p.display()));
        }
    }
}

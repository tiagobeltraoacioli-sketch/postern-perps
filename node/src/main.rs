//! `postern-perps-node`: dev mode (sequencer + in-process validators with THROWAWAY keys), log verification.
//! Live mode is deliberately not available in this prototype: it needs operator key files and a reviewed
//! deployment; no agent generates or handles a real key.
//!
//! UNAUDITED PROTOTYPE.

use perps_node::api::router;
use perps_node::dev::dev_genesis;
use perps_node::engine::stub::StubEngine;
use perps_node::engine::Engine;
use perps_node::geo::GeoPolicy;
use perps_node::node::{run_producer, spawn_local_validators, App, Mode};
use perps_node::sequencer::{RateLimit, Sequencer};
use perps_node::watcher::{self, L2View, WatcherConfig};
use std::collections::HashMap;
use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

fn args() -> (String, HashMap<String, String>) {
    let mut it = std::env::args().skip(1);
    let cmd = it.next().unwrap_or_else(|| "help".into());
    let mut m = HashMap::new();
    let rest: Vec<String> = it.collect();
    let mut i = 0;
    while i < rest.len() {
        if let Some(k) = rest[i].strip_prefix("--") {
            let v = if i + 1 < rest.len() && !rest[i + 1].starts_with("--") {
                i += 1;
                rest[i].clone()
            } else {
                "true".into()
            };
            m.insert(k.to_string(), v);
        }
        i += 1;
    }
    (cmd, m)
}

fn get<T: std::str::FromStr>(m: &HashMap<String, String>, k: &str, d: T) -> T {
    m.get(k).and_then(|v| v.parse().ok()).unwrap_or(d)
}

#[tokio::main]
async fn main() {
    let (cmd, a) = args();
    match cmd.as_str() {
        "dev" => dev(a).await,
        "verify-log" => verify_log(a),
        "dev-genesis" => dev_genesis_print(a),
        "validator" => remote_validator(a).await,
        "live" => {
            eprintln!("live mode is not available in this UNAUDITED prototype (operator key files, reviewed deployment)");
            std::process::exit(2);
        }
        _ => {
            eprintln!("usage: postern-perps-node dev --data DIR [--listen 127.0.0.1:48900] [GENESIS] [--local-validators N] [--block-ms 200] [--snapshot-every 1000] [--engine real|stub] [--l2 http://A,http://B] [--l2-start-block N] [--l2-poll-ms 1000] [--token 0x..] [--oracle-token-file F] [--oracle-gather-ms 500] [--geoblock FILE] [--ip-rate 50] [--account-rate 20] [--dev-default-country ZZ] [--edge-secret-file F] [--dev-oracle]");
            eprintln!("       postern-perps-node validator --index I --sequencer http://127.0.0.1:48900 [--data DIR] [GENESIS] [--l2 http://A,http://B] [--l2-start-block N] [--l2-poll-ms 1000]");
            eprintln!("       postern-perps-node verify-log --data DIR [GENESIS] [--engine real|stub]");
            eprintln!("       postern-perps-node dev-genesis [GENESIS]      (prints the dev genesis as JSON: addresses only, no key)");
            eprintln!("  GENESIS (set once; every process of one deployment gets the same): [--seed dev] [--validators 7] [--threshold 5] [--checkpoint-interval 10] [--genesis-time MS] [--vault 0x..] [--oracle-committee 0xA,0xB,.. --oracle-quorum Q | --oracle-committee-file committee.json] [--dev-funding-interval-ms MS]");
            std::process::exit(2);
        }
    }
}

/// The GENESIS flags (usage text) -> dev options. Any malformed value stops the process: a silently ignored flag
/// would start a different deployment than the operator named.
fn die(m: String) -> ! {
    eprintln!("{m}");
    std::process::exit(2)
}

fn dev_options(a: &HashMap<String, String>) -> perps_node::dev::DevOptions {
    let vault = a.get("vault").map(|v| perps_node::util::parse_address(v).unwrap_or_else(|e| die(format!("--vault: {e}"))));
    let oracle = if let Some(f) = a.get("oracle-committee-file") {
        // agent 432's committee.json: {"quorum": q, "signers": ["0x..", ...]} (addresses ascending = signer index)
        let j: serde_json::Value = serde_json::from_slice(&std::fs::read(f).unwrap_or_else(|e| die(format!("--oracle-committee-file: {e}")))).unwrap_or_else(|e| die(format!("--oracle-committee-file: {e}")));
        let q = j["quorum"].as_u64().unwrap_or_else(|| die("--oracle-committee-file: quorum".into())) as usize;
        let list: Vec<String> = j["signers"].as_array().unwrap_or_else(|| die("--oracle-committee-file: signers".into())).iter().filter_map(|x| x.as_str().map(|s| s.to_string())).collect();
        Some((perps_node::genesis::parse_address_list(&list).unwrap_or_else(|e| die(format!("--oracle-committee-file: {e}"))), q))
    } else if let Some(c) = a.get("oracle-committee") {
        let list: Vec<String> = c.split(',').map(|x| x.trim().to_string()).collect();
        let q: usize = a.get("oracle-quorum").and_then(|q| q.parse().ok()).unwrap_or_else(|| die("--oracle-committee needs --oracle-quorum Q".into()));
        Some((perps_node::genesis::parse_address_list(&list).unwrap_or_else(|e| die(format!("--oracle-committee: {e}"))), q))
    } else {
        None
    };
    let funding_interval_ms = a.get("dev-funding-interval-ms").map(|v| v.parse::<u64>().unwrap_or_else(|_| die("--dev-funding-interval-ms: milliseconds".into())));
    perps_node::dev::DevOptions { vault, oracle, funding_interval_ms }
}

fn real_genesis(a: &HashMap<String, String>) -> (Arc<perps_node::genesis::Genesis>, perps_node::dev::DevKeys, postern_perps_engine::genesis::Checked) {
    let seed = a.get("seed").cloned().unwrap_or_else(|| "dev".into());
    match perps_node::dev::dev_genesis_real_with(&seed, get(a, "validators", 7), get(a, "threshold", 5), get(a, "checkpoint-interval", 10), get(a, "genesis-time", 0), &dev_options(a)) {
        Ok(x) => x,
        Err(e) => {
            eprintln!("genesis refused: {e}");
            std::process::exit(2);
        }
    }
}

/// `--l2 A,B` with the watcher's flags.
fn watcher_config(a: &HashMap<String, String>, vault: perps_node::util::Address) -> Option<WatcherConfig> {
    let s = a.get("l2")?;
    let v: Vec<String> = s.split(',').map(|x| x.to_string()).collect();
    if v.len() != 2 {
        eprintln!("--l2 takes exactly two endpoints (two independent operators)");
        std::process::exit(2);
    }
    let mut cfg = WatcherConfig::new([v[0].clone(), v[1].clone()], vault);
    cfg.start_block = get(a, "l2-start-block", cfg.start_block);
    cfg.poll_ms = get(a, "l2-poll-ms", cfg.poll_ms).max(50);
    Some(cfg)
}

/// Resolves when the process is asked to stop (SIGINT or SIGTERM).
async fn stop_signal() {
    use tokio::signal::unix::{signal, SignalKind};
    let mut term = signal(SignalKind::terminate()).expect("SIGTERM handler");
    tokio::select! {
        _ = tokio::signal::ctrl_c() => {}
        _ = term.recv() => {}
    }
}

/// Prints the dev genesis (ADDRESSES only: the throwaway keys are derived from the seed inside each process and
/// never printed), so the vault can be deployed with this committee before any node starts.
fn dev_genesis_print(a: HashMap<String, String>) {
    let (g, _keys, checked) = real_genesis(&a);
    let hx = |x: &[u8]| perps_node::util::to_hex(x);
    println!(
        "{}",
        serde_json::json!({
            "unaudited": true, "throwawayKeys": true, "seed": a.get("seed").cloned().unwrap_or_else(|| "dev".into()),
            "collateralChainId": 700770, "vault": hx(&g.vault), "feeRecipient": hx(&g.fee_recipient),
            "committee": {"validators": g.committee.iter().map(|x| hx(x)).collect::<Vec<_>>(), "threshold": g.threshold, "setHash": hx(&g.committee_set_hash())},
            "sequencer": hx(&g.sequencer),
            "oracle": {"signers": g.oracle_committee.iter().map(|x| hx(x)).collect::<Vec<_>>(), "quorum": g.oracle_quorum},
            "engineDomain": g.engine_domain.to_json(), "vaultDomain": g.vault_domain.to_json(),
            "checkpointInterval": g.checkpoint_interval, "genesisTime": g.genesis_time, "genesisLogHash": hx(&g.genesis_log_hash()),
            "withdrawalFee": g.withdrawal_fee.to_string(),
            "markets": checked.g.markets.iter().map(|m| serde_json::json!({"marketId": m.market_id, "symbol": m.symbol, "fundingIntervalMs": m.funding_interval_ms})).collect::<Vec<_>>(),
        })
    );
}

/// Dev: one validator process (`--index i` of the seed's throwaway committee) following `--sequencer URL`.
async fn remote_validator(a: HashMap<String, String>) {
    let (g, keys, checked) = real_genesis(&a);
    let i: usize = get(&a, "index", 0);
    if i >= keys.validators.len() {
        eprintln!("--index must be below the committee size {}", keys.validators.len());
        std::process::exit(2);
    }
    let view = Arc::new(Mutex::new(L2View::default()));
    let (tx, rx) = tokio::sync::watch::channel(false);
    if let Some(cfg) = watcher_config(&a, g.vault) {
        tokio::spawn(watcher::run(cfg, view.clone(), rx.clone()));
    }
    let e = Box::new(perps_node::engine::real::RealEngine::new(g.clone(), checked).expect("engine"));
    let v = perps_node::validator::Validator::new(g.clone(), keys.validators[i].clone(), e, view, a.get("data").map(PathBuf::from)).expect("validator");
    let base = a.get("sequencer").cloned().unwrap_or_else(|| "http://127.0.0.1:48900".into());
    eprintln!("postern-perps-node validator {} (UNAUDITED, THROWAWAY key) following {base}", perps_node::util::to_hex(&v.address));
    tokio::select! {
        r = perps_node::node::run_remote_validator(base, v, rx) => {
            if let Err(e) = r {
                eprintln!("{e}");
                std::process::exit(1);
            }
        }
        _ = stop_signal() => {
            let _ = tx.send(true);
        }
    }
}

fn verify_log(a: HashMap<String, String>) {
    let seed = a.get("seed").cloned().unwrap_or_else(|| "dev".into());
    let (n, th, ci, gt) = (get(&a, "validators", 7), get(&a, "threshold", 5), get(&a, "checkpoint-interval", 10), get(&a, "genesis-time", 0));
    let (g, k, e): (Arc<perps_node::genesis::Genesis>, _, Box<dyn Engine>) = if a.get("engine").map(|s| s.as_str()).unwrap_or("real") == "real" {
        let (g, k, c) = real_genesis(&a);
        let e = Box::new(perps_node::engine::real::RealEngine::new(g.clone(), c).expect("engine"));
        (g, k, e)
    } else {
        let (g, k) = dev_genesis(&seed, n, th, ci, gt);
        let e = Box::new(StubEngine::new(g.clone()));
        (g, k, e)
    };
    let data = PathBuf::from(a.get("data").expect("--data"));
    let view = Arc::new(Mutex::new(L2View::default()));
    match Sequencer::open(g.clone(), k.sequencer, e, data, view) {
        Ok(s) => {
            println!(
                "{}",
                serde_json::json!({"ok": true, "height": s.log.last_height, "logHash": perps_node::util::to_hex(&s.log.last_log_hash),
                "stateRoot": perps_node::util::to_hex(&s.engine.state_root()), "snapshot": s.recovered.snapshot_height,
                "replayed": s.recovered.replayed_blocks, "tornTailBytes": s.recovered.torn_tail_bytes, "checkpoints": s.recovered.checkpoints_loaded,
                "indexDigest": s.index.as_ref().map(|ix| perps_node::util::to_hex(&ix.digest())), "indexMismatches": s.index.as_ref().map(|ix| ix.mismatches)})
            );
        }
        Err(e) => {
            println!("{}", serde_json::json!({"ok": false, "error": e}));
            std::process::exit(1);
        }
    }
}

async fn dev(a: HashMap<String, String>) {
    let seed = a.get("seed").cloned().unwrap_or_else(|| "dev".into());
    let n: usize = get(&a, "validators", 7);
    let th: usize = get(&a, "threshold", 5);
    let ci: u64 = get(&a, "checkpoint-interval", 10);
    let gt: u64 = get(&a, "genesis-time", 0);
    let real = a.get("engine").map(|s| s.as_str()).unwrap_or("real") == "real";
    let (g, keys, checked) = if real {
        let (g, k, c) = real_genesis(&a);
        (g, k, Some(c))
    } else {
        let (g, k) = dev_genesis(&seed, n, th, ci, gt);
        (g, k, None)
    };
    let mk_engine = |g: &Arc<perps_node::genesis::Genesis>| -> Box<dyn Engine> {
        match &checked {
            Some(c) => Box::new(perps_node::engine::real::RealEngine::new(g.clone(), c.clone()).expect("engine")),
            None => Box::new(StubEngine::new(g.clone())),
        }
    };
    let data = PathBuf::from(a.get("data").cloned().unwrap_or_else(|| "./perps-dev-data".into()));
    let listen: SocketAddr = get(&a, "listen", "127.0.0.1:48900".parse().unwrap());
    if !listen.ip().is_loopback() {
        eprintln!("dev mode listens on loopback only");
        std::process::exit(2);
    }
    let geo_json: serde_json::Value = match a.get("geoblock") {
        Some(p) => serde_json::from_slice(&std::fs::read(p).expect("geoblock file")).expect("geoblock json"),
        None => serde_json::json!({"version": "dev-fixture", "unknown_country_effect": "deny",
            "tiers": {"allowed": {"effect": "allow"}, "blocked": {"effect": "deny"}},
            "countries": {"PA": {"tier": "allowed"}, "ZZ": {"tier": "allowed"}, "BR": {"tier": "blocked"}, "US": {"tier": "blocked"}},
            "denied_policy": {"policy": "exit_only"}}),
    };
    let mut geo = GeoPolicy::from_json(&geo_json, true).expect("geoblock policy");
    geo.add_sanctioned(&["0x000000000000000000000000000000000000dead".into()]).unwrap();
    geo.dev_default_country = a.get("dev-default-country").cloned();
    // dev: the edge secret comes from a file the operator names (never a flag value, never logged)
    if let Some(p) = a.get("edge-secret-file") {
        let sec = std::fs::read(p).expect("edge secret file");
        geo.edge = Some(Arc::new(perps_node::geo::EdgeVerifier::new(sec).expect("edge secret")));
    }
    let l2 = watcher_config(&a, g.vault);
    // how many of the committee run inside this process; the others are separate `validator --index i` processes
    let local: usize = get(&a, "local-validators", n).min(n);
    let token = match a.get("token") {
        Some(t) => perps_node::util::parse_address(t).unwrap_or_else(|e| {
            eprintln!("--token: {e}");
            std::process::exit(2)
        }),
        None => [0u8; 20],
    };
    // the oracle ingest token comes from a file the operator names (never a flag value, never logged)
    let oracle_token: Option<Vec<u8>> = a.get("oracle-token-file").map(|p| {
        let raw = std::fs::read(p).unwrap_or_else(|e| {
            eprintln!("--oracle-token-file: {e}");
            std::process::exit(2)
        });
        let t = String::from_utf8_lossy(&raw).trim().as_bytes().to_vec();
        if t.len() < 16 {
            eprintln!("--oracle-token-file: the token must be at least 16 bytes");
            std::process::exit(2);
        }
        t
    });
    let (stop_tx, stop_rx) = tokio::sync::watch::channel(false);
    let mk_view = || Arc::new(Mutex::new(L2View::default()));
    let seq_view = mk_view();
    let mut val_views = Vec::new();
    for _ in 0..local {
        val_views.push(mk_view());
    }
    if let Some(cfg) = &l2 {
        tokio::spawn(watcher::run(cfg.clone(), seq_view.clone(), stop_rx.clone()));
        for v in &val_views {
            tokio::spawn(watcher::run(cfg.clone(), v.clone(), stop_rx.clone()));
        }
    }
    let mut seq = match Sequencer::open(g.clone(), keys.sequencer.clone(), mk_engine(&g), data.clone(), seq_view.clone()) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("recovery refused: {e}");
            std::process::exit(1);
        }
    };
    seq.snapshot_every = get(&a, "snapshot-every", 1000);
    seq.rate = RateLimit::new(get(&a, "account-rate", 20.0), get(&a, "account-rate", 20.0) * 2.0);
    seq.oracle_pool.gather_ms = get(&a, "oracle-gather-ms", perps_node::oracle_pool::DEFAULT_GATHER_MS);
    eprintln!("recovered: {:?}", seq.recovered);
    let app = App::with(g.clone(), seq, geo, Mode::Dev, seq_view, get(&a, "block-ms", 200), l2.is_some(), token, oracle_token);
    *app.ip_rate.lock().unwrap() = RateLimit::new(get(&a, "ip-rate", 50.0), get(&a, "ip-rate", 50.0) * 2.0);
    let engines: Vec<Box<dyn Engine>> = (0..local).map(|_| mk_engine(&g)).collect();
    spawn_local_validators(&app, keys.validators.iter().take(local).cloned().collect(), engines, val_views, &stop_rx).expect("validators");
    tokio::spawn(run_producer(app.clone(), stop_rx.clone()));
    if a.contains_key("dev-oracle") {
        #[cfg(feature = "dev-oracle")]
        {
            eprintln!("DEV ORACLE: throwaway oracle keys, FIXED prices (not a price source)");
            if let Err(e) = perps_node::node::start_dev_oracle(app.clone(), keys.oracle.clone(), perps_node::dev::dev_prices(), stop_rx.clone()) {
                eprintln!("{e}");
                std::process::exit(2);
            }
        }
        #[cfg(not(feature = "dev-oracle"))]
        {
            eprintln!("--dev-oracle refused: this binary is built without the dev-oracle feature");
            std::process::exit(2);
        }
    }
    let listener = tokio::net::TcpListener::bind(listen).await.expect("bind");
    eprintln!("postern-perps-node dev (UNAUDITED, THROWAWAY keys) on http://{listen} data {}", data.display());
    let srv = std::future::IntoFuture::into_future(axum::serve(listener, router(app.clone()).into_make_service_with_connect_info::<SocketAddr>()).with_graceful_shutdown(stop_signal()));
    // SIGINT / SIGTERM: the server stops accepting, open WebSocket sessions get 2 s, then the tasks are told to
    // stop. Nothing is lost by stopping: every produced block was fsynced before it was published.
    tokio::select! {
        _ = srv => {}
        _ = async { stop_signal().await; tokio::time::sleep(std::time::Duration::from_secs(2)).await } => {}
    }
    let _ = stop_tx.send(true);
    eprintln!("postern-perps-node: stopped");
}

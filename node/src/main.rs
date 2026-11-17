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
        "live" => {
            eprintln!("live mode is not available in this UNAUDITED prototype (operator key files, reviewed deployment)");
            std::process::exit(2);
        }
        _ => {
            eprintln!("usage: postern-perps-node dev --data DIR [--listen 127.0.0.1:48900] [--seed dev] [--validators 7] [--threshold 5] [--block-ms 200] [--checkpoint-interval 10] [--snapshot-every 1000] [--engine real|stub] [--l2 http://A,http://B] [--geoblock FILE] [--ip-rate 50] [--account-rate 20]");
            eprintln!("       postern-perps-node verify-log --data DIR [--seed dev] [--validators 7] [--threshold 5] [--checkpoint-interval 10] [--genesis-time MS]");
            std::process::exit(2);
        }
    }
}

fn verify_log(a: HashMap<String, String>) {
    let seed = a.get("seed").cloned().unwrap_or_else(|| "dev".into());
    let (g, k) = dev_genesis(&seed, get(&a, "validators", 7), get(&a, "threshold", 5), get(&a, "checkpoint-interval", 10), get(&a, "genesis-time", 0));
    let data = PathBuf::from(a.get("data").expect("--data"));
    let view = Arc::new(Mutex::new(L2View::default()));
    match Sequencer::open(g.clone(), k.sequencer, Box::new(StubEngine::new(g.clone())), data, view) {
        Ok(s) => {
            println!(
                "{}",
                serde_json::json!({"ok": true, "height": s.log.last_height, "logHash": perps_node::util::to_hex(&s.log.last_log_hash),
                "stateRoot": perps_node::util::to_hex(&s.engine.state_root()), "snapshot": s.recovered.snapshot_height,
                "replayed": s.recovered.replayed_blocks, "tornTailBytes": s.recovered.torn_tail_bytes, "checkpoints": s.recovered.checkpoints_loaded})
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
        let (g, k, c) = perps_node::dev::dev_genesis_real(&seed, n, th, ci, gt);
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
    let l2: Option<[String; 2]> = a.get("l2").map(|s| {
        let v: Vec<String> = s.split(',').map(|x| x.to_string()).collect();
        assert!(v.len() == 2, "--l2 takes exactly two endpoints");
        [v[0].clone(), v[1].clone()]
    });
    let (stop_tx, stop_rx) = tokio::sync::watch::channel(false);
    let mk_view = || Arc::new(Mutex::new(L2View::default()));
    let seq_view = mk_view();
    let mut val_views = Vec::new();
    for _ in 0..n {
        val_views.push(mk_view());
    }
    if let Some(eps) = &l2 {
        let cfg = WatcherConfig::new(eps.clone(), g.vault);
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
    eprintln!("recovered: {:?}", seq.recovered);
    let app = App::new(g.clone(), seq, geo, Mode::Dev, seq_view, get(&a, "block-ms", 200), l2.is_some());
    *app.ip_rate.lock().unwrap() = RateLimit::new(get(&a, "ip-rate", 50.0), get(&a, "ip-rate", 50.0) * 2.0);
    let engines: Vec<Box<dyn Engine>> = (0..n).map(|_| mk_engine(&g)).collect();
    spawn_local_validators(&app, keys.validators.clone(), engines, val_views, &stop_rx).expect("validators");
    tokio::spawn(run_producer(app.clone(), stop_rx.clone()));
    let listener = tokio::net::TcpListener::bind(listen).await.expect("bind");
    eprintln!("postern-perps-node dev (UNAUDITED, THROWAWAY keys) on http://{listen} data {}", data.display());
    let srv = axum::serve(listener, router(app.clone()).into_make_service_with_connect_info::<SocketAddr>()).with_graceful_shutdown(async {
        let _ = tokio::signal::ctrl_c().await;
    });
    let _ = srv.await;
    let _ = stop_tx.send(true);
}

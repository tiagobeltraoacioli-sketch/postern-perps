//! `perps-loadgen`: signs PlaceOrder actions with THROWAWAY account keys and posts them to a dev node over HTTP
//! (one connection per request), then waits until all are included and reports admission latency, admission
//! throughput, time to inclusion and the finality lag. Run against a node started with
//! `--account-rate 0 --ip-rate 0` (dev only), otherwise the rate limits are what is measured.
//!
//! UNAUDITED PROTOTYPE.

use perps_node::actions::action_digest;
use perps_node::dev::dev_genesis;
use perps_node::sig::{key_address, sign, throwaway_key};
use perps_node::types::SignedAction;
use perps_node::util::to_hex;
use perps_node::watcher::http_call;
use std::sync::Arc;
use std::time::Instant;

#[tokio::main]
async fn main() {
    let mut a = std::collections::HashMap::new();
    let v: Vec<String> = std::env::args().skip(1).collect();
    for w in v.chunks(2) {
        if w.len() == 2 {
            a.insert(w[0].trim_start_matches("--").to_string(), w[1].clone());
        }
    }
    let url = a.get("url").cloned().unwrap_or_else(|| "http://127.0.0.1:48900".into());
    let seed = a.get("seed").cloned().unwrap_or_else(|| "dev".into());
    let accounts: u64 = a.get("accounts").and_then(|x| x.parse().ok()).unwrap_or(200);
    let total: u64 = a.get("actions").and_then(|x| x.parse().ok()).unwrap_or(20_000);
    let conc: usize = a.get("concurrency").and_then(|x| x.parse().ok()).unwrap_or(64);
    let ci: u64 = a.get("checkpoint-interval").and_then(|x| x.parse().ok()).unwrap_or(10);
    let (g, _) = dev_genesis(&seed, 7, 5, ci, 0);
    let keys: Vec<_> = (0..accounts).map(|i| throwaway_key("loadgen", i)).collect();
    let base = perps_node::node::now_ms();
    eprintln!("signing {total} actions for {accounts} accounts");
    let t_sign = Instant::now();
    let mut reqs = Vec::with_capacity(total as usize);
    for i in 0..total {
        let k = &keys[(i % accounts) as usize];
        let acct = key_address(k);
        let nonce = base + i / accounts;
        let msg = serde_json::json!({"account": to_hex(&acct), "nonce": nonce.to_string(), "marketId": 1 + (i % 3) as u32, "isBuy": i % 2 == 0,
            "orderType": 0, "timeInForce": 0, "price": (600_000 + (i % 1000)).to_string(), "size": "1", "reduceOnly": false, "triggerPrice": "0",
            "clientOrderId": to_hex(&[0u8; 32])});
        let val = g.types.registry.val_from_json("PlaceOrder", &msg).unwrap();
        let d = action_digest(&g, "PlaceOrder", &val).unwrap();
        let sa = SignedAction { type_index: g.types.index_of("PlaceOrder").unwrap(), message: val, signature: sign(k, &d) };
        reqs.push(serde_json::json!({"type": "PlaceOrder", "message": msg, "signature": to_hex(&sa.signature), "wire": true}).to_string());
    }
    let sign_s = t_sign.elapsed().as_secs_f64();
    // one task per account, each sending its own actions in nonce order (a signer's nonces must arrive
    // increasing); `concurrency` bounds the in-flight requests across accounts
    let sem = Arc::new(tokio::sync::Semaphore::new(conc));
    let lat = Arc::new(std::sync::Mutex::new(Vec::with_capacity(total as usize)));
    let errs = Arc::new(std::sync::Mutex::new(std::collections::BTreeMap::<String, u64>::new()));
    let mut per: Vec<Vec<String>> = vec![Vec::new(); accounts as usize];
    for (i, b) in reqs.into_iter().enumerate() {
        per[i % accounts as usize].push(b);
    }
    let t0 = Instant::now();
    let mut hs = Vec::new();
    for bodies in per {
        let (url, lat, errs, sem) = (url.clone(), lat.clone(), errs.clone(), sem.clone());
        hs.push(tokio::spawn(async move {
            for body in bodies {
                let p = sem.acquire().await.unwrap();
                let t = Instant::now();
                let r = http_call("POST", &format!("{url}/v1/actions"), Some(&body), &[("X-Mock-Country", "PA")]).await;
                let dt = t.elapsed().as_micros() as u64;
                drop(p);
                match r {
                    Ok((200, _)) => lat.lock().unwrap().push(dt),
                    Ok((s, b)) => {
                        let code = serde_json::from_slice::<serde_json::Value>(&b).ok().and_then(|j| j["error"]["code"].as_str().map(|s| s.to_string())).unwrap_or_default();
                        *errs.lock().unwrap().entry(format!("{s} {code}")).or_default() += 1;
                    }
                    Err(e) => *errs.lock().unwrap().entry(e).or_default() += 1,
                }
            }
        }));
    }
    for h in hs {
        let _ = h.await;
    }
    let adm_s = t0.elapsed().as_secs_f64();
    // wait for inclusion
    let mut included_s = f64::NAN;
    let mut st = serde_json::Value::Null;
    for _ in 0..600 {
        if let Ok((200, b)) = http_call("GET", &format!("{url}/v1/status"), None, &[]).await {
            st = serde_json::from_slice(&b).unwrap_or_default();
            if st["sequencer"]["pendingActions"] == 0 {
                included_s = t0.elapsed().as_secs_f64();
                break;
            }
        }
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    }
    tokio::time::sleep(std::time::Duration::from_millis(1500)).await;
    let st2: serde_json::Value = http_call("GET", &format!("{url}/v1/status"), None, &[]).await.ok().and_then(|(_, b)| serde_json::from_slice(&b).ok()).unwrap_or_default();
    let mut l = lat.lock().unwrap().clone();
    l.sort_unstable();
    let pct = |p: f64| -> u64 { if l.is_empty() { 0 } else { l[((l.len() as f64 - 1.0) * p) as usize] } };
    println!(
        "{}",
        serde_json::json!({
            "actions": total, "accounts": accounts, "concurrency": conc, "signSeconds": sign_s,
            "accepted": l.len(), "errors": *errs.lock().unwrap(),
            "admissionSeconds": adm_s, "admissionPerSecond": l.len() as f64 / adm_s,
            "admissionLatencyMicros": {"p50": pct(0.5), "p90": pct(0.9), "p99": pct(0.99), "max": pct(1.0)},
            "allIncludedSeconds": included_s,
            "latestBlock": st["latestBlock"], "latestFinalizedAfter1.5s": st2["latestFinalized"], "latestBlockAfter1.5s": st2["latestBlock"],
            "validators": st2["validators"],
        })
    );
}

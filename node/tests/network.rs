//! Loopback tests: the L2 watcher against two fake L2 JSON-RPC endpoints, and the full HTTP / WebSocket API with
//! the block producer and seven in-process validators.

mod common;
use axum::extract::State;
use axum::routing::post;
use axum::{Json, Router};
use common::*;
use futures_util::{SinkExt, StreamExt};
use perps_node::checkpoint::verify_checkpoint_json;
use perps_node::engine::stub::StubEngine;
use perps_node::engine::Engine;
use perps_node::geo::GeoPolicy;
use perps_node::node::{run_producer, spawn_local_validators, App, Mode};
use perps_node::sequencer::Sequencer;
use perps_node::sig::key_address;
use perps_node::util::{keccak, to_hex, word_address, word_u128, Address, H256};
use perps_node::watcher::{http_call, poll_once, WatcherConfig};
use std::collections::BTreeMap;
use std::net::SocketAddr;
use std::sync::{Arc, Mutex};
use std::time::Duration;

#[derive(Clone, Default)]
struct FakeL2 {
    chain_id: u64,
    finalized: u64,
    hashes: BTreeMap<u64, H256>,
    logs: Vec<serde_json::Value>,
}

type Fake = Arc<Mutex<FakeL2>>;

fn q(n: u64) -> String {
    format!("0x{n:x}")
}

fn pq(v: &serde_json::Value) -> u64 {
    u64::from_str_radix(v.as_str().unwrap().trim_start_matches("0x"), 16).unwrap()
}

async fn fake_rpc(State(f): State<Fake>, Json(req): Json<serde_json::Value>) -> Json<serde_json::Value> {
    let f = f.lock().unwrap().clone();
    let p = &req["params"];
    let result = match req["method"].as_str().unwrap() {
        "eth_chainId" => serde_json::json!(q(f.chain_id)),
        "eth_getBlockByNumber" => {
            let n = if p[0] == "finalized" { f.finalized } else { pq(&p[0]) };
            match f.hashes.get(&n) {
                Some(h) if n <= f.finalized || p[0] != "finalized" => serde_json::json!({"number": q(n), "hash": to_hex(h)}),
                _ => serde_json::Value::Null,
            }
        }
        "eth_getLogs" => {
            let from = pq(&p[0]["fromBlock"]);
            let to = pq(&p[0]["toBlock"]);
            serde_json::Value::Array(f.logs.iter().filter(|l| { let b = pq(&l["blockNumber"]); b >= from && b <= to }).cloned().collect())
        }
        _ => serde_json::Value::Null,
    };
    Json(serde_json::json!({"jsonrpc": "2.0", "id": req["id"], "result": result}))
}

async fn start_fake(f: Fake) -> String {
    let l = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let a = l.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(l, Router::new().route("/", post(fake_rpc)).with_state(f)).await.unwrap() });
    format!("http://{a}/")
}

fn chain(n: u64, salt: &[u8]) -> BTreeMap<u64, H256> {
    (0..=n).map(|i| (i, keccak(&[salt, &i.to_be_bytes()].concat()))).collect()
}

/// A vault `Deposit` log (VAULT.md) with the correct chain head after `prev`.
fn deposit_log(vault: &Address, block: u64, bhash: &H256, li: u64, index: u64, acct: &Address, amount: u64, prev: &H256) -> (serde_json::Value, H256) {
    let head = perps_node::vaultfmt::deposit_head_next(prev, index, acct, amount);
    let mut data = word_u128(amount as u128).to_vec();
    data.extend_from_slice(&head);
    (
        serde_json::json!({"address": to_hex(vault), "blockNumber": q(block), "blockHash": to_hex(bhash), "transactionHash": to_hex(&keccak(&index.to_be_bytes())),
            "logIndex": q(li), "removed": false,
            "topics": [to_hex(&keccak(b"Deposit(uint64,address,address,uint64,bytes32)")), to_hex(&word_u128(index as u128)), to_hex(&word_address(acct)), to_hex(&word_address(acct))],
            "data": to_hex(&data)}),
        head,
    )
}

#[tokio::test]
async fn watcher_needs_two_agreeing_endpoints_pinned_to_one_hash() {
    let vault = [0x11u8; 20];
    let acct = [0x22u8; 20];
    let hashes = chain(20, b"canon");
    let h0 = perps_node::vaultfmt::deposit_head0(700770, &vault);
    let (l0, h1) = deposit_log(&vault, 5, &hashes[&5], 0, 0, &acct, 1_000_000, &h0);
    let (l1, h2) = deposit_log(&vault, 7, &hashes[&7], 2, 1, &acct, 2_000_000, &h1);
    let (l2, h3) = deposit_log(&vault, 12, &hashes[&12], 0, 2, &acct, 3, &h2);
    let (l2bad, _) = deposit_log(&vault, 12, &hashes[&12], 0, 2, &acct, 999, &h2);
    let (l3orphan, _) = deposit_log(&vault, 18, &keccak(b"orphan"), 0, 3, &acct, 4, &h3);
    let (l5gap, _) = deposit_log(&vault, 18, &hashes[&18], 0, 5, &acct, 4, &h3);
    let (l3badhead, _) = deposit_log(&vault, 18, &hashes[&18], 0, 3, &acct, 4, &keccak(b"not the previous head"));
    let base = FakeL2 { chain_id: 700770, finalized: 10, hashes: hashes.clone(), logs: vec![l0, l1] };
    let fa: Fake = Arc::new(Mutex::new(base.clone()));
    let fb: Fake = Arc::new(Mutex::new(base.clone()));
    let (ua, ub) = (start_fake(fa.clone()).await, start_fake(fb.clone()).await);
    let cfg = WatcherConfig::new([ua, ub], vault);
    let v = view();
    assert!(poll_once(&cfg, &v).await.unwrap());
    {
        let s = v.lock().unwrap();
        assert_eq!(s.deposits.len(), 2);
        assert_eq!(s.deposits[&1].amount, 2_000_000);
        assert_eq!(s.scanned_to, 10);
        assert!(s.endpoints_agree);
    }
    // a third deposit at block 12 beyond B's finalized head: F = min(heads) = 10 -> not read yet
    fa.lock().unwrap().finalized = 15;
    fa.lock().unwrap().logs.push(l2.clone());
    poll_once(&cfg, &v).await.unwrap();
    assert_eq!(v.lock().unwrap().deposits.len(), 2);
    // B finalizes 15 but shows a different hash for block 15: no progress, disagreement flagged
    {
        let mut b = fb.lock().unwrap();
        b.finalized = 15;
        b.hashes.insert(15, keccak(b"fork"));
        b.logs.push(l2.clone());
    }
    assert!(poll_once(&cfg, &v).await.is_err());
    assert_eq!(v.lock().unwrap().deposits.len(), 2);
    assert!(!v.lock().unwrap().endpoints_agree);
    // B agrees on hashes but reports a different amount: no progress
    {
        let mut b = fb.lock().unwrap();
        b.hashes.insert(15, hashes[&15]);
        b.logs.pop();
        b.logs.push(l2bad);
    }
    assert!(poll_once(&cfg, &v).await.unwrap_err().contains("different logs"));
    // B agrees fully: progress
    {
        let mut b = fb.lock().unwrap();
        b.logs.pop();
        b.logs.push(l2.clone());
    }
    assert!(poll_once(&cfg, &v).await.unwrap());
    assert_eq!(v.lock().unwrap().deposits.len(), 3);
    // a log whose blockHash is not the canonical hash of its block (both endpoints agree on the lie): refused
    {
        for f in [&fa, &fb] {
            let mut x = f.lock().unwrap();
            x.finalized = 20;
            x.logs.push(l3orphan.clone());
        }
    }
    assert!(poll_once(&cfg, &v).await.unwrap_err().contains("hash"));
    // a gap in deposit indices: refused
    for f in [&fa, &fb] {
        let mut x = f.lock().unwrap();
        x.logs.pop();
        x.logs.push(l5gap.clone());
    }
    assert!(poll_once(&cfg, &v).await.unwrap_err().contains("gap"));
    assert_eq!(v.lock().unwrap().deposits.len(), 3);
    // the right index but a head that does not chain from the previous event: refused (VAULT.md 5.4)
    for f in [&fa, &fb] {
        let mut x = f.lock().unwrap();
        x.logs.pop();
        x.logs.push(l3badhead.clone());
    }
    assert!(poll_once(&cfg, &v).await.unwrap_err().contains("chain"));
    assert_eq!(v.lock().unwrap().deposits[&2].head, h3);
    // R395-30: a VAULT 0.2 ForcedRequest(uint64,address,uint8,uint64,uint64,bytes32) log is decoded and its head
    // chain checked (i, account, kind, deadline, arg); a wrong head is refused
    for f in [&fa, &fb] {
        f.lock().unwrap().logs.pop();
    }
    let fh0 = perps_node::vaultfmt::forced_head0(700770, &vault);
    let forced_log = |head: &H256| {
        let mut data = word_u128(2).to_vec();
        data.extend_from_slice(&word_u128(1_800_000_000));
        data.extend_from_slice(&word_u128(5_000_000));
        data.extend_from_slice(head);
        serde_json::json!({"address": to_hex(&vault), "blockNumber": q(19), "blockHash": to_hex(&hashes[&19]), "transactionHash": to_hex(&keccak(b"ftx")),
            "logIndex": q(0), "removed": false,
            "topics": [to_hex(&keccak(b"ForcedRequest(uint64,address,uint8,uint64,uint64,bytes32)")), to_hex(&word_u128(0)), to_hex(&word_address(&acct))],
            "data": to_hex(&data)})
    };
    let good_head = perps_node::vaultfmt::forced_head_next(&fh0, 0, &acct, 2, 1_800_000_000, 5_000_000);
    for f in [&fa, &fb] {
        f.lock().unwrap().logs.push(forced_log(&keccak(b"wrong")));
    }
    assert!(poll_once(&cfg, &v).await.unwrap_err().contains("chain"));
    for f in [&fa, &fb] {
        let mut x = f.lock().unwrap();
        x.logs.pop();
        x.logs.push(forced_log(&good_head));
    }
    assert!(poll_once(&cfg, &v).await.unwrap(), "{:?}", v.lock().unwrap().last_error);
    {
        let s = v.lock().unwrap();
        let fe = &s.forced[&0];
        assert_eq!((fe.kind, fe.deadline, fe.arg, fe.head), (perps_node::types::ForcedKind::Withdraw, 1_800_000_000, 5_000_000, good_head));
    }
    // wrong chain id on one endpoint: refused
    fb.lock().unwrap().chain_id = 1;
    assert!(poll_once(&cfg, &v).await.unwrap_err().contains("chain"));
}

struct Running {
    url: String,
    app: Arc<App>,
    stop: tokio::sync::watch::Sender<bool>,
    g: Arc<perps_node::genesis::Genesis>,
}

async fn start_node(name: &str) -> Running {
    let (g, keys) = perps_node::dev::dev_genesis(&format!("http-{name}"), 7, 5, 5, 0);
    let dir = tmpdir(name);
    let sv = view();
    let seq = Sequencer::open(g.clone(), keys.sequencer.clone(), Box::new(StubEngine::new(g.clone())), dir, sv.clone()).unwrap();
    let geo = GeoPolicy::from_json(
        &serde_json::json!({"version": "test", "unknown_country_effect": "deny",
            "tiers": {"allowed": {"effect": "allow"}, "blocked": {"effect": "deny"}},
            "countries": {"PA": {"tier": "allowed"}, "BR": {"tier": "blocked"}}, "denied_policy": {"policy": "exit_only"}}),
        true,
    )
    .unwrap();
    let app = App::new(g.clone(), seq, geo, Mode::Dev, sv.clone(), 50, false);
    let (tx, rx) = tokio::sync::watch::channel(false);
    let views: Vec<_> = (0..7).map(|_| sv.clone()).collect();
    let engines: Vec<Box<dyn Engine>> = (0..7).map(|_| Box::new(StubEngine::new(g.clone())) as Box<dyn Engine>).collect();
    spawn_local_validators(&app, keys.validators.clone(), engines, views, &rx).unwrap();
    tokio::spawn(run_producer(app.clone(), rx.clone()));
    let l = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = l.local_addr().unwrap();
    let r = perps_node::api::router(app.clone());
    tokio::spawn(async move { axum::serve(l, r.into_make_service_with_connect_info::<SocketAddr>()).await.unwrap() });
    Running { url: format!("http://{addr}"), app, stop: tx, g }
}

async fn get_json(url: &str) -> (u16, serde_json::Value) {
    let (s, b) = http_call("GET", url, None, &[]).await.unwrap();
    (s, serde_json::from_slice(&b).unwrap_or(serde_json::Value::Null))
}

async fn post_json(url: &str, body: &serde_json::Value, h: &[(&str, &str)]) -> (u16, serde_json::Value) {
    let (s, b) = http_call("POST", url, Some(&body.to_string()), h).await.unwrap();
    (s, serde_json::from_slice(&b).unwrap_or(serde_json::Value::Null))
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn http_api_actions_finality_geo_and_websocket() {
    let n = start_node("api").await;
    let g = n.g.clone();
    let u = user(30);
    let ua = key_address(&u);
    // fund through the sequencer's (and validators', shared here) view
    n.app.view.lock().unwrap().deposits.insert(0, deposit(0, ua, 10_000_000_000));
    tokio::time::sleep(Duration::from_millis(300)).await;
    let (s, acc) = get_json(&format!("{}/v1/accounts/{}", n.url, to_hex(&ua))).await;
    assert_eq!(s, 200);
    assert_eq!(acc["collateral"], "10000.000000");
    let (_, st) = get_json(&format!("{}/v1/status", n.url)).await;
    assert_eq!(st["unaudited"], true);
    assert_eq!(st["network"]["eip712"]["chainId"], 700770, "SPEC 2.1");
    assert_eq!(st["network"]["eip712"]["verifyingContract"], to_hex(&g.vault));
    let (_, td) = get_json(&format!("{}/v1/eip712", n.url)).await;
    assert!(td["actions"].as_array().unwrap().iter().any(|a| a == "PlaceOrder"));
    // WebSocket: status channel delivers updates
    let ws_url = format!("{}/v1/ws", n.url.replace("http://", "ws://"));
    let (mut ws, _) = tokio_tungstenite::connect_async(&ws_url).await.unwrap();
    ws.send(tokio_tungstenite::tungstenite::Message::Text(serde_json::json!({"op":"subscribe","id":1,"channel":"status"}).to_string().into())).await.unwrap();
    ws.send(tokio_tungstenite::tungstenite::Message::Text(serde_json::json!({"op":"subscribe","id":2,"channel":"account","account":to_hex(&ua)}).to_string().into())).await.unwrap();
    // place an order with wait=included, from an allowed country
    let t = now();
    let po = place_order(&g, &u, ua, t, 1, 672_505, 2_500, false);
    let (s, ack) = post_json(&format!("{}/v1/actions?wait=included", n.url), &request(&g, &po), &[("X-Mock-Country", "PA")]).await;
    assert_eq!(s, 200, "{ack}");
    assert_eq!(ack["status"], "included", "{ack}");
    assert_eq!(ack["result"]["ok"], true, "{ack}");
    // geo: BR may not open, may reduce, cancel and withdraw
    let po2 = place_order(&g, &u, ua, t + 1, 1, 672_505, 2_500, false);
    let (s, e) = post_json(&format!("{}/v1/actions", n.url), &request(&g, &po2), &[("X-Mock-Country", "BR")]).await;
    assert_eq!((s, e["error"]["code"].as_str()), (451, Some("GEO_RESTRICTED")));
    let (s, _) = post_json(&format!("{}/v1/actions", n.url), &request(&g, &po2), &[]).await;
    assert_eq!(s, 451, "unknown country is denied");
    let ro = place_order(&g, &u, ua, t + 2, 1, 672_505, 2_500, true);
    let (s, e) = post_json(&format!("{}/v1/actions", n.url), &request(&g, &ro), &[("X-Mock-Country", "BR")]).await;
    assert_eq!(s, 200, "{e}");
    let wd = withdraw(&g, &u, ua, t + 3, 1_000_000_000, ua);
    let (s, wack) = post_json(&format!("{}/v1/actions?wait=included", n.url), &request(&g, &wd), &[("X-Mock-Country", "BR")]).await;
    assert_eq!(s, 200, "{wack}");
    let wid = wack["result"]["withdrawalId"].as_str().unwrap().to_string();
    // the withdrawal becomes certified; the certificate verifies against /v1/committee alone
    let mut cert = serde_json::Value::Null;
    for _ in 0..60 {
        let (_, w) = get_json(&format!("{}/v1/withdrawals/{wid}", n.url)).await;
        if w["status"] == "certified" {
            cert = w["certificate"].clone();
            break;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    assert!(!cert.is_null(), "withdrawal not certified in time");
    verify_checkpoint_json(&g, &cert["checkpoint"]).unwrap();
    let (s, fin) = get_json(&format!("{}/v1/finality/latest", n.url)).await;
    assert_eq!(s, 200);
    verify_checkpoint_json(&g, &fin).unwrap();
    // malformed and oversize bodies
    let (s, e) = post_json(&format!("{}/v1/actions", n.url), &serde_json::json!({"type": "PlaceOrder"}), &[]).await;
    assert_eq!((s, e["error"]["code"].as_str()), (400, Some("MALFORMED")));
    let big = serde_json::json!({"x": "a".repeat(20_000)});
    let (s, e) = post_json(&format!("{}/v1/actions", n.url), &big, &[]).await;
    assert_eq!((s, e["error"]["code"].as_str()), (413, Some("TOO_LARGE")));
    // blocks are public and replayable: replay them with a fresh engine and compare every root
    let (_, bl) = get_json(&format!("{}/v1/blocks?from=1&limit=500&raw=1", n.url)).await;
    let mut e = StubEngine::new(g.clone());
    let blocks = bl["blocks"].as_array().unwrap();
    assert!(blocks.len() >= 3);
    for b in blocks {
        let raw = perps_node::util::parse_hex_vec(b["raw"].as_str().unwrap()).unwrap();
        let blk = perps_node::types::Block::from_bytes(&raw).unwrap();
        let inputs = perps_node::types::decode_inputs(&g.types, &blk.inputs_encoded).unwrap();
        e.apply_block(blk.header.height, blk.header.time, &inputs).unwrap();
        assert_eq!(e.state_root(), blk.header.state_root);
    }
    // websocket received a status update and an account update with the included actions
    let mut got_status = false;
    let mut got_account = false;
    let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
    while !(got_status && got_account) && tokio::time::Instant::now() < deadline {
        if let Ok(Some(Ok(m))) = tokio::time::timeout(Duration::from_millis(500), ws.next()).await {
            if let tokio_tungstenite::tungstenite::Message::Text(t) = m {
                let j: serde_json::Value = serde_json::from_str(&t).unwrap();
                if j["channel"] == "status" && j["type"] == "update" {
                    got_status = true;
                }
                if j["channel"] == "account" {
                    got_account = true;
                }
            }
        }
    }
    assert!(got_status && got_account);
    // WS action op goes through the same admission (geo included)
    let po3 = place_order(&g, &u, ua, t + 4, 1, 672_505, 2_500, false);
    ws.send(tokio_tungstenite::tungstenite::Message::Text(serde_json::json!({"op":"action","id":9,"request": request(&g, &po3)}).to_string().into())).await.unwrap();
    let mut answered = false;
    for _ in 0..50 {
        if let Ok(Some(Ok(tokio_tungstenite::tungstenite::Message::Text(t)))) = tokio::time::timeout(Duration::from_millis(200), ws.next()).await {
            let j: serde_json::Value = serde_json::from_str(&t).unwrap();
            if j["id"] == 9 {
                assert_eq!(j["op"], "error", "no country on the WS connection: denied (fail closed) {j}");
                assert_eq!(j["error"]["code"], "GEO_RESTRICTED");
                answered = true;
                break;
            }
        }
    }
    assert!(answered);
    let st = n.app.validators.lock().unwrap().clone();
    assert_eq!(st.len(), 7);
    assert!(st.values().all(|v| v.latched.is_none()));
    let _ = n.stop.send(true);
}

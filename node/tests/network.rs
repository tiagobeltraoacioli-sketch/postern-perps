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
    start_node_with(name, None).await
}

async fn start_node_with(name: &str, edge_secret: Option<Vec<u8>>) -> Running {
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
    let mut geo = geo;
    if let Some(sec) = edge_secret {
        geo.edge = Some(Arc::new(perps_node::geo::EdgeVerifier::new(sec).unwrap()));
    }
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
    // the certificate is the schema's WithdrawalCertificate: a whole Checkpoint (blockHash included), 32 siblings
    assert_eq!(cert["checkpoint"]["blockHash"].as_str().map(|h| h.len()), Some(66), "{}", cert["checkpoint"]);
    assert_eq!(cert["checkpoint"]["blockHash"], cert["checkpoint"]["logHash"]);
    assert_eq!(cert["proof"].as_array().map(|a| a.len()), Some(32));
    assert_eq!(cert.as_object().unwrap().keys().map(|k| k.as_str()).collect::<std::collections::BTreeSet<_>>(), ["checkpoint", "digest", "proof", "record", "signatures"].into_iter().collect());
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


fn assertion(secret: &[u8], country: &str, rid: u8, ts: u64) -> (String, String) {
    let header = format!("v1;c={country};s=-;tor=0;vpn=0;host=0;sur=api;ts={ts};rid={:032x}", rid as u128 + 1);
    let mac = postern_geoblock::assertion::mac_hex(secret, &header).unwrap();
    (header, mac)
}

async fn ws_action(ws: &mut tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>, id: u64, frame: serde_json::Value) -> serde_json::Value {
    ws.send(tokio_tungstenite::tungstenite::Message::Text(frame.to_string().into())).await.unwrap();
    for _ in 0..100 {
        if let Ok(Some(Ok(tokio_tungstenite::tungstenite::Message::Text(t)))) = tokio::time::timeout(Duration::from_millis(200), ws.next()).await {
            let j: serde_json::Value = serde_json::from_str(&t).unwrap();
            if j["id"] == id {
                return j;
            }
        }
    }
    panic!("no answer to {id}");
}

/// R395-36 (395) / 397 38ca9a4: with an edge verifier, a WebSocket action is admitted only with its OWN valid
/// per-action `edgeGeo` assertion (HMAC, 30 s, replay cache shared with REST); the upgrade request's headers never
/// count. Before this change the node took the WebSocket action's country from the upgrade request (a 30 s
/// connection-wide assertion, or the dev header), so the first action below was admitted.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn r395_36_ws_action_needs_its_own_edge_assertion() {
    let secret = vec![7u8; 32];
    let n = start_node_with("edge", Some(secret.clone())).await;
    let g = n.g.clone();
    let u = user(60);
    let ua = key_address(&u);
    let t = now();
    let ws_url = format!("{}/v1/ws?country=PA", n.url.replace("http://", "ws://"));
    let req = tokio_tungstenite::tungstenite::client::IntoClientRequest::into_client_request(ws_url.as_str()).unwrap();
    let mut req = req;
    // a valid assertion on the UPGRADE request must not admit frames
    let (uh, um) = assertion(&secret, "PA", 1, t);
    req.headers_mut().insert("x-postern-geo", uh.parse().unwrap());
    req.headers_mut().insert("x-postern-geo-mac", um.parse().unwrap());
    req.headers_mut().insert("x-mock-country", "PA".parse().unwrap());
    let (mut ws, _) = tokio_tungstenite::connect_async(req).await.unwrap();
    let r = ws_action(&mut ws, 1, serde_json::json!({"op":"action","id":1,"request": request(&g, &place_order(&g, &u, ua, t, 1, 10, 1, false))})).await;
    assert_eq!((r["op"].as_str(), r["error"]["code"].as_str()), (Some("error"), Some("GEO_RESTRICTED")), "{r}");
    assert_eq!(r["error"]["details"]["assertion"], "missing edgeGeo");
    // a bad MAC is refused
    let (h, _) = assertion(&secret, "PA", 2, t);
    let r = ws_action(&mut ws, 2, serde_json::json!({"op":"action","id":2,"request": request(&g, &place_order(&g, &u, ua, t + 1, 1, 10, 1, false)), "edgeGeo": {"header": h, "mac": "0".repeat(64)}})).await;
    assert_eq!(r["error"]["details"]["assertion"], "mac", "{r}");
    // a valid per-action assertion from an allowed country admits; edgeGeo is stripped before parsing
    let (h, m) = assertion(&secret, "PA", 3, now());
    let r = ws_action(&mut ws, 3, serde_json::json!({"op":"action","id":3,"request": request(&g, &place_order(&g, &u, ua, t + 2, 1, 10, 1, false)), "edgeGeo": {"header": h.clone(), "mac": m.clone()}})).await;
    assert_eq!(r["op"], "actionResult", "{r}");
    // the same assertion again (replay) is refused, also over REST: one cache for both
    let r = ws_action(&mut ws, 4, serde_json::json!({"op":"action","id":4,"request": request(&g, &place_order(&g, &u, ua, t + 3, 1, 10, 1, false)), "edgeGeo": {"header": h.clone(), "mac": m.clone()}})).await;
    assert_eq!(r["error"]["details"]["assertion"], "replay", "{r}");
    let (s, e) = post_json(&format!("{}/v1/actions", n.url), &request(&g, &place_order(&g, &u, ua, t + 4, 1, 10, 1, false)), &[("x-postern-geo", &h), ("x-postern-geo-mac", &m)]).await;
    assert_eq!((s, e["error"]["details"]["assertion"].as_str()), (451, Some("replay")));
    // a valid assertion from a blocked country: geo decision applies (exit-only)
    let (h, m) = assertion(&secret, "BR", 5, now());
    let r = ws_action(&mut ws, 5, serde_json::json!({"op":"action","id":5,"request": request(&g, &place_order(&g, &u, ua, t + 5, 1, 10, 1, false)), "edgeGeo": {"header": h, "mac": m}})).await;
    assert_eq!(r["error"]["code"], "GEO_RESTRICTED", "{r}");
    // a stale assertion (older than 30 s) is refused
    let (h, m) = assertion(&secret, "PA", 6, now() - 31_000);
    let r = ws_action(&mut ws, 6, serde_json::json!({"op":"action","id":6,"request": request(&g, &place_order(&g, &u, ua, t + 6, 1, 10, 1, false)), "edgeGeo": {"header": h, "mac": m}})).await;
    assert_eq!(r["error"]["details"]["assertion"], "stale", "{r}");
    // REST: the dev mock header no longer counts once an edge verifier is configured; a fresh header does
    let (s, e) = post_json(&format!("{}/v1/actions", n.url), &request(&g, &place_order(&g, &u, ua, t + 7, 1, 10, 1, false)), &[("X-Mock-Country", "PA")]).await;
    assert_eq!((s, e["error"]["details"]["assertion"].as_str()), (451, Some("missing")));
    let (h, m) = assertion(&secret, "PA", 7, now());
    let (s, e) = post_json(&format!("{}/v1/actions", n.url), &request(&g, &place_order(&g, &u, ua, t + 8, 1, 10, 1, false)), &[("x-postern-geo", &h), ("x-postern-geo-mac", &m)]).await;
    assert_eq!(s, 200, "{e}");
    let _ = n.stop.send(true);
}

// ------------------------------------------------------------------------------------------------ round 2 (431)

const ORACLE_TOKEN: &str = "test-oracle-ingest-token-0123456789";

struct RealRunning {
    url: String,
    app: Arc<App>,
    stop: tokio::sync::watch::Sender<bool>,
    g: Arc<perps_node::genesis::Genesis>,
    keys: perps_node::dev::DevKeys,
}

/// The REAL engine behind the HTTP API: producer, seven in-process validators, oracle ingest with a bearer token.
async fn start_real_node(name: &str) -> RealRunning {
    let (g, keys, checked) = perps_node::dev::dev_genesis_real(&format!("http-real-{name}"), 7, 5, 5, 0);
    let dir = tmpdir(name);
    let sv = view();
    let mk = || -> Box<dyn Engine> { Box::new(perps_node::engine::real::RealEngine::new(g.clone(), checked.clone()).unwrap()) };
    let seq = Sequencer::open(g.clone(), keys.sequencer.clone(), mk(), dir, sv.clone()).unwrap();
    let geo = GeoPolicy::from_json(
        &serde_json::json!({"version": "test", "unknown_country_effect": "deny",
            "tiers": {"allowed": {"effect": "allow"}, "blocked": {"effect": "deny"}},
            "countries": {"PA": {"tier": "allowed"}, "BR": {"tier": "blocked"}}, "denied_policy": {"policy": "exit_only"}}),
        true,
    )
    .unwrap();
    let app = App::with(g.clone(), seq, geo, Mode::Dev, sv.clone(), 50, false, [0x11; 20], Some(ORACLE_TOKEN.as_bytes().to_vec()));
    let (tx, rx) = tokio::sync::watch::channel(false);
    let views: Vec<_> = (0..7).map(|_| sv.clone()).collect();
    let engines: Vec<Box<dyn Engine>> = (0..7).map(|_| mk()).collect();
    spawn_local_validators(&app, keys.validators.clone(), engines, views, &rx).unwrap();
    tokio::spawn(run_producer(app.clone(), rx.clone()));
    let l = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = l.local_addr().unwrap();
    let r = perps_node::api::router(app.clone());
    tokio::spawn(async move { axum::serve(l, r.into_make_service_with_connect_info::<SocketAddr>()).await.unwrap() });
    RealRunning { url: format!("http://{addr}"), app, stop: tx, g, keys }
}

fn observation_json(g: &perps_node::genesis::Genesis, key: &k256::ecdsa::SigningKey, signer: u8, t: u64, prices: &[(u32, u64)]) -> serde_json::Value {
    let mut o = perps_node::types::SignedObservation {
        signer,
        time_ms: t,
        prices: prices.iter().map(|(m, p)| perps_node::types::OraclePrice { market_id: *m, price: *p, sources: 5 }).collect(),
        signature: [0u8; 65],
    };
    o.signature = perps_node::sig::sign(key, &perps_node::oracle_pool::observation_digest(g, t / 3000, &o));
    // 432's wire form: numbers for signer / timeMs / marketId / sources, a decimal string for the price
    serde_json::json!({"signer": signer, "timeMs": t, "prices": prices.iter().map(|(m, p)| serde_json::json!({"marketId": m, "price": p.to_string(), "sources": 5})).collect::<Vec<_>>(), "signature": to_hex(&o.signature)})
}

/// A stand-in for 432's feeder: every 3 s, the round of the wall clock signed by the seven throwaway oracle keys,
/// four through `/v1/oracle/updates` and three one by one through `/v1/oracle/observations`.
async fn feed(url: String, g: Arc<perps_node::genesis::Genesis>, keys: Vec<k256::ecdsa::SigningKey>, price: u64, mut stop: tokio::sync::watch::Receiver<bool>) {
    let auth = format!("Bearer {ORACLE_TOKEN}");
    let mut last_round = 0;
    loop {
        let t = now();
        if t / 3000 > last_round {
            last_round = t / 3000;
            let obs: Vec<serde_json::Value> = keys.iter().enumerate().map(|(i, k)| observation_json(&g, k, i as u8, t, &[(1, price)])).collect();
            let (s, j) = post_json(&format!("{url}/v1/oracle/updates"), &serde_json::json!({"round": last_round, "observations": &obs[..4]}), &[("Authorization", &auth)]).await;
            assert_eq!((s, j["held"].as_u64(), j["status"].as_str()), (200, Some(4), Some("held")), "{j}");
            for o in &obs[4..] {
                let (s, j) = post_json(&format!("{url}/v1/oracle/observations"), &serde_json::json!({"round": last_round, "observation": o}), &[("Authorization", &auth)]).await;
                assert_eq!(s, 200, "{j}");
            }
        }
        tokio::select! {
            _ = tokio::time::sleep(Duration::from_millis(200)) => {}
            _ = stop.changed() => return,
        }
    }
}

type Ws = tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>;

async fn ws_send(ws: &mut Ws, v: serde_json::Value) {
    ws.send(tokio_tungstenite::tungstenite::Message::Text(v.to_string().into())).await.unwrap();
}

/// Reads frames for `ms` milliseconds.
async fn ws_collect(ws: &mut Ws, ms: u64) -> Vec<serde_json::Value> {
    let mut out = Vec::new();
    let end = tokio::time::Instant::now() + Duration::from_millis(ms);
    while let Ok(Some(Ok(m))) = tokio::time::timeout_at(end, ws.next()).await {
        if let tokio_tungstenite::tungstenite::Message::Text(t) = m {
            out.push(serde_json::from_str(&t).unwrap());
        }
    }
    out
}

#[allow(clippy::too_many_arguments)]
fn order_req(g: &perps_node::genesis::Genesis, key: &k256::ecdsa::SigningKey, nonce: u64, is_buy: bool, tif: u8, price: u64, size: u64) -> serde_json::Value {
    let a = signed(
        g,
        key,
        "PlaceOrder",
        serde_json::json!({"account": to_hex(&key_address(key)), "nonce": nonce.to_string(), "marketId": 1, "isBuy": is_buy, "orderType": 0,
            "timeInForce": tif, "price": price.to_string(), "size": size.to_string(), "reduceOnly": false, "triggerPrice": "0",
            "clientOrderId": to_hex(&[0u8; 32])}),
    );
    request(g, &a)
}

/// Round 2: the oracle ingest path (bearer token, pre-filter) and the whole market-data surface over HTTP and
/// WebSocket, served from the real engine: ingest -> active market -> trade -> trades / candles / ticker /
/// positions / fills / orders / proofs, and the WebSocket channels deliver the block's changes.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn real_engine_oracle_ingest_market_data_and_websocket_channels() {
    const P: u64 = 6_725_000_000_000;
    let n = start_real_node("md").await;
    let g = n.g.clone();
    let base = n.url.clone();
    // ingest authentication: no token, a wrong token -> 401 and nothing is held
    let t = now();
    let o = observation_json(&g, &n.keys.oracle[0], 0, t, &[(1, P)]);
    let body = serde_json::json!({"round": t / 3000, "observation": o});
    let (s, e) = post_json(&format!("{base}/v1/oracle/observations"), &body, &[]).await;
    assert_eq!((s, e["error"]["code"].as_str()), (401, Some("UNAUTHORIZED")));
    let (s, _) = post_json(&format!("{base}/v1/oracle/observations"), &body, &[("Authorization", "Bearer wrong-token-wrong-token-wrong")]).await;
    assert_eq!(s, 401);
    let (_, rounds) = get_json(&format!("{base}/v1/oracle/rounds/latest")).await;
    assert_eq!((rounds["enabled"].as_bool(), rounds["accepted"].as_u64(), rounds["held"].as_array().map(|a| a.len())), (Some(true), Some(0), Some(0)), "{rounds}");
    // an observation signed by a key outside the committee is refused by the pre-filter, with the right token
    let auth = format!("Bearer {ORACLE_TOKEN}");
    let forged = observation_json(&g, &user(5), 0, t, &[(1, P)]);
    let (s, e) = post_json(&format!("{base}/v1/oracle/observations"), &serde_json::json!({"round": t / 3000, "observation": forged}), &[("Authorization", &auth)]).await;
    assert_eq!((s, e["error"]["code"].as_str()), (400, Some("ORACLE_BAD_SIGNATURE")), "{e}");
    let (s, e) = post_json(&format!("{base}/v1/oracle/observations"), &serde_json::json!({"round": t / 3000, "observation": o, "extra": 1}), &[("Authorization", &auth)]).await;
    assert_eq!((s, e["error"]["code"].as_str()), (400, Some("ORACLE_MALFORMED")), "{e}");

    // the feeder stand-in runs; two funded accounts
    let (a, b) = (user(61), user(62));
    let (aa, ba) = (key_address(&a), key_address(&b));
    {
        let mut v = n.app.view.lock().unwrap();
        v.deposits.insert(0, deposit(0, aa, 100_000_000_000));
        v.deposits.insert(1, deposit(1, ba, 1_000_000_000));
    }
    let (fstop_tx, fstop_rx) = tokio::sync::watch::channel(false);
    let feeder = tokio::spawn(feed(base.clone(), g.clone(), n.keys.oracle.clone(), P, fstop_rx));
    // three agreeing rounds (confirmRounds) make the market active: at most ~12 s of wall clock
    let mut active = false;
    for _ in 0..160 {
        let (_, m) = get_json(&format!("{base}/v1/markets/BTC-USD")).await;
        if m["status"] == "active" {
            active = true;
            break;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    assert!(active, "BTC-USD did not become active from the ingested rounds");
    let (s, os) = get_json(&format!("{base}/v1/oracle/BTC-USD")).await;
    assert_eq!((s, os["status"].as_str(), os["price"].as_str(), os["threshold"].as_u64(), os["haltReason"].is_null()), (200, Some("ok"), Some("67250"), Some(5), true), "{os}");
    let (_, rounds) = get_json(&format!("{base}/v1/oracle/rounds/latest")).await;
    assert_eq!(rounds["lastBuilt"]["signers"], serde_json::json!([0, 1, 2, 3, 4, 5, 6]), "every observation of the round went into the block: {rounds}");
    assert_eq!(rounds["conflicts"], 0);

    // WebSocket subscriptions before the trade
    let ws_url = format!("{}/v1/ws", base.replace("http://", "ws://"));
    let (mut ws, _) = tokio_tungstenite::connect_async(&ws_url).await.unwrap();
    ws_send(&mut ws, serde_json::json!({"op":"subscribe","id":1,"channel":"trades","symbol":"BTC-USD"})).await;
    ws_send(&mut ws, serde_json::json!({"op":"subscribe","id":2,"channel":"candles","symbol":"BTC-USD","interval":"1m"})).await;
    ws_send(&mut ws, serde_json::json!({"op":"subscribe","id":3,"channel":"ticker","symbol":"*"})).await;
    ws_send(&mut ws, serde_json::json!({"op":"subscribe","id":4,"channel":"account","account":to_hex(&ba)})).await;
    ws_send(&mut ws, serde_json::json!({"op":"subscribe","id":5,"channel":"candles","symbol":"BTC-USD","interval":"7m"})).await;
    let first = ws_collect(&mut ws, 400).await;
    let has = |v: &[serde_json::Value], f: &dyn Fn(&serde_json::Value) -> bool| v.iter().any(f);
    assert!(has(&first, &|m| m["channel"] == "trades" && m["type"] == "snapshot" && m["data"]["trades"].as_array().is_some_and(|a| a.is_empty())), "{first:?}");
    assert!(has(&first, &|m| m["channel"] == "candles" && m["type"] == "snapshot" && m["interval"] == "1m"), "{first:?}");
    assert_eq!(first.iter().filter(|m| m["channel"] == "ticker" && m["type"] == "update").count(), 3, "one ticker per market for \"*\": {first:?}");
    assert!(has(&first, &|m| m["channel"] == "account" && m["type"] == "snapshot" && m["data"]["account"]["collateral"] == "1000.000000"), "{first:?}");
    assert!(has(&first, &|m| m["op"] == "error" && m["id"] == 5 && m["error"]["code"] == "MALFORMED"), "unknown interval refused: {first:?}");

    // the trade: A rests an ask, B takes 0.14 with an IOC
    let t = now();
    let (s, ack) = post_json(&format!("{base}/v1/actions?wait=included"), &order_req(&g, &a, t, false, 0, P, 50_000_000), &[("X-Mock-Country", "PA")]).await;
    assert_eq!((s, ack["result"]["ok"].as_bool(), ack["result"]["orderStatus"].as_str()), (200, Some(true), Some("open")), "{ack}");
    let maker_order = ack["result"]["orderId"].as_str().unwrap().to_string();
    let (s, ack) = post_json(&format!("{base}/v1/actions?wait=included"), &order_req(&g, &b, t + 1, true, 1, P, 14_000_000), &[("X-Mock-Country", "PA")]).await;
    assert_eq!((s, ack["result"]["ok"].as_bool(), ack["result"]["orderStatus"].as_str(), ack["result"]["filledSize"].as_str()), (200, Some(true), Some("filled"), Some("0.1400")), "{ack}");

    // REST market data
    let (_, tr) = get_json(&format!("{base}/v1/markets/BTC-USD/trades")).await;
    assert_eq!(tr["trades"].as_array().unwrap().len(), 1, "{tr}");
    assert_eq!((tr["trades"][0]["price"].as_str(), tr["trades"][0]["size"].as_str(), tr["trades"][0]["side"].as_str(), tr.get("nextCursor")), (Some("67250.0"), Some("0.1400"), Some("buy"), None));
    for iv in ["1m", "5m", "15m", "1h", "4h", "1d"] {
        let (s, c) = get_json(&format!("{base}/v1/markets/BTC-USD/candles?interval={iv}")).await;
        assert_eq!((s, c["candles"].as_array().unwrap().len(), c["candles"][0]["n"].as_u64(), c["candles"][0]["c"].as_str(), c["candles"][0]["q"].as_str()), (200, 1, Some(1), Some("67250.0"), Some("9415.000000")), "{iv}: {c}");
    }
    let (s, e) = get_json(&format!("{base}/v1/markets/BTC-USD/candles?interval=2m")).await;
    assert_eq!((s, e["error"]["code"].as_str()), (400, Some("MALFORMED")));
    let (s, e) = get_json(&format!("{base}/v1/markets/BTC-USD/trades?limit=0")).await;
    assert_eq!((s, e["error"]["code"].as_str()), (400, Some("MALFORMED")));
    let (s, e) = get_json(&format!("{base}/v1/markets/BTC-USD/trades?before=abc")).await;
    assert_eq!((s, e["error"]["code"].as_str()), (400, Some("MALFORMED")));
    let (s, _) = get_json(&format!("{base}/v1/markets/XYZ-USD/trades")).await;
    assert_eq!(s, 404);
    let (_, tk) = get_json(&format!("{base}/v1/markets/BTC-USD/ticker")).await;
    assert_eq!((tk["lastPrice"].as_str(), tk["openInterest"].as_str(), tk["volume24h"].as_str(), tk["indexPrice"].as_str()), (Some("67250"), Some("0.14"), Some("9415.000000"), Some("67250")), "{tk}");
    let (_, fu) = get_json(&format!("{base}/v1/markets/BTC-USD/funding")).await;
    assert_eq!(fu, serde_json::json!({"funding": []}), "no settlement yet");

    // REST account data
    let (_, acc) = get_json(&format!("{base}/v1/accounts/{}", to_hex(&ba))).await;
    assert_eq!((acc["collateral"].as_str(), acc["positions"][0]["size"].as_str(), acc["positions"][0]["entryPrice"].as_str(), acc["initialMargin"].as_str()), (Some("995.763250"), Some("0.14"), Some("67250"), Some("941.500000")), "{acc}");
    assert!(acc.get("engine").is_none(), "exactly the schema's Account fields");
    let (_, fills) = get_json(&format!("{base}/v1/accounts/{}/fills?symbol=BTC-USD", to_hex(&ba))).await;
    assert_eq!((fills["fills"].as_array().unwrap().len(), fills["fills"][0]["fee"].as_str(), fills["fills"][0]["liquidity"].as_str()), (1, Some("4.236750"), Some("taker")), "{fills}");
    let (_, none) = get_json(&format!("{base}/v1/accounts/{}/fills?symbol=ETH-USD", to_hex(&ba))).await;
    assert_eq!(none["fills"].as_array().unwrap().len(), 0, "symbol filter");
    let (_, oo) = get_json(&format!("{base}/v1/accounts/{}/orders", to_hex(&aa))).await;
    assert_eq!((oo["orders"].as_array().unwrap().len(), oo["orders"][0]["orderId"].as_str(), oo["orders"][0]["status"].as_str(), oo["orders"][0]["filledSize"].as_str()), (1, Some(maker_order.as_str()), Some("open"), Some("0.1400")), "{oo}");
    let (_, all) = get_json(&format!("{base}/v1/accounts/{}/orders?status=all", to_hex(&ba))).await;
    assert_eq!(all["orders"][0]["status"], "filled");
    let (s, e) = get_json(&format!("{base}/v1/accounts/{}/orders?status=closed", to_hex(&ba))).await;
    assert_eq!((s, e["error"]["code"].as_str()), (400, Some("MALFORMED")));
    let (s, one) = get_json(&format!("{base}/v1/orders/{maker_order}")).await;
    assert_eq!((s, one["account"].as_str()), (200, Some(to_hex(&aa).as_str())));
    let (_, pays) = get_json(&format!("{base}/v1/accounts/{}/funding", to_hex(&ba))).await;
    assert_eq!(pays, serde_json::json!({"payments": []}));
    let (_, deps) = get_json(&format!("{base}/v1/accounts/{}/deposits", to_hex(&ba))).await;
    let d0 = &deps["deposits"][0];
    assert_eq!((d0["depositId"].as_str(), d0["amount"].as_str(), d0["status"].as_str()), (Some("1"), Some("1000.000000"), Some("credited")), "{deps}");
    assert_eq!(d0.as_object().unwrap().len(), 9, "exactly the schema's Deposit fields: {d0}");
    let (_, st) = get_json(&format!("{base}/v1/status")).await;
    assert_eq!(st["network"]["collateralToken"], to_hex(&[0x11; 20]));
    let (_, ni) = get_json(&format!("{base}/v1/node")).await;
    assert_eq!((ni["index"]["mismatches"].as_u64(), ni["oracleIngest"]["enabled"].as_bool()), (Some(0), Some(true)), "{ni}");

    // R435-1: the published genesis document is what logHash(0) commits to; with /v1/blocks/stream it replays
    let (st, doc) = http_call("GET", &format!("{base}/v1/genesis"), None, &[]).await.unwrap();
    assert_eq!(st, 200);
    assert_eq!(to_hex(&perps_node::util::keccak_parts(&[b"PosternPerps/genesis/v1", &doc])), ni["genesisLogHash"].as_str().unwrap(), "keccak(prefix || document) = logHash(0)");
    let parsed = postern_perps_engine::genesis::Genesis::from_json(std::str::from_utf8(&doc).unwrap()).expect("the engine CLI's own parser reads it");
    assert_eq!(parsed.g.markets.len(), 3);
    // CORS preflight is answered by the node (API.md section 1). Restated for F434-2: the allowlist is empty by
    // default (decided by the coordinator), so the origin is listed first; the old assertion (204) is kept, and
    // the full rules are in f434_2_cors_is_an_exact_allowlist_with_preflight_and_a_websocket_origin_check.
    *n.app.cors_origins.lock().unwrap() = vec!["http://127.0.0.1:49920".to_string()];
    let (st, _) = http_call("OPTIONS", &format!("{base}/v1/actions"), None, &[("Origin", "http://127.0.0.1:49920"), ("Access-Control-Request-Method", "POST")]).await.unwrap();
    assert_eq!(st, 204);
    // F434-6 / F434-5: the bands in `Market` are the engine's own MarketParams (the genesis document just parsed)
    let (_, ml) = get_json(&format!("{base}/v1/markets")).await;
    for p in &parsed.g.markets {
        let m = ml["markets"].as_array().unwrap().iter().find(|m| m["symbol"] == p.symbol.as_str()).expect("market");
        assert_eq!((m["marketSlippageBps"].as_u64(), m["markBandBps"].as_u64()), (Some(p.market_slippage_bps as u64), Some(p.mark_band_bps as u64)), "{m}");
        let o = &m["oracle"];
        assert_eq!(
            (o["exitHaircutBps"].as_u64(), o["exitHaircutPerMinBps"].as_u64(), o["exitMaxHaircutBps"].as_u64(), o["maxHaltMs"].as_u64()),
            (Some(p.oracle.exit_haircut_bps as u64), Some(p.oracle.exit_haircut_per_min_bps as u64), Some(p.oracle.exit_max_haircut_bps as u64), Some(p.oracle.max_halt_ms)),
            "{m}"
        );
        assert!(p.oracle.exit_haircut_bps > 0 && p.oracle.max_halt_ms > 0, "the comparison is not of zeros");
        let (_, one) = get_json(&format!("{base}/v1/markets/{}", p.symbol)).await;
        assert_eq!(&one, m);
    }
    // an ACTIVE market has no exit band and no halt time
    let (_, os) = get_json(&format!("{base}/v1/oracle/BTC-USD")).await;
    assert_eq!((os["status"].as_str(), os["haltedSince"].is_null(), os["exitBand"].is_null()), (Some("ok"), true, true), "{os}");
    assert!(os.as_object().unwrap().contains_key("haltedSince") && os.as_object().unwrap().contains_key("exitBand"));
    assert_eq!(acc["positions"][0]["openNotional"], "94150000000000000000", "0.14 BTC x 67,250 in 1e-16 USD");

    // the account proof against the latest final checkpoint (the validators sign in real time)
    let mut proof = serde_json::Value::Null;
    for _ in 0..80 {
        let (s, p) = get_json(&format!("{base}/v1/accounts/{}/proof", to_hex(&ba))).await;
        if s == 200 {
            proof = p;
            break;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    assert_eq!(proof["proof"].as_array().map(|a| a.len()), Some(32), "{proof}");
    let body = verify_checkpoint_json(&g, &proof["checkpoint"]).unwrap();
    let leaf = perps_node::util::parse_hex_fixed::<32>(proof["leaf"].as_str().unwrap()).unwrap();
    let sib: Vec<[u8; 32]> = proof["proof"].as_array().unwrap().iter().map(|p| perps_node::util::parse_hex_fixed::<32>(p.as_str().unwrap()).unwrap()).collect();
    assert!(postern_perps_engine::merkle::verify(&leaf, proof["index"].as_str().unwrap().parse().unwrap(), &sib, &body.accounts_root));
    let (s, e) = get_json(&format!("{base}/v1/accounts/{}/proof?height=999999", to_hex(&ba))).await;
    assert_eq!((s, e["error"]["code"].as_str()), (404, Some("NOT_FOUND")));
    let (s, fin) = get_json(&format!("{base}/v1/accounts/{}?finalized=true", to_hex(&ba))).await;
    assert_eq!((s, fin["address"].as_str()), (200, Some(to_hex(&ba).as_str())));

    // the WebSocket delivered the block's changes
    let msgs = ws_collect(&mut ws, 1500).await;
    assert!(has(&msgs, &|m| m["channel"] == "trades" && m["type"] == "update" && m["data"]["trades"][0]["size"] == "0.1400"), "{msgs:?}");
    assert!(has(&msgs, &|m| m["channel"] == "candles" && m["type"] == "update" && m["interval"] == "1m" && m["data"]["n"] == 1), "{msgs:?}");
    assert!(has(&msgs, &|m| m["channel"] == "ticker" && m["symbol"] == "BTC-USD" && m["data"]["lastPrice"] == "67250"), "{msgs:?}");
    assert!(
        has(&msgs, &|m| m["channel"] == "account" && m["type"] == "update" && m["data"]["fills"][0]["fee"] == "4.236750" && m["data"]["orders"][0]["status"] == "filled"
            && m["data"]["account"]["positions"][0]["size"] == "0.14" && m["data"]["actions"][0]["status"] == "included"),
        "{msgs:?}"
    );
    let _ = fstop_tx.send(true);
    let _ = feeder.await;
    let _ = n.stop.send(true);
}

/// A node without `--oracle-token-file` has no ingest path at all (503), whatever the caller presents.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn oracle_ingest_is_disabled_without_a_configured_token() {
    let n = start_node("no-ingest").await;
    for path in ["observations", "updates"] {
        let (s, e) = post_json(&format!("{}/v1/oracle/{path}", n.url), &serde_json::json!({"round": 1}), &[("Authorization", "Bearer anything-at-all-0123456789")]).await;
        assert_eq!((s, e["error"]["code"].as_str()), (503, Some("ORACLE_INGEST_DISABLED")), "{path}: {e}");
    }
    let _ = n.stop.send(true);
}

/// R436-6: REST reads are rate-limited per IP (API.md section 8); writes keep their own limits.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn r436_6_rest_reads_are_rate_limited_per_ip() {
    let n = start_node("read-limit").await;
    *n.app.read_rate.lock().unwrap() = perps_node::sequencer::RateLimit::new(1.0, 3.0);
    let mut codes = Vec::new();
    for _ in 0..5 {
        codes.push(get_json(&format!("{}/v1/status", n.url)).await.0);
    }
    assert_eq!(codes, vec![200, 200, 200, 429, 429], "a burst of 3, then refused");
    let (s, e) = get_json(&format!("{}/v1/markets", n.url)).await;
    assert_eq!((s, e["error"]["code"].as_str()), (429, Some("RATE_LIMITED")));
    // a write is not a read: it reaches admission (and is refused there for its own reason)
    let (s, e) = post_json(&format!("{}/v1/actions", n.url), &serde_json::json!({"type": "PlaceOrder"}), &[]).await;
    assert_eq!((s, e["error"]["code"].as_str()), (400, Some("MALFORMED")));
    tokio::time::sleep(Duration::from_millis(1100)).await;
    assert_eq!(get_json(&format!("{}/v1/status", n.url)).await.0, 200, "the bucket refills");
    let _ = n.stop.send(true);
}

/// R436-11 (sequencer side): an account proof against an OLD final checkpoint, whose engine state this node no
/// longer holds, is rebuilt from the log (deterministic replay, checked against the checkpoint's state root)
/// instead of answering 404; a height that is not a final checkpoint still has no proof.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn r436_11_account_proof_at_an_old_final_checkpoint_is_rebuilt_from_the_log() {
    let n = start_real_node("rebuild").await;
    let g = n.g.clone();
    let base = n.url.clone();
    let users: Vec<_> = (0..4).map(|i| user(300 + i)).collect();
    // unfunded actions make blocks; a checkpoint every 5 blocks: more than FRAMES_FINAL_KEEP (8) final ones
    let mut height = 0;
    let mut i = 0u64;
    while height < 62 {
        let u = &users[(i % 4) as usize];
        i += 1;
        let a = signed(&g, u, "CancelAll", serde_json::json!({"account": to_hex(&key_address(u)), "nonce": (now() + i).to_string(), "marketId": 0}));
        let (s, ack) = post_json(&format!("{base}/v1/actions?wait=included"), &request(&g, &a), &[("X-Mock-Country", "PA")]).await;
        assert_eq!(s, 200, "{ack}");
        height = ack["height"].as_u64().unwrap_or(height);
    }
    let mut fin = serde_json::Value::Null;
    for _ in 0..80 {
        let (s, f) = get_json(&format!("{base}/v1/finality/latest")).await;
        if s == 200 && f["height"].as_u64() >= Some(60) {
            fin = f;
            break;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    assert!(fin["height"].as_u64() >= Some(60), "{fin}");
    let (_, ni) = get_json(&format!("{base}/v1/node")).await;
    let held: Vec<u64> = ni["proofFrames"]["held"].as_array().unwrap().iter().map(|x| x.as_u64().unwrap()).collect();
    assert!(!held.contains(&5) && !held.contains(&10), "the first checkpoints' states were dropped: {held:?}");
    assert!(n.app.seq.lock().unwrap().final_without_frame(10));
    // the proof at height 10 is rebuilt from the log and verifies against THAT checkpoint
    let insurance = "0x0000000000000000000000000000000000000002";
    let (s, p) = get_json(&format!("{base}/v1/accounts/{insurance}/proof?height=10")).await;
    assert_eq!(s, 200, "{p}");
    let body = verify_checkpoint_json(&g, &p["checkpoint"]).unwrap();
    assert_eq!(body.height, 10);
    let leaf = perps_node::util::parse_hex_fixed::<32>(p["leaf"].as_str().unwrap()).unwrap();
    let sib: Vec<[u8; 32]> = p["proof"].as_array().unwrap().iter().map(|x| perps_node::util::parse_hex_fixed::<32>(x.as_str().unwrap()).unwrap()).collect();
    assert!(postern_perps_engine::merkle::verify(&leaf, 1, &sib, &body.accounts_root));
    let (_, ni) = get_json(&format!("{base}/v1/node")).await;
    assert!(ni["proofFrames"]["held"].as_array().unwrap().iter().any(|x| x == 10), "the rebuilt state is kept: {ni}");
    // a second request is served from the kept state; a height without a final checkpoint has no proof
    let (s, _) = get_json(&format!("{base}/v1/accounts/{insurance}/proof?height=10")).await;
    assert_eq!(s, 200);
    let (s, _) = get_json(&format!("{base}/v1/accounts/{insurance}/proof?height=7")).await;
    assert_eq!(s, 404, "7 is not a checkpoint height");
    let _ = n.stop.send(true);
}

/// The headers with which an account reads its own attestation record (R436-17).
/// (time, nonce, signature); every call takes a new nonce (R436-17 residue: one signature, one read).
fn read_auth(g: &perps_node::genesis::Genesis, key: &k256::ecdsa::SigningKey, t: u64) -> (String, String, String) {
    static NONCE: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);
    let n = NONCE.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    (t.to_string(), n.to_string(), to_hex(&perps_node::sig::sign(key, &perps_node::attest::read_digest(g, &key_address(key), t, n))))
}

async fn get_own_attestation(base: &str, g: &perps_node::genesis::Genesis, key: &k256::ecdsa::SigningKey) -> (u16, serde_json::Value) {
    let (t, nonce, sig) = read_auth(g, key, now());
    let (s, b) = http_call("GET", &format!("{base}/v1/attestations/{}", to_hex(&key_address(key))), None, &[("X-Postern-Read-Time", &t), ("X-Postern-Read-Nonce", &nonce), ("X-Postern-Read-Signature", &sig)]).await.unwrap();
    (s, serde_json::from_slice(&b).unwrap_or(serde_json::Value::Null))
}

fn attestation(g: &perps_node::genesis::Genesis, key: &k256::ecdsa::SigningKey, statements: &std::collections::BTreeMap<String, String>, residence: &str, issued: u64, expires: u64) -> serde_json::Value {
    let (id, hash) = statements.iter().next().unwrap();
    let msg = serde_json::json!({"account": to_hex(&key_address(key)), "statementId": id, "statementHash": hash, "residenceCountry": residence,
        "issuedAt": issued.to_string(), "expiresAt": expires.to_string()});
    let m = postern_geoblock::attestation::parse_message(&msg).unwrap();
    let d = postern_geoblock::attestation::digest(&m, &g.vault);
    serde_json::json!({"message": msg, "signature": to_hex(&perps_node::sig::sign(key, &d))})
}

/// Attestation records (397's design; 434's ask): the signed eligibility record is verified with 397's Rust port,
/// stored, served, and, with the requirement on, an action that opens exposure needs a valid one; a declared
/// residence the list denies makes the account exit-only whatever its IP; exits never need one.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn attestation_records_are_verified_stored_and_enforced_when_required() {
    let n = start_node("attest").await;
    let g = n.g.clone();
    let base = n.url.clone();
    let notices: serde_json::Value = serde_json::from_slice(&std::fs::read(std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../compliance/legal/notices.json")).unwrap()).unwrap();
    let statements = perps_node::attest::statements_from_notices(&notices).expect("the statement hash is the hash of its text");
    let file = tmpdir("attest-file").join("attestations.jsonl");
    let (u, w) = (user(80), user(81));
    let (ua, wa) = (key_address(&u), key_address(&w));
    {
        let mut v = n.app.view.lock().unwrap();
        v.deposits.insert(0, deposit(0, ua, 10_000_000_000));
        v.deposits.insert(1, deposit(1, wa, 10_000_000_000));
    }
    let pa = [("X-Mock-Country", "PA")];
    let t = now();
    // no statement configured: the endpoint says so
    let rec = attestation(&g, &u, &statements, "PA", t - 1000, t + 86_400_000);
    let (s, e) = post_json(&format!("{base}/v1/attestations"), &rec, &[]).await;
    assert_eq!((s, e["error"]["code"].as_str()), (503, Some("ATTESTATION_UNAVAILABLE")));
    *n.app.attestations.lock().unwrap() = perps_node::attest::Attestations::new(statements.clone(), &g.vault, Some(file.clone()), t);
    // (reads are the account's own since R436-17: the same assertions, with the account's signature)
    let (s, st) = get_own_attestation(&base, &g, &u).await;
    assert_eq!((s, st["status"].as_str(), st["record"].is_null()), (200, Some("missing"), true));

    // refused records: another signer, an altered message, an outdated statement, too long a validity, extra fields
    let by_other = serde_json::json!({"message": rec["message"], "signature": attestation(&g, &w, &statements, "PA", t - 1000, t + 86_400_000)["signature"]});
    let mut altered = rec.clone();
    altered["message"]["residenceCountry"] = "CH".into();
    let old: std::collections::BTreeMap<String, String> = [("attest-0.0-old".to_string(), to_hex(&[7u8; 32]))].into_iter().collect();
    let outdated = attestation(&g, &u, &old, "PA", t - 1000, t + 86_400_000);
    let too_long = attestation(&g, &u, &statements, "PA", t - 1000, t + 401 * 86_400_000);
    let mut extra = rec.clone();
    extra["note"] = "x".into();
    for (name, bad) in [("another signer", &by_other), ("altered", &altered), ("outdated statement", &outdated), ("validity above 400 days", &too_long), ("extra field", &extra)] {
        let (s, e) = post_json(&format!("{base}/v1/attestations"), bad, &[]).await;
        assert_eq!((s, e["error"]["code"].as_str()), (400, Some("ATTESTATION_INVALID")), "{name}: {e}");
    }
    assert_eq!(n.app.attestations.lock().unwrap().len(), 0);

    // requirement ON: opening needs a valid attestation; exits do not
    n.app.attestations.lock().unwrap().required = true;
    tokio::time::sleep(Duration::from_millis(300)).await;
    let (s, e) = post_json(&format!("{base}/v1/actions"), &request(&g, &place_order(&g, &u, ua, t + 1, 1, 672_505, 2_500, false)), &pa).await;
    assert_eq!((s, e["error"]["code"].as_str(), e["error"]["details"]["status"].as_str()), (403, Some("ATTESTATION_REQUIRED"), Some("missing")), "{e}");
    let (s, e) = post_json(&format!("{base}/v1/actions"), &request(&g, &withdraw(&g, &u, ua, t + 2, 1_000_000_000, ua)), &pa).await;
    assert_eq!(s, 200, "a withdrawal never needs an attestation: {e}");
    let (s, e) = post_json(&format!("{base}/v1/actions"), &request(&g, &place_order(&g, &u, ua, t + 3, 1, 672_505, 2_500, true)), &pa).await;
    assert_eq!(s, 200, "a reduce-only order never needs one: {e}");
    // the valid record: stored, served, and the same order is admitted
    let (s, st) = post_json(&format!("{base}/v1/attestations"), &rec, &[]).await;
    assert_eq!((s, st["status"].as_str(), st["residenceCountry"].as_str(), st["account"].as_str()), (200, Some("valid"), Some("PA"), Some(to_hex(&ua).as_str())), "{st}");
    let (_, st) = get_own_attestation(&base, &g, &u).await;
    assert_eq!((st["status"].as_str(), st["record"]["signature"].as_str()), (Some("valid"), rec["signature"].as_str()), "the record is served to its account, re-verifiable");
    let (s, e) = post_json(&format!("{base}/v1/actions"), &request(&g, &place_order(&g, &u, ua, t + 4, 1, 672_505, 2_500, false)), &pa).await;
    assert_eq!(s, 200, "{e}");
    // an older record does not replace a newer one
    let older = attestation(&g, &u, &statements, "PA", t - 5000, t + 86_400_000);
    let (s, _) = post_json(&format!("{base}/v1/attestations"), &older, &[]).await;
    assert_eq!(s, 400);

    // decision 3: a valid attestation declaring a residence the list denies (BR) makes the account exit-only,
    // although its IP country (PA) is allowed, and with the requirement off as well
    let (s, st) = post_json(&format!("{base}/v1/attestations"), &attestation(&g, &w, &statements, "BR", t - 1000, t + 86_400_000), &[]).await;
    assert_eq!((s, st["status"].as_str()), (200, Some("valid")));
    // R435-5: every action of this signer gets its own nonce from ONE base and a fixed offset per iteration. The
    // first version took `now() + 10 + required` in each iteration: when the two iterations ran 1 ms apart (the
    // usual case in a release build) both withdrawals had the same nonce, i.e. were the SAME signed action, and the
    // node rightly refused the second as a replay (NONCE_USED): 14 failures of 20 release runs. The node was
    // right; the test was wrong. The replay refusal is now asserted on purpose below.
    let base_nonce = now();
    let mut last_withdrawal = serde_json::Value::Null;
    for (k, required) in [true, false].into_iter().enumerate() {
        n.app.attestations.lock().unwrap().required = required;
        let nonce = base_nonce + 10 + 2 * k as u64;
        let (s, e) = post_json(&format!("{base}/v1/actions"), &request(&g, &place_order(&g, &w, wa, nonce, 1, 672_505, 2_500, false)), &pa).await;
        assert_eq!((s, e["error"]["code"].as_str(), e["error"]["details"]["reason"].as_str()), (451, Some("GEO_RESTRICTED"), Some("RESIDENCE_DECLARED_RESTRICTED")), "required={required}: {e}");
        last_withdrawal = request(&g, &withdraw(&g, &w, wa, nonce + 1, 1_000_000_000, wa));
        let (s, e) = post_json(&format!("{base}/v1/actions"), &last_withdrawal, &pa).await;
        assert_eq!(s, 200, "the exit stays open: {e}");
    }
    // the same signed withdrawal again is a replay: refused, whatever the attestation state
    let (s, e) = post_json(&format!("{base}/v1/actions"), &last_withdrawal, &pa).await;
    assert_eq!((s, e["error"]["code"].as_str()), (400, Some("NONCE_USED")), "{e}");
    // an expired record is `expired`, and with the requirement on it does not admit
    let short = attestation(&g, &user(82), &statements, "PA", now() - 1000, now() + 400);
    let (s, _) = post_json(&format!("{base}/v1/attestations"), &short, &[]).await;
    assert_eq!(s, 200);
    tokio::time::sleep(Duration::from_millis(600)).await;
    let (_, st) = get_own_attestation(&base, &g, &user(82)).await;
    assert_eq!(st["status"], "expired");
    // records survive a restart of the store (re-verified at load); a tampered line is not loaded
    let reloaded = perps_node::attest::Attestations::new(statements.clone(), &g.vault, Some(file.clone()), now());
    assert_eq!(reloaded.len(), 3);
    assert_eq!(reloaded.status(&ua, now()).status, "valid");
    let other_vault = perps_node::attest::Attestations::new(statements.clone(), &[0x99; 20], Some(file.clone()), now());
    assert_eq!(other_vault.len(), 0, "records signed for another deployment do not verify");
    let _ = n.stop.send(true);
}

/// R436-17 (Low, privacy; the legal side is for counsel): anyone could read any address's declared residence and
/// signed attestation, and anyone could make the store grow without bound. Properties: a read needs the account's
/// own signature or the operator's token (fail closed: 401, and not even the status leaks); posting the same
/// record again stores nothing; a replacement must be issued at least a minute after the record it replaces;
/// records of accounts the engine does not know are bounded.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn r436_17_attestation_records_are_read_only_by_the_account_or_the_operator() {
    let n = start_node("attest-read").await;
    let g = n.g.clone();
    let base = n.url.clone();
    let notices: serde_json::Value = serde_json::from_slice(&std::fs::read(std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../compliance/legal/notices.json")).unwrap()).unwrap();
    let statements = perps_node::attest::statements_from_notices(&notices).unwrap();
    let file = tmpdir("attest-read-file").join("attestations.jsonl");
    *n.app.attestations.lock().unwrap() = perps_node::attest::Attestations::new(statements.clone(), &g.vault, Some(file.clone()), now());
    let u = user(85);
    let ua = key_address(&u);
    let t = now();
    let rec = attestation(&g, &u, &statements, "PA", t - 1000, t + 86_400_000);
    let (s, _) = post_json(&format!("{base}/v1/attestations"), &rec, &[]).await;
    assert_eq!(s, 200);
    let url = format!("{base}/v1/attestations/{}", to_hex(&ua));
    // a stranger learns nothing: not the record, not the residence, not even whether one exists
    let (s, e) = get_json(&url).await;
    assert_eq!((s, e["error"]["code"].as_str()), (401, Some("UNAUTHORIZED")), "{e}");
    assert!(e.get("residenceCountry").is_none() && e.get("status").is_none() && e.get("record").is_none());
    let (s, e2) = get_json(&format!("{base}/v1/attestations/{}", to_hex(&key_address(&user(86))))).await;
    assert_eq!((s, e2.clone()), (401, e), "an account with no record answers exactly the same");
    // the same record posted again and again stores nothing more
    let lines = || std::fs::read_to_string(&file).unwrap().lines().count();
    assert_eq!(lines(), 1);
    for _ in 0..3 {
        let (s, _) = post_json(&format!("{base}/v1/attestations"), &rec, &[]).await;
        assert_eq!(s, 200, "idempotent");
    }
    assert_eq!(lines(), 1, "the store did not grow");
    // the account reads its own record with a fresh signature
    let (s, st) = get_own_attestation(&base, &g, &u).await;
    assert_eq!((s, st["status"].as_str(), st["residenceCountry"].as_str(), st["record"]["signature"].as_str()), (200, Some("valid"), Some("PA"), rec["signature"].as_str()), "{st}");
    // refused: another key's signature for this address, a signature for another address, a stale or future time,
    // a time that is not the signed one, one header missing
    let get_with = |t: String, nonce: String, sig: String| {
        let url = url.clone();
        async move { http_call("GET", &url, None, &[("X-Postern-Read-Time", &t), ("X-Postern-Read-Nonce", &nonce), ("X-Postern-Read-Signature", &sig)]).await.unwrap().0 }
    };
    let now_t = now();
    let other = user(86);
    let by_other = to_hex(&perps_node::sig::sign(&other, &perps_node::attest::read_digest(&g, &ua, now_t, 900_001)));
    assert_eq!(get_with(now_t.to_string(), "900001".into(), by_other).await, 401, "another key");
    let (t2, n2, sig_for_other_addr) = read_auth(&g, &other, now_t);
    assert_eq!(get_with(t2, n2, sig_for_other_addr).await, 401, "a signature over another address");
    for skew in [-61_000i64, 61_000] {
        let ts = (now() as i64 + skew) as u64;
        let (t3, n3, s3) = read_auth(&g, &u, ts);
        assert_eq!(get_with(t3, n3, s3).await, 401, "outside the 60 s window: {skew}");
    }
    let (_, n4, s4) = read_auth(&g, &u, now_t);
    assert_eq!(get_with((now_t + 1).to_string(), n4, s4.clone()).await, 401, "the time is part of what is signed");
    assert_eq!(http_call("GET", &url, None, &[("X-Postern-Read-Signature", &s4)]).await.unwrap().0, 401);
    // the operator: no path without a configured token; with it, the token and nothing else
    let bearer = "Bearer operator-token-0123456789abcdef";
    assert_eq!(http_call("GET", &url, None, &[("Authorization", bearer)]).await.unwrap().0, 401, "no operator token configured: fail closed");
    *n.app.operator_token.lock().unwrap() = Some(b"operator-token-0123456789abcdef".to_vec());
    let (s, b) = http_call("GET", &url, None, &[("Authorization", bearer)]).await.unwrap();
    let st: serde_json::Value = serde_json::from_slice(&b).unwrap();
    assert_eq!((s, st["residenceCountry"].as_str()), (200, Some("PA")));
    assert_eq!(http_call("GET", &url, None, &[("Authorization", "Bearer operator-token-0123456789abcdeX")]).await.unwrap().0, 401);
    assert_eq!(http_call("GET", &url, None, &[("Authorization", &format!("Bearer {ORACLE_TOKEN}"))]).await.unwrap().0, 401, "the oracle feeder's token is not the operator's");
    // the POST answer does not echo the record
    let (s, st) = post_json(&format!("{base}/v1/attestations"), &rec, &[]).await;
    assert_eq!((s, st["status"].as_str(), st["record"].is_null()), (200, Some("valid"), true));
    // a replacement issued less than a minute after the held record is refused; a later one replaces it
    let soon = attestation(&g, &u, &statements, "PA", t - 1000 + 59_999, t + 86_400_000);
    let (s, e) = post_json(&format!("{base}/v1/attestations"), &soon, &[]).await;
    assert_eq!((s, e["error"]["code"].as_str()), (400, Some("ATTESTATION_INVALID")), "{e}");
    let later = attestation(&g, &u, &statements, "PA", t - 1000 + 60_000, t + 86_400_000);
    let (s, _) = post_json(&format!("{base}/v1/attestations"), &later, &[]).await;
    assert_eq!((s, lines()), (200, 2));
    // accounts the engine does not know are bounded (unit level: the bound is 10,000)
    let mut store = perps_node::attest::Attestations::new(statements.clone(), &g.vault, None, now());
    let nobody = |_: &Address| false;
    for i in 0..3u64 {
        store.submit(&attestation(&g, &user(900 + i), &statements, "PA", t - 1000, t + 86_400_000), now(), &nobody).unwrap();
    }
    assert_eq!(store.len(), 3);
    assert_eq!(perps_node::attest::MAX_UNKNOWN_ACCOUNTS, 10_000);
    let _ = n.stop.send(true);
}

/// R436-17: the bound itself (accounts without a deposit share a fixed number of places; an account the engine
/// knows is never refused for it, and frees its place once it is known).
#[test]
fn r436_17_the_store_of_accounts_without_a_deposit_is_bounded() {
    let (g, _) = perps_node::dev::dev_genesis("attest-bound", 7, 5, 10, 0);
    let notices: serde_json::Value = serde_json::from_slice(&std::fs::read(std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../compliance/legal/notices.json")).unwrap()).unwrap();
    let statements = perps_node::attest::statements_from_notices(&notices).unwrap();
    let t = now();
    let mut store = perps_node::attest::Attestations::new(statements.clone(), &g.vault, None, t);
    let rec = |i: u64| attestation(&g, &user(5_000 + i), &statements, "PA", t - 1000, t + 86_400_000);
    let nobody = |_: &Address| false;
    // the default is 10,000 places; the rule is the same with 40 (signing 10,000 records takes a minute)
    assert_eq!(store.max_unknown, perps_node::attest::MAX_UNKNOWN_ACCOUNTS);
    store.max_unknown = 40;
    let cap = store.max_unknown as u64;
    for i in 0..cap {
        store.submit(&rec(i), t, &nobody).unwrap();
    }
    let e = store.submit(&rec(cap), t, &nobody).expect_err("no place left for an account without a deposit");
    assert!(e.starts_with("STORE FULL"), "{e}");
    assert_eq!(store.len() as u64, cap);
    // an account the engine knows is stored all the same
    let known_one = key_address(&user(5_000 + cap));
    store.submit(&rec(cap), t, &|a: &Address| *a == known_one).unwrap();
    // and when earlier accounts became known (they deposited), their places are free again
    let first = key_address(&user(5_000));
    store.submit(&rec(cap + 1), t, &|a: &Address| *a == first).unwrap();
    assert_eq!(store.len() as u64, cap + 2);
}

/// A raw GET from a chosen loopback source address that sends the request and reads NOTHING until asked.
async fn raw_get(from_ip: &str, url: &str, path: &str) -> tokio::net::TcpStream {
    use tokio::io::AsyncWriteExt;
    let target: SocketAddr = url.trim_start_matches("http://").parse().unwrap();
    let sock = tokio::net::TcpSocket::new_v4().unwrap();
    sock.bind(format!("{from_ip}:0").parse().unwrap()).unwrap();
    let mut s = sock.connect(target).await.unwrap();
    s.write_all(format!("GET {path} HTTP/1.1\r\nHost: node\r\nConnection: close\r\n\r\n").as_bytes()).await.unwrap();
    s
}

/// R436-14 (`/v1/blocks/stream`): the stream was built whole in memory for any number of callers. Properties: one
/// stream per client address and four in all (429 beyond); a consumer that does not read is disconnected after
/// the slow-consumer time and its slot is free again; a reader gets every block, in order.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn r436_14_block_streams_are_bounded_backpressured_and_slow_consumers_cut() {
    use tokio::io::AsyncReadExt;
    let n = start_node("stream").await;
    let base = n.url.clone();
    // 60 blocks of 2,000 deposits: about 25 MB of ndjson, far more than loopback socket buffers hold
    {
        let mut v = n.app.view.lock().unwrap();
        let a = key_address(&user(70));
        for i in 0..120_000u64 {
            v.deposits.insert(i, deposit(i, a, 1_000_000 + i));
        }
    }
    for _ in 0..600 {
        if n.app.seq.lock().unwrap().engine.next_deposit_index() == 120_000 {
            break;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    // every deposit is in a block; nothing else makes blocks in this test, so the height is fixed from here on
    // (read AFTER the last deposit was credited: reading it before raced with the last block)
    assert_eq!(n.app.seq.lock().unwrap().engine.next_deposit_index(), 120_000);
    let height = n.app.seq.lock().unwrap().log.last_height;
    assert!(height >= 60, "{height}");
    *n.app.read_rate.lock().unwrap() = perps_node::sequencer::RateLimit::new(0.0, 0.0);
    n.app.streams.slow_ms.store(600, std::sync::atomic::Ordering::Relaxed);
    let path = "/v1/blocks/stream?from=1";
    // a consumer that never reads holds its address's stream
    let mut idle = raw_get("127.0.0.1", &base, path).await;
    tokio::time::sleep(Duration::from_millis(150)).await;
    let (s, e) = get_json(&format!("{base}{path}")).await;
    assert_eq!((s, e["error"]["code"].as_str()), (429, Some("RATE_LIMITED")), "a second stream from the same address: {e}");
    // three more addresses fill the four slots; a fifth address is refused
    let mut others = Vec::new();
    for ip in ["127.0.0.2", "127.0.0.3", "127.0.0.4"] {
        others.push(raw_get(ip, &base, path).await);
    }
    tokio::time::sleep(Duration::from_millis(150)).await;
    let mut fifth = raw_get("127.0.0.5", &base, path).await;
    let mut head = vec![0u8; 12];
    fifth.read_exact(&mut head).await.unwrap();
    assert_eq!(std::str::from_utf8(&head).unwrap(), "HTTP/1.1 429", "four streams at once is the limit");
    // the silent consumers are cut once they have not taken a chunk for the slow-consumer time (600 ms here):
    // their slots come back without anyone reading a byte
    // (restated for the residue of R436-14: the limits are one table now, `in_use` counts what the semaphore and
    // the address set counted)
    for _ in 0..300 {
        if n.app.streams.in_use() == 0 {
            break;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    assert_eq!(n.app.streams.in_use(), 0, "every slot was given back");
    // a consumer that reads gets every block, in order
    let (s, body) = http_call("GET", &format!("{base}{path}"), None, &[]).await.unwrap();
    assert_eq!(s, 200);
    let heights: Vec<u64> = std::str::from_utf8(&body).unwrap().lines().map(|l| serde_json::from_str::<serde_json::Value>(l).unwrap()["height"].as_u64().unwrap()).collect();
    assert_eq!(heights, (1..=height).collect::<Vec<_>>());
    // what the cut consumer finds when it finally reads: the head of the stream, not all of it
    let mut got = Vec::new();
    let _ = idle.read_to_end(&mut got).await;
    assert!(got.starts_with(b"HTTP/1.1 200"));
    assert!(got.len() < body.len(), "the silent consumer was cut: {} of {} bytes", got.len(), body.len());
    drop(others);
    let _ = n.stop.send(true);
}

/// One HTTP/1.1 exchange with the response HEADERS (the node's own client returns status and body only).
/// Header names are lower-cased; a repeated header's values are joined with ", ".
async fn raw_http(method: &str, base: &str, path: &str, headers: &[(&str, &str)], body: Option<&str>) -> (u16, BTreeMap<String, String>, Vec<u8>) {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    let host = base.trim_start_matches("http://");
    let mut s = tokio::net::TcpStream::connect(host).await.unwrap();
    let mut req = format!("{method} {path} HTTP/1.1\r\nHost: {host}\r\nConnection: close\r\n");
    for (k, v) in headers {
        req += &format!("{k}: {v}\r\n");
    }
    if let Some(b) = body {
        req += &format!("Content-Type: application/json\r\nContent-Length: {}\r\n", b.len());
    }
    req += "\r\n";
    req += body.unwrap_or("");
    s.write_all(req.as_bytes()).await.unwrap();
    let mut buf = Vec::new();
    tokio::time::timeout(Duration::from_secs(10), s.read_to_end(&mut buf)).await.expect("the node answers and closes").unwrap();
    let split = buf.windows(4).position(|w| w == b"\r\n\r\n").expect("a header block");
    let head = String::from_utf8_lossy(&buf[..split]).to_string();
    let mut lines = head.split("\r\n");
    let status: u16 = lines.next().unwrap().split(' ').nth(1).unwrap().parse().unwrap();
    let mut h: BTreeMap<String, String> = BTreeMap::new();
    for l in lines {
        let (k, v) = l.split_once(':').unwrap();
        let (k, v) = (k.trim().to_ascii_lowercase(), v.trim().to_string());
        h.entry(k).and_modify(|x| *x = format!("{x}, {v}")).or_insert(v);
    }
    (status, h, buf[split + 4..].to_vec())
}

async fn ws_with_origin(base: &str, origin: Option<&str>) -> Result<Ws, u16> {
    let url = format!("{}/v1/ws?country=PA", base.replace("http://", "ws://"));
    let mut req = tokio_tungstenite::tungstenite::client::IntoClientRequest::into_client_request(url.as_str()).unwrap();
    if let Some(o) = origin {
        req.headers_mut().insert("origin", o.parse().unwrap());
    }
    match tokio_tungstenite::connect_async(req).await {
        Ok((ws, _)) => Ok(ws),
        Err(tokio_tungstenite::tungstenite::Error::Http(r)) => Err(r.status().as_u16()),
        Err(e) => panic!("neither an upgrade nor an HTTP refusal: {e}"),
    }
}

/// F434-2 (434; decided by the coordinator): CORS is an explicit allowlist, empty by default; exact origin only,
/// never `*`, never a reflected origin, no credentials; a correct preflight; the WebSocket upgrade checks `Origin`
/// against the same list.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn f434_2_cors_is_an_exact_allowlist_with_preflight_and_a_websocket_origin_check() {
    const A: &str = "https://posterndex.example";
    const B: &str = "http://127.0.0.1:49920";
    const EVIL: &str = "https://evil.example";
    let n = start_node("cors").await;
    let base = n.url.clone();
    let cors_headers = |h: &BTreeMap<String, String>| h.keys().filter(|k| k.starts_with("access-control-")).cloned().collect::<Vec<_>>();

    // 1. DEFAULT (empty list): no CORS header for anyone, no preflight answered, the WebSocket is as before
    for origin in [None, Some(A), Some(EVIL)] {
        let hs: Vec<(&str, &str)> = origin.iter().map(|o| ("Origin", *o)).collect();
        let (st, h, _) = raw_http("GET", &base, "/v1/status", &hs, None).await;
        assert_eq!((st, cors_headers(&h)), (200, vec![]), "default: nothing is granted to {origin:?}: {h:?}");
    }
    let (st, h, _) = raw_http("OPTIONS", &base, "/v1/actions", &[("Origin", A), ("Access-Control-Request-Method", "POST")], None).await;
    assert!(st != 204 && st != 200 && cors_headers(&h).is_empty(), "default: no preflight is answered: {st} {h:?}");
    drop(ws_with_origin(&base, Some(EVIL)).await.expect("no list: the upgrade does not look at Origin"));

    // 2. a list of two origins
    *n.app.cors_origins.lock().unwrap() = vec![A.to_string(), B.to_string()];
    for o in [A, B] {
        // 2xx, 4xx and a POST refusal all carry the header, with exactly the caller's own listed origin
        for (method, path, body, want) in [("GET", "/v1/status", None, 200), ("GET", "/v1/markets/NOPE-USD", None, 404), ("POST", "/v1/actions", Some("{}"), 400)] {
            let (st, h, _) = raw_http(method, &base, path, &[("Origin", o)], body).await;
            assert_eq!(st, want, "{method} {path}");
            assert_eq!(h.get("access-control-allow-origin").map(String::as_str), Some(o), "{method} {path}: {h:?}");
            assert!(h.get("vary").is_some_and(|v| v.to_ascii_lowercase().contains("origin")), "{h:?}");
            assert!(!h.contains_key("access-control-allow-credentials"), "no credentials, ever: {h:?}");
        }
    }
    // refused: another site, `null`, and every near miss of a listed origin; also two Origin headers
    let near = [
        EVIL,
        "null",
        "*",
        "https://posterndex.example.evil.example",
        "https://evilposterndex.example",
        "https://posterndex.example:443",
        "http://posterndex.example",
        "https://POSTERNDEX.example",
        "https://posterndex.example/",
        "http://127.0.0.1:4992",
        "http://127.0.0.1:49920.evil.example",
        "http://127.0.0.1",
    ];
    for o in near {
        let (st, h, _) = raw_http("GET", &base, "/v1/status", &[("Origin", o)], None).await;
        assert_eq!(st, 200, "CORS is the browser's rule: the read itself is public");
        assert_eq!(cors_headers(&h), Vec::<String>::new(), "nothing is granted to {o:?}: {h:?}");
        assert!(h.get("vary").is_some_and(|v| v.to_ascii_lowercase().contains("origin")), "a cache must still key on Origin: {h:?}");
    }
    let (_, h, _) = raw_http("GET", &base, "/v1/status", &[("Origin", A), ("Origin", EVIL)], None).await;
    assert_eq!(cors_headers(&h), Vec::<String>::new(), "two Origin headers: {h:?}");
    let (_, h, _) = raw_http("GET", &base, "/v1/status", &[], None).await;
    assert_eq!(cors_headers(&h), Vec::<String>::new(), "no Origin, no grant: {h:?}");
    // never `*`, whatever is asked
    for o in [A, EVIL, "*"] {
        let (_, h, _) = raw_http("GET", &base, "/v1/markets", &[("Origin", o)], None).await;
        assert_ne!(h.get("access-control-allow-origin").map(String::as_str), Some("*"));
    }

    // 3. preflight: the methods and every header API.md lets a page send
    let (st, h, body) = raw_http("OPTIONS", &base, "/v1/actions", &[("Origin", A), ("Access-Control-Request-Method", "POST"), ("Access-Control-Request-Headers", "content-type, x-postern-geo, x-postern-geo-mac")], None).await;
    assert_eq!((st, body.len()), (204, 0), "{h:?}");
    assert_eq!(h.get("access-control-allow-origin").map(String::as_str), Some(A));
    let methods: Vec<String> = h["access-control-allow-methods"].split(',').map(|x| x.trim().to_string()).collect();
    assert_eq!(methods, vec!["GET", "POST", "OPTIONS"]);
    let allowed: Vec<String> = h["access-control-allow-headers"].split(',').map(|x| x.trim().to_ascii_lowercase()).collect();
    for need in ["content-type", "x-postern-geo", "x-postern-geo-mac", "x-postern-read-time", "x-postern-read-signature"] {
        assert!(allowed.iter().any(|x| x == need), "{need} is in {allowed:?}");
    }
    assert!(!allowed.iter().any(|x| x == "authorization" || x == "*"), "operator tokens are not for pages: {allowed:?}");
    assert_eq!(h.get("access-control-max-age").map(String::as_str), Some("600"));
    assert!(!h.contains_key("access-control-allow-credentials"));
    // any path, and GET too
    let (st, h, _) = raw_http("OPTIONS", &base, "/v1/accounts/0x0000000000000000000000000000000000000001", &[("Origin", B), ("Access-Control-Request-Method", "GET")], None).await;
    assert_eq!((st, h.get("access-control-allow-origin").map(String::as_str)), (204, Some(B)));
    // a preflight from an origin that is not listed is refused and granted nothing
    for o in [EVIL, "null", "https://posterndex.example.evil.example"] {
        let (st, h, _) = raw_http("OPTIONS", &base, "/v1/actions", &[("Origin", o), ("Access-Control-Request-Method", "POST")], None).await;
        assert_eq!((st, cors_headers(&h)), (403, vec![]), "{o}: {h:?}");
    }

    // 4. the WebSocket upgrade: a listed origin and a client without Origin are served; another site is refused
    let mut ws = ws_with_origin(&base, Some(A)).await.expect("a listed origin");
    ws_send(&mut ws, serde_json::json!({"op":"ping","id":1})).await;
    drop(ws);
    drop(ws_with_origin(&base, Some(B)).await.expect("the second listed origin"));
    drop(ws_with_origin(&base, None).await.expect("not a browser: no Origin"));
    for o in [EVIL, "null", "https://posterndex.example.evil.example", "https://posterndex.example:443"] {
        assert_eq!(ws_with_origin(&base, Some(o)).await.err(), Some(403), "{o}");
    }

    // 5. what the flag accepts: exact origins only
    for ok in [A, B, "https://app.posterndex.example:8443", "http://localhost:5173", "http://[::1]:49920"] {
        assert_eq!(perps_node::api::parse_origin(ok).as_deref(), Ok(ok));
    }
    for bad in ["*", "null", "", "https://", "https://*.posterndex.example", "https://posterndex.example/", "https://posterndex.example/app", "posterndex.example", "ftp://posterndex.example",
        "https://Posterndex.example", "https://a.example https://b.example", "https://user@a.example", "https://a.example:", "https://a.example:99999", "https://a.example:80/x", "https://a.example?x", "https://a..example", "http://[::1"] {
        assert!(perps_node::api::parse_origin(bad).is_err(), "{bad:?} is not an origin");
    }
    n.stop.send(true).unwrap();
}

/// Residue (a) of R436-17 (436's re-check of 531750e): the read signature bound account and time only, so a
/// captured header set read the record again for 60 s. It now binds method, path and a nonce and is spent once.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn r436_17_residue_a_read_signature_is_for_one_request_and_is_spent_once() {
    use perps_node::attest::{read_digest, ReadOnce, ReadReplay, READ_WINDOW_MS};
    let n = start_node("readonce").await;
    let (base, g) = (n.url.clone(), n.g.clone());
    let u = user(90);
    let ua = key_address(&u);
    let url = format!("{base}/v1/attestations/{}", to_hex(&ua));
    let get = |t: String, nonce: String, sig: String| {
        let url = url.clone();
        async move { http_call("GET", &url, None, &[("X-Postern-Read-Time", &t), ("X-Postern-Read-Nonce", &nonce), ("X-Postern-Read-Signature", &sig)]).await.unwrap().0 }
    };
    // one signature, one read: the captured headers read nothing a second time
    let (t, nonce, sig) = read_auth(&g, &u, now());
    assert_eq!(get(t.clone(), nonce.clone(), sig.clone()).await, 200);
    assert_eq!(get(t.clone(), nonce.clone(), sig.clone()).await, 401, "REPLAY of a captured header set");
    assert_eq!(get(t.clone(), nonce.clone(), sig.clone()).await, 401, "and again");
    // the nonce is part of what is signed, in one spelling only
    let (t, nonce, sig) = read_auth(&g, &u, now());
    let other_nonce = (nonce.parse::<u64>().unwrap() + 1_000_000).to_string();
    assert_eq!(get(t.clone(), other_nonce, sig.clone()).await, 401, "another nonce than the signed one");
    assert_eq!(get(t.clone(), format!("0{nonce}"), sig.clone()).await, 401, "a second spelling of the same nonce");
    assert_eq!(http_call("GET", &url, None, &[("X-Postern-Read-Time", &t), ("X-Postern-Read-Signature", &sig)]).await.unwrap().0, 401, "no nonce");
    assert_eq!(get(t.clone(), nonce.clone(), sig.clone()).await, 200, "the refusals above did not spend it");
    // the signature is for THIS path: the same account, time and nonce signed in the old form (account and time
    // only) or for another path or method is not a read of this record
    let t0 = now();
    let reg = |fields: serde_json::Value| perps_node::eip712::Registry::from_json(&serde_json::json!({"AttestationRead": fields})).unwrap();
    let sign_with = |r: &perps_node::eip712::Registry, v: perps_node::eip712::Val| to_hex(&perps_node::sig::sign(&u, &perps_node::eip712::digest(&g.engine_domain.separator(), &r.hash_struct("AttestationRead", &v).unwrap())));
    use perps_node::eip712::Val;
    let old = sign_with(&reg(serde_json::json!([{"name":"account","type":"address"},{"name":"timestamp","type":"uint64"}])), Val::Struct(vec![Val::Address(ua), Val::Uint(t0 as u128)]));
    assert_eq!(get(t0.to_string(), "424242".into(), old).await, 401, "the old form, which named no request");
    let full = reg(serde_json::json!([{"name":"account","type":"address"},{"name":"method","type":"string"},{"name":"path","type":"string"},{"name":"timestamp","type":"uint64"},{"name":"nonce","type":"uint64"}]));
    let own_path = format!("/v1/attestations/{}", to_hex(&ua));
    for (method, path, want) in [("GET", "/v1/attestations".to_string(), 401), ("GET", format!("/v1/attestations/{}", to_hex(&key_address(&user(91)))), 401), ("POST", own_path.clone(), 401), ("GET", own_path.clone(), 200)] {
        let nonce = 500_000 + want as u64 + path.len() as u64 + method.len() as u64;
        let sig = sign_with(&full, Val::Struct(vec![Val::Address(ua), Val::Str(method.into()), Val::Str(path.clone()), Val::Uint(t0 as u128), Val::Uint(nonce as u128)]));
        assert_eq!(get(t0.to_string(), nonce.to_string(), sig.clone()).await, want, "{method} {path}");
        // the node's own digest is exactly that message
        if want == 200 {
            assert_eq!(sig, to_hex(&perps_node::sig::sign(&u, &read_digest(&g, &ua, t0, nonce))));
        }
    }
    // bounded per account: beyond it this account waits (429), another account still reads
    n.app.read_replay.lock().unwrap().max_per_account = 5;
    let mut codes = Vec::new();
    for _ in 0..4 {
        let (t, nonce, sig) = read_auth(&g, &u, now());
        codes.push(get(t, nonce, sig).await);
    }
    assert_eq!(codes, vec![200, 200, 429, 429], "3 reads were remembered already: 2 more, then refused");
    let v = user(92);
    let (t, nonce, sig) = read_auth(&g, &v, now());
    assert_eq!(http_call("GET", &format!("{base}/v1/attestations/{}", to_hex(&key_address(&v))), None, &[("X-Postern-Read-Time", &t), ("X-Postern-Read-Nonce", &nonce), ("X-Postern-Read-Signature", &sig)]).await.unwrap().0, 200);
    // bounded in all, and nothing is forgotten while its signature could still be accepted
    let mut r = ReadReplay::default();
    r.max = 3;
    let t = 1_000_000u64;
    let acct = |i: u8| [i; 20];
    assert_eq!(r.once(&acct(1), 7, t, t), ReadOnce::Fresh);
    assert_eq!(r.once(&acct(1), 7, t, t + 1), ReadOnce::Replayed);
    assert_eq!(r.once(&acct(2), 7, t, t), ReadOnce::Fresh, "the nonce is per account");
    assert_eq!(r.once(&acct(3), 1, t + READ_WINDOW_MS, t), ReadOnce::Fresh, "a timestamp at the far edge of the window");
    assert_eq!(r.once(&acct(4), 1, t, t), ReadOnce::Full, "full: refused, not forgotten");
    assert_eq!(r.once(&acct(1), 7, t, t + READ_WINDOW_MS), ReadOnce::Replayed, "still acceptable by time, still remembered");
    // once a timestamp can no longer be accepted its entry goes: two of three here
    assert_eq!(r.once(&acct(4), 1, t + READ_WINDOW_MS + 1, t + READ_WINDOW_MS + 1), ReadOnce::Fresh);
    assert_eq!(r.len(), 2, "the far-edge entry and the new one");
    assert_eq!(r.once(&acct(3), 1, t + READ_WINDOW_MS, t + READ_WINDOW_MS + 1), ReadOnce::Replayed);
    n.stop.send(true).unwrap();
}

/// Fills the node's log with `deposits` deposits (2,000 per block) and returns the height once all are credited.
async fn fill_blocks(n: &Running, deposits: u64) -> u64 {
    {
        let mut v = n.app.view.lock().unwrap();
        let a = key_address(&user(70));
        for i in 0..deposits {
            v.deposits.insert(i, deposit(i, a, 1_000_000 + i));
        }
    }
    for _ in 0..600 {
        if n.app.seq.lock().unwrap().engine.next_deposit_index() == deposits {
            break;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    assert_eq!(n.app.seq.lock().unwrap().engine.next_deposit_index(), deposits);
    let h = n.app.seq.lock().unwrap().log.last_height;
    h
}

/// Residue (b) of R436-14: `GET /v1/blocks` rendered up to 500 blocks (every signer recovered) on the runtime's
/// own threads. On a ONE-thread runtime the render must run on another thread; the answer stops at a byte bound
/// and a caller that continues from its last height still gets every block.
#[tokio::test]
async fn r436_14_residue_b_blocks_are_rendered_off_the_runtime_up_to_a_byte_bound() {
    let n = start_node("blocksoff").await;
    let base = n.url.clone();
    let height = fill_blocks(&n, 20_000).await;
    assert!(height >= 10, "{height}");
    *n.app.read_rate.lock().unwrap() = perps_node::sequencer::RateLimit::new(0.0, 0.0);
    let heights = |j: &serde_json::Value| j["blocks"].as_array().unwrap().iter().map(|b| b["height"].as_u64().unwrap()).collect::<Vec<_>>();
    // the whole range in one answer under the default bound, rendered on a thread that is not the runtime's
    let (s, all) = get_json(&format!("{base}/v1/blocks?from=1&limit=500")).await;
    assert_eq!((s, heights(&all)), (200, (1..=height).collect::<Vec<_>>()));
    let rendered_on = *n.app.block_renders.last_thread.lock().unwrap();
    assert!(rendered_on.is_some(), "the render went through the bounded path");
    assert_ne!(rendered_on, Some(std::thread::current().id()), "this runtime has ONE thread (this one): the render must not be on it");
    // a byte bound: an answer holds at least one block and stops once the bound is reached
    let one = all["blocks"][0].to_string().len();
    n.app.block_renders.max_bytes.store(1, std::sync::atomic::Ordering::Relaxed);
    let (_, j) = get_json(&format!("{base}/v1/blocks?from=1&limit=500")).await;
    assert_eq!(heights(&j), vec![1], "never fewer than one block");
    n.app.block_renders.max_bytes.store(one * 2 + one / 2, std::sync::atomic::Ordering::Relaxed);
    let (mut from, mut got, mut calls) = (1u64, Vec::new(), 0);
    loop {
        let (s, j) = get_json(&format!("{base}/v1/blocks?from={from}&limit=500&raw=1")).await;
        assert_eq!(s, 200);
        let hs = heights(&j);
        if hs.is_empty() {
            break;
        }
        assert!(hs.len() <= 4, "the bound cut the answer: {} blocks", hs.len());
        // the raw form still decodes to the block of that height
        for b in j["blocks"].as_array().unwrap() {
            let raw = perps_node::util::parse_hex_vec(b["raw"].as_str().unwrap()).unwrap();
            assert_eq!(perps_node::types::Block::from_bytes(&raw).unwrap().header.height, b["height"].as_u64().unwrap());
        }
        from = hs.last().unwrap() + 1;
        got.extend(hs);
        calls += 1;
    }
    assert_eq!(got, (1..=height).collect::<Vec<_>>(), "continuing from the last height + 1 loses nothing");
    assert!(calls >= 3, "{calls}");
    // the answers are the same blocks as the unbounded one
    n.app.block_renders.max_bytes.store(perps_node::api::BLOCKS_MAX_BYTES, std::sync::atomic::Ordering::Relaxed);
    let (_, again) = get_json(&format!("{base}/v1/blocks?from=1&limit=500")).await;
    assert_eq!(again, all);
    assert_eq!(n.app.block_renders.permits.available_permits(), perps_node::api::BLOCKS_RENDERS, "every render gave its place back");
    // at most BLOCKS_RENDERS at once: with every place taken a read waits, it is not rendered beside them
    let held: Vec<_> = (0..perps_node::api::BLOCKS_RENDERS).map(|_| n.app.block_renders.permits.clone().try_acquire_owned().unwrap()).collect();
    let url = format!("{base}/v1/blocks?from=1&limit=1");
    let waiting = tokio::spawn(async move { get_json(&url).await.0 });
    tokio::time::sleep(Duration::from_millis(400)).await;
    assert!(!waiting.is_finished(), "no place: the read waits");
    drop(held);
    assert_eq!(waiting.await.unwrap(), 200);
    let _ = n.stop.send(true);
}

/// A raw GET like `raw_get`, with headers; reads nothing.
async fn raw_get_h(from_ip: &str, url: &str, path: &str, headers: &[(&str, &str)]) -> tokio::net::TcpStream {
    use tokio::io::AsyncWriteExt;
    let target: SocketAddr = url.trim_start_matches("http://").parse().unwrap();
    let sock = tokio::net::TcpSocket::new_v4().unwrap();
    sock.bind(format!("{from_ip}:0").parse().unwrap()).unwrap();
    let mut s = sock.connect(target).await.unwrap();
    let mut req = format!("GET {path} HTTP/1.1\r\nHost: node\r\nConnection: close\r\n");
    for (k, v) in headers {
        req += &format!("{k}: {v}\r\n");
    }
    req += "\r\n";
    s.write_all(req.as_bytes()).await.unwrap();
    s
}

async fn status_of(s: &mut tokio::net::TcpStream) -> u16 {
    use tokio::io::AsyncReadExt;
    let mut head = vec![0u8; 12];
    tokio::time::timeout(Duration::from_secs(20), s.read_exact(&mut head)).await.expect("a status line").unwrap();
    std::str::from_utf8(&head).unwrap()[9..12].parse().unwrap()
}

/// Residue (c) of R436-14: everyone behind one proxy shared ONE stream slot (the limit was keyed by the TCP peer),
/// and four parties could sit on the four slots for hours by taking a chunk every 9 s. Now: the client is the
/// address the EDGE names, believed only with a valid edge assertion (a bare header is not believed); an idle
/// stream gives way to a request that needs its slot; no request holds a slot beyond its deadline.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn r436_14_residue_c_stream_slots_are_fair_and_only_the_edge_names_the_client() {
    use std::sync::atomic::Ordering::Relaxed;
    use tokio::io::AsyncReadExt;
    let secret = b"edge-secret-0123456789abcdef0123456789abcdef".to_vec();
    let n = start_node_with("fair", Some(secret.clone())).await;
    let base = n.url.clone();
    let height = fill_blocks(&n, 120_000).await;
    assert!(height >= 60, "{height}");
    *n.app.read_rate.lock().unwrap() = perps_node::sequencer::RateLimit::new(0.0, 0.0);
    *n.app.edge_client_header.lock().unwrap() = Some("x-edge-client".into());
    let st = n.app.streams.clone();
    // nothing gives way and nothing is cut by time in the first part
    st.slow_ms.store(600_000, Relaxed);
    st.evict_idle_ms.store(600_000, Relaxed);
    let path = "/v1/blocks/stream?from=1";
    let mut rid = 0u8;
    let mut edge = |client: &str| -> Vec<(String, String)> {
        rid += 1;
        let (h, m) = assertion(&secret, "PA", rid, now());
        vec![("x-postern-geo".into(), h), ("x-postern-geo-mac".into(), m), ("x-edge-client".into(), client.into())]
    };
    let refs = |v: &[(String, String)]| v.iter().map(|(k, v)| (k.as_str().to_string(), v.as_str().to_string())).collect::<Vec<_>>();
    async fn open(base: &str, path: &str, from_ip: &str, hs: &[(String, String)]) -> (u16, tokio::net::TcpStream) {
        let h: Vec<(&str, &str)> = hs.iter().map(|(k, v)| (k.as_str(), v.as_str())).collect();
        let mut s = raw_get_h(from_ip, base, path, &h).await;
        (status_of(&mut s).await, s)
    }

    // 1. two clients behind ONE peer address (the edge), each named by the edge with its assertion: each has its
    //    own stream. (They read nothing: their streams stay open.)
    let h1 = edge("203.0.113.1");
    let (s1, _c1) = open(&base, path, "127.0.0.1", &h1).await;
    let (s2, _c2) = open(&base, path, "127.0.0.1", &edge("203.0.113.2")).await;
    assert_eq!((s1, s2), (200, 200), "two clients of the edge are two clients");
    // the same client again: one stream per client
    assert_eq!(open(&base, path, "127.0.0.1", &edge("203.0.113.1")).await.0, 429);
    // a bare header is not believed: without an assertion the request is the PEER's, whatever address it claims
    let bare = |c: &str| vec![("x-edge-client".to_string(), c.to_string())];
    let (s3, _c3) = open(&base, path, "127.0.0.1", &bare("203.0.113.3")).await;
    assert_eq!(s3, 200, "counted against the peer address, which had no stream yet");
    assert_eq!(open(&base, path, "127.0.0.1", &bare("203.0.113.4")).await.0, 429, "a second claimed address from the same peer is the same peer");
    // a wrong MAC, a replayed assertion and a header that is not one address are not believed either
    let mut bad = edge("203.0.113.5");
    bad[1].1 = "0".repeat(64);
    assert_eq!(open(&base, path, "127.0.0.1", &bad).await.0, 429, "a MAC the edge did not make");
    let mut replay = refs(&h1);
    replay[2].1 = "203.0.113.6".into();
    assert_eq!(open(&base, path, "127.0.0.1", &replay).await.0, 429, "the first request's assertion again, with another address");
    assert_eq!(open(&base, path, "127.0.0.1", &edge("203.0.113.7, 203.0.113.8")).await.0, 429, "a list is not an address");
    assert_eq!((st.in_use(), st.evicted.load(Relaxed)), (3, 0));

    // 2. a fourth client takes the last slot; a fifth finds none while nothing has been idle long enough
    let (s4, mut c4) = open(&base, path, "127.0.0.2", &[]).await;
    assert_eq!((s4, st.in_use()), (200, 4));
    assert_eq!(open(&base, path, "127.0.0.3", &[]).await.0, 429, "four streams at once");

    // 3. the four consumers read nothing: once that has lasted the idle time, a client that needs a slot gets
    //    one of theirs, and reads every block in order while a second eviction happens beside it
    for _ in 0..1200 {
        if st.idle(now(), 300) == 4 {
            break;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    assert_eq!(st.idle(now(), 300), 4, "four streams whose consumers left a chunk waiting");
    st.evict_idle_ms.store(300, Relaxed);
    let reader = {
        let base = base.clone();
        tokio::spawn(async move {
            let mut s = raw_get("127.0.0.3", &base, "/v1/blocks/stream?from=1").await;
            let mut buf = Vec::new();
            s.read_to_end(&mut buf).await.unwrap();
            buf
        })
    };
    // the fourth client asks again: its OWN idle stream gives way (never someone else's for a client at its cap)
    let (s4b, mut c4b) = open(&base, path, "127.0.0.2", &[]).await;
    assert_eq!(s4b, 200);
    let buf = reader.await.unwrap();
    let text = String::from_utf8_lossy(&buf).to_string();
    assert!(text.starts_with("HTTP/1.1 200"), "{}", &text[..60.min(text.len())]);
    // chunked transfer: the ndjson lines are the ones that parse as a block
    let heights: Vec<u64> = text.lines().filter_map(|l| serde_json::from_str::<serde_json::Value>(l).ok()).filter_map(|j| j["height"].as_u64()).collect();
    assert_eq!(heights, (1..=height).collect::<Vec<_>>(), "the reader that took a freed slot got every block in order");
    assert_eq!(st.evicted.load(Relaxed), 2, "one for the newcomer, one for the fourth client's second request");
    // the fourth client's first stream was evicted (by its own second request, or already by the newcomer):
    // it ends, and what its consumer finds is the head of the stream only
    let mut got = Vec::new();
    tokio::time::timeout(Duration::from_secs(20), c4.read_to_end(&mut got)).await.expect("an evicted stream ends").unwrap();
    assert!(got.len() < buf.len(), "{} of {}", got.len(), buf.len());
    let mut rest = Vec::new();
    tokio::time::timeout(Duration::from_secs(150), c4b.read_to_end(&mut rest)).await.expect("the second request of the fourth client ends").unwrap();

    // 4. no request holds a slot beyond its deadline, even one that is neither slow enough to be cut nor in
    //    anyone's way
    st.evict_idle_ms.store(600_000, Relaxed);
    for _ in 0..600 {
        if st.in_use() <= 2 {
            break;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    let before = st.in_use();
    assert!(before <= 2, "{before}");
    st.max_ms.store(500, Relaxed);
    let (s9, _c9) = open(&base, path, "127.0.0.9", &[]).await;
    assert_eq!((s9, st.in_use()), (200, before + 1));
    for _ in 0..600 {
        if st.in_use() == before {
            break;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    assert_eq!(st.in_use(), before, "the silent stream ended at its deadline (500 ms) with the slow-consumer time at 10 minutes");
    let _ = n.stop.send(true);
}

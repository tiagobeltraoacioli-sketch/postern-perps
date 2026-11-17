//! The public API of `docs/API.md` (REST + WebSocket) over the sequencer state.
//!
//! Served by this prototype: status, eip712, markets, tickers (oracle-only), book / trades / candles (empty with
//! the stub engine: it has no matching), committee, finality (latest, by height), blocks (by height, range,
//! ndjson stream of the stored range), actions (POST with `?wait=included`, GET by hash), accounts (account,
//! orders, agents, nonce, withdrawals, deposits), withdrawals by id with the certificate, deposits by L2 tx,
//! forced requests, geo, the validator signature intake, and the WebSocket (`status`, `account`, `book`, `trades`
//! channels, `action` and `ping` ops).
//!
//! UNAUDITED PROTOTYPE.

use crate::actions::{field_bool, parse_request, verify_signed_action};
use crate::geo::GeoDecision;
use crate::node::{ip_key, now_ms, App};
use crate::types::{deposit_json, forced_json, input_json, decode_inputs};
use crate::util::{parse_address, parse_hex_fixed, to_hex, Address};
use axum::extract::ws::{Message, WebSocket, WebSocketUpgrade};
use axum::extract::{ConnectInfo, DefaultBodyLimit, Path, Query, State};
use axum::http::{HeaderMap, HeaderValue, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

pub const API_VERSION: &str = "0.2";

type Q = Query<HashMap<String, String>>;

pub fn error(status: u16, code: &str, message: &str, details: serde_json::Value) -> Response {
    let st = StatusCode::from_u16(status).unwrap_or(StatusCode::INTERNAL_SERVER_ERROR);
    let mut r = (st, Json(serde_json::json!({"error": {"code": code, "message": message, "details": details}}))).into_response();
    if status == 429 {
        r.headers_mut().insert("retry-after", HeaderValue::from_static("1"));
    }
    r
}

fn not_found(what: &str) -> Response {
    error(404, "NOT_FOUND", what, serde_json::json!({}))
}

fn addr_param(s: &str) -> Result<Address, Response> {
    parse_address(s).map_err(|e| error(400, "MALFORMED", &e, serde_json::json!({"path": "address"})))
}

pub fn router(app: Arc<App>) -> Router {
    Router::new()
        .route("/v1/status", get(status))
        .route("/v1/node", get(node_info))
        .route("/v1/eip712", get(eip712))
        .route("/v1/markets", get(markets))
        .route("/v1/markets/{symbol}", get(market))
        .route("/v1/tickers", get(tickers))
        .route("/v1/markets/{symbol}/ticker", get(ticker))
        .route("/v1/markets/{symbol}/book", get(book))
        .route("/v1/markets/{symbol}/trades", get(empty_list_trades))
        .route("/v1/markets/{symbol}/candles", get(empty_list_candles))
        .route("/v1/markets/{symbol}/funding", get(empty_list_funding))
        .route("/v1/committee", get(committee))
        .route("/v1/finality/latest", get(finality_latest))
        .route("/v1/finality/signatures", post(finality_signature))
        .route("/v1/finality/{height}", get(finality_at))
        .route("/v1/blocks", get(blocks_range))
        .route("/v1/blocks/stream", get(blocks_stream))
        .route("/v1/blocks/{height}", get(block_at))
        .route("/v1/actions", post(post_action))
        .route("/v1/actions/{hash}", get(get_action))
        .route("/v1/accounts/{address}", get(account))
        .route("/v1/accounts/{address}/orders", get(account_orders))
        .route("/v1/accounts/{address}/agents", get(account_agents))
        .route("/v1/accounts/{address}/nonce", get(account_nonce))
        .route("/v1/accounts/{address}/withdrawals", get(account_withdrawals))
        .route("/v1/accounts/{address}/deposits", get(account_deposits))
        .route("/v1/accounts/{address}/fills", get(account_fills))
        .route("/v1/accounts/{address}/funding", get(account_funding))
        .route("/v1/withdrawals/{id}", get(withdrawal))
        .route("/v1/deposits/{tx}", get(deposits_by_tx))
        .route("/v1/forced", get(forced))
        .route("/v1/geo", get(geo))
        .route("/v1/ws", get(ws))
        .route("/v1/oracle/{symbol}", get(oracle_status))
        .fallback(|| async { not_found("unknown endpoint") })
        .layer(DefaultBodyLimit::max(16 * 1024))
        .with_state(app)
}

fn block_summary(b: &crate::types::Block) -> serde_json::Value {
    serde_json::json!({"height": b.header.height, "time": b.header.time, "hash": to_hex(&b.header.log_hash),
        "stateRoot": to_hex(&b.header.state_root), "logHash": to_hex(&b.header.log_hash)})
}

async fn status(State(app): State<Arc<App>>) -> Response {
    let s = app.seq.lock().unwrap();
    let latest = s.log.read(s.log.last_height).ok().flatten();
    let fin = s.latest_final().and_then(|h| s.block(h).ok().flatten());
    let v = app.view.lock().unwrap();
    let vals = app.validators.lock().unwrap();
    let online = vals.values().filter(|x| x.latched.is_none()).count();
    let genesis_ref = serde_json::json!({"height": 0, "time": app.g.genesis_time, "hash": to_hex(&app.g.genesis_log_hash()),
        "stateRoot": to_hex(&[0u8; 32]), "logHash": to_hex(&app.g.genesis_log_hash())});
    Json(serde_json::json!({
        "name": "postern-perps-node", "version": env!("CARGO_PKG_VERSION"), "apiVersion": API_VERSION,
        "mode": app.mode.name(), "unaudited": true,
        "network": {"eip712": app.g.engine_domain.to_json(), "collateralChainId": 700770, "vault": to_hex(&app.g.vault),
            "collateralToken": to_hex(&[0u8; 20]), "collateralSymbol": "bUSDC", "collateralDecimals": 6},
        "time": now_ms(),
        "latestBlock": latest.as_ref().map(block_summary).unwrap_or(genesis_ref),
        "latestFinalized": fin.as_ref().map(block_summary),
        "sequencer": {"state": if s.halted.is_some() {"halted"} else {"running"}, "blockIntervalMs": app.block_interval_ms,
            "pendingActions": s.mempool_len()},
        "committee": {"n": app.g.committee.len(), "threshold": app.g.threshold, "online": online},
        "vaultMode": "unknown",
        "l2": {"finalizedBlock": v.finalized_block, "lastDepositBlock": v.deposits.values().next_back().map(|d| d.l2_block).unwrap_or(0),
            "endpointsAgree": v.endpoints_agree},
    }))
    .into_response()
}

/// Node diagnostics outside API.md's `Status` (dev): engine, validators, L2 watcher, recovery, genesis.
async fn node_info(State(app): State<Arc<App>>) -> Response {
    let s = app.seq.lock().unwrap();
    let v = app.view.lock().unwrap();
    let vals = app.validators.lock().unwrap();
    Json(serde_json::json!({
        "engine": s.engine.name(), "haltReason": s.halted, "collateralToken": "TBD-deploy (zero address in /v1/status)",
        "validators": vals.values().map(|x| serde_json::json!({"address": x.address, "height": x.height, "signed": x.signed, "last": x.last, "latched": x.latched})).collect::<Vec<_>>(),
        "l2": {"configured": app.l2_configured, "finalizedBlock": v.finalized_block, "scannedTo": v.scanned_to, "l2TimeS": v.l2_time_s,
            "lastDepositIndex": v.deposits.keys().next_back(), "endpointsAgree": v.endpoints_agree, "lastError": v.last_error, "polls": v.polls, "disagreements": v.disagreements},
        "recovery": {"snapshotHeight": s.recovered.snapshot_height, "replayedBlocks": s.recovered.replayed_blocks,
            "tornTailBytes": s.recovered.torn_tail_bytes, "checkpointsLoaded": s.recovered.checkpoints_loaded},
        "genesisLogHash": to_hex(&app.g.genesis_log_hash()), "typedDataHash": to_hex(&app.g.typed_data_hash),
        "committeeSetHash": to_hex(&app.g.committee_set_hash()),
    }))
    .into_response()
}

async fn eip712(State(app): State<Arc<App>>) -> Response {
    let td = &app.g.typed_data_json;
    Json(serde_json::json!({
        "domain": app.g.engine_domain.to_json(), "types": td["types"], "actions": app.g.types.actions,
        "agentAllowed": app.g.types.agent_allowed, "enums": td.get("enums"), "notes": td.get("notes"), "specVersion": td.get("specVersion"),
        "scales": {"price": 8, "size": 8, "usdc": 6},
    }))
    .into_response()
}

fn market_json(app: &App, m: &crate::genesis::Market) -> serde_json::Value {
    let mut j = m.json.clone();
    if let Some(o) = j.as_object_mut() {
        o.retain(|k, _| !k.starts_with("mock"));
        o.insert("status".into(), "active".into());
        o.insert("quote".into(), "USD".into());
        // per-market values default to markets.json "defaults" (fees, funding, oracle)
        for k in ["makerFeeBps", "takerFeeBps", "liquidationFeeBps", "fundingIntervalSec", "maxFundingRate", "oracle"] {
            if !o.contains_key(k) {
                if let Some(v) = app.g.markets_defaults.get(k) {
                    o.insert(k.into(), v.clone());
                }
            }
        }
    }
    j
}

async fn markets(State(app): State<Arc<App>>) -> Response {
    Json(serde_json::json!({"markets": app.g.markets.iter().map(|m| market_json(&app, m)).collect::<Vec<_>>()})).into_response()
}

async fn market(State(app): State<Arc<App>>, Path(sym): Path<String>) -> Response {
    match app.g.market_by_symbol(&sym) {
        Some(m) => Json(market_json(&app, m)).into_response(),
        None => not_found("unknown market"),
    }
}

fn ticker_json(app: &App, m: &crate::genesis::Market) -> serde_json::Value {
    let s = app.seq.lock().unwrap();
    let t = s.engine.time();
    // the dev node has no oracle feeder and the stats indexer is not built: zero values, stated in /v1/node
    serde_json::json!({"symbol": m.symbol, "markPrice": "0", "indexPrice": "0", "lastPrice": "0", "fundingRate": "0",
        "nextFundingTime": (t / 3_600_000 + 1) * 3_600_000, "openInterest": "0", "volume24h": "0", "high24h": "0", "low24h": "0",
        "price24hAgo": "0", "change24h": "0", "height": s.engine.height(), "time": t})
}

async fn tickers(State(app): State<Arc<App>>) -> Response {
    Json(serde_json::json!({"tickers": app.g.markets.iter().map(|m| ticker_json(&app, m)).collect::<Vec<_>>()})).into_response()
}

async fn ticker(State(app): State<Arc<App>>, Path(sym): Path<String>) -> Response {
    match app.g.market_by_symbol(&sym) {
        Some(m) => Json(ticker_json(&app, m)).into_response(),
        None => not_found("unknown market"),
    }
}

async fn book(State(app): State<Arc<App>>, Path(sym): Path<String>, q: Q) -> Response {
    if app.g.market_by_symbol(&sym).is_none() {
        return not_found("unknown market");
    }
    if let Some(d) = q.get("depth") {
        match d.parse::<u32>() {
            Ok(n) if (1..=500).contains(&n) => {}
            _ => return error(400, "MALFORMED", "depth must be 1 to 500", serde_json::json!({"path": "depth"})),
        }
    }
    let s = app.seq.lock().unwrap();
    Json(serde_json::json!({"symbol": sym, "seq": 0, "height": s.engine.height(), "time": s.engine.time(), "bids": [], "asks": []})).into_response()
}

async fn empty_list_trades(State(app): State<Arc<App>>, Path(sym): Path<String>) -> Response {
    if app.g.market_by_symbol(&sym).is_none() {
        return not_found("unknown market");
    }
    Json(serde_json::json!({"trades": []})).into_response()
}
async fn empty_list_candles(State(app): State<Arc<App>>, Path(sym): Path<String>, q: Q) -> Response {
    if app.g.market_by_symbol(&sym).is_none() {
        return not_found("unknown market");
    }
    if !matches!(q.get("interval").map(|s| s.as_str()).unwrap_or("1m"), "1m" | "5m" | "15m" | "1h" | "4h" | "1d") {
        return error(400, "MALFORMED", "unknown interval", serde_json::json!({"path": "interval"}));
    }
    Json(serde_json::json!({"candles": []})).into_response()
}
async fn empty_list_funding(State(app): State<Arc<App>>, Path(sym): Path<String>) -> Response {
    if app.g.market_by_symbol(&sym).is_none() {
        return not_found("unknown market");
    }
    Json(serde_json::json!({"funding": []})).into_response()
}

async fn oracle_status(State(app): State<Arc<App>>, Path(sym): Path<String>) -> Response {
    let Some(m) = app.g.market_by_symbol(&sym) else { return not_found("unknown market") };
    let s = app.seq.lock().unwrap();
    Json(serde_json::json!({"symbol": m.symbol, "status": "halted", "price": "0", "time": s.engine.time(),
        "round": s.engine.last_oracle_round().to_string(), "sources": [], "signers": 0, "threshold": app.g.oracle_quorum,
        "haltReason": "no oracle feeder wired to this dev node (agent 394's feeder submits OracleUpdate inputs)"}))
    .into_response()
}

async fn committee(State(app): State<Arc<App>>) -> Response {
    Json(app.g.committee_json()).into_response()
}

fn checkpoint_with_hash(app: &App, h: u64) -> Option<serde_json::Value> {
    let s = app.seq.lock().unwrap();
    let bh = s.block(h).ok().flatten().map(|b| b.header.log_hash);
    s.aggregator.checkpoint_json(&app.g, h, bh)
}

async fn finality_latest(State(app): State<Arc<App>>) -> Response {
    let h = app.seq.lock().unwrap().latest_final();
    match h.and_then(|h| checkpoint_with_hash(&app, h)) {
        Some(j) => Json(j).into_response(),
        None => not_found("no final checkpoint yet"),
    }
}

async fn finality_at(State(app): State<Arc<App>>, Path(h): Path<u64>) -> Response {
    let fh = {
        let s = app.seq.lock().unwrap();
        s.aggregator.final_at_or_below(&app.g, h)
    };
    match fh.and_then(|h| checkpoint_with_hash(&app, h)) {
        Some(j) => Json(j).into_response(),
        None => not_found("no final checkpoint at or below this height"),
    }
}

/// `{"height": H, "signature": "0x…65 bytes"}` from a remote validator. Only a signature that recovers to a
/// committee member over THIS node's body at H is kept (a validator with another root signs another digest).
async fn finality_signature(State(app): State<Arc<App>>, Json(body): Json<serde_json::Value>) -> Response {
    let h = match body.get("height").and_then(|v| v.as_u64()) {
        Some(h) => h,
        None => return error(400, "MALFORMED", "height", serde_json::json!({})),
    };
    let sig = match body.get("signature").and_then(|v| v.as_str()).map(parse_hex_fixed::<65>) {
        Some(Ok(s)) => s,
        _ => return error(400, "MALFORMED", "signature", serde_json::json!({})),
    };
    let r = app.seq.lock().unwrap().add_signature(h, sig);
    match r {
        Ok(fin) => Json(serde_json::json!({"accepted": true, "final": fin})).into_response(),
        Err(e) => error(400, "BAD_SIGNATURE", &e, serde_json::json!({"height": h})),
    }
}

fn block_json(app: &App, b: &crate::types::Block, raw: bool) -> serde_json::Value {
    let inputs = decode_inputs(&app.g.types, &b.inputs_encoded).unwrap_or_default();
    let mut j = serde_json::json!({
        "height": b.header.height, "time": b.header.time, "prevLogHash": to_hex(&b.header.prev_log_hash),
        "logHash": to_hex(&b.header.log_hash), "stateRoot": to_hex(&b.header.state_root),
        "inputs": inputs.iter().map(|i| {
            let mut j = input_json(&app.g.types, i);
            if let crate::types::Input::Action(a) = i {
                if let Ok(v) = verify_signed_action(&app.g, a) {
                    j["account"] = to_hex(&v.account).into();
                    j["signer"] = to_hex(&v.signer).into();
                }
            }
            j
        }).collect::<Vec<_>>(),
        "inputsEncoded": to_hex(&b.inputs_encoded), "sequencerSignature": to_hex(&b.sequencer_signature),
    });
    if raw {
        j["raw"] = serde_json::Value::String(to_hex(&b.to_bytes()));
    }
    j
}

async fn block_at(State(app): State<Arc<App>>, Path(h): Path<u64>, q: Q) -> Response {
    let b = app.seq.lock().unwrap().block(h);
    match b {
        Ok(Some(b)) => Json(block_json(&app, &b, q.get("raw").is_some())).into_response(),
        _ => not_found("no block at this height"),
    }
}

async fn blocks_range(State(app): State<Arc<App>>, q: Q) -> Response {
    let from: u64 = q.get("from").and_then(|s| s.parse().ok()).unwrap_or(1).max(1);
    let limit: u64 = q.get("limit").and_then(|s| s.parse().ok()).unwrap_or(100).clamp(1, 500);
    let raw = q.get("raw").is_some();
    let s = app.seq.lock().unwrap();
    let mut out = Vec::new();
    for h in from..from.saturating_add(limit) {
        match s.block(h) {
            Ok(Some(b)) => out.push(block_json(&app, &b, raw)),
            _ => break,
        }
    }
    Json(serde_json::json!({"blocks": out})).into_response()
}

async fn blocks_stream(State(app): State<Arc<App>>, q: Q) -> Response {
    let from: u64 = q.get("from").and_then(|s| s.parse().ok()).unwrap_or(1).max(1);
    let s = app.seq.lock().unwrap();
    let last = s.log.last_height;
    let mut body = String::new();
    for h in from..=last.min(from.saturating_add(10_000)) {
        if let Ok(Some(b)) = s.block(h) {
            body.push_str(&block_json(&app, &b, q.get("raw").is_some()).to_string());
            body.push('\n');
        }
    }
    ([("content-type", "application/x-ndjson")], body).into_response()
}

/// Admission path shared by REST and WebSocket: parse, recover the signer, geoblock, then the sequencer.
/// Where the action arrived from: REST (the assertion is in the request headers) or a WebSocket frame (the
/// assertion is the frame's own `edgeGeo`, never the upgrade request's headers: R395-36).
pub enum Via<'a> {
    Rest,
    Ws(Option<&'a serde_json::Value>),
}

pub fn submit(app: &App, peer: Option<SocketAddr>, headers: &HeaderMap, query_country: Option<&str>, req: &serde_json::Value, via: Via) -> Result<crate::sequencer::Ack, Response> {
    let now = now_ms();
    let geo_refusal = |reason: &str| {
        error(451, "GEO_RESTRICTED", "PosternDEX Perpetuals is not available in your jurisdiction; cancelling, reducing and withdrawing stay open.",
            serde_json::json!({"country": null, "listVersion": app.geo.version, "assertion": reason}))
    };
    // country: with an edge verifier configured, ONLY from a valid per-action assertion
    let asserted: Option<(Option<String>, Option<String>)> = match (&app.geo.edge, &via) {
        (Some(ev), Via::Rest) => {
            let h = headers.get(crate::geo::ASSERTION_HEADER).and_then(|v| v.to_str().ok());
            let m = headers.get(crate::geo::ASSERTION_MAC_HEADER).and_then(|v| v.to_str().ok());
            Some(ev.verify(h, m, now).map_err(|e| geo_refusal(&e))?)
        }
        (Some(ev), Via::Ws(eg)) => {
            let eg = eg.ok_or_else(|| geo_refusal("missing edgeGeo"))?;
            Some(ev.verify(eg.get("header").and_then(|v| v.as_str()), eg.get("mac").and_then(|v| v.as_str()), now).map_err(|e| geo_refusal(&e))?)
        }
        // no verifier: a WebSocket frame that claims an edge assertion cannot be honoured
        (None, Via::Ws(Some(_))) => return Err(geo_refusal("edgeGeo present but this node has no edge verifier")),
        (None, _) => None,
    };
    if let Some(p) = peer {
        if !app.ip_rate.lock().unwrap().allow(&ip_key(p.ip()), now) {
            return Err(error(429, "RATE_LIMITED", "per-IP action rate", serde_json::json!({})));
        }
    }
    let a = parse_request(&app.g, req).map_err(|e| {
        let code = if e.starts_with("signature:") { "BAD_SIGNATURE" } else { "MALFORMED" };
        error(400, code, &e, serde_json::json!({}))
    })?;
    let v = verify_signed_action(&app.g, &a).map_err(|e| error(400, "BAD_SIGNATURE", &e.to_string(), serde_json::json!({})))?;
    let (country, sub) = match asserted {
        Some(cs) => cs,
        None => app.geo.country(peer.map(|p| p.ip()), headers, query_country),
    };
    let d = app.geo.decide(country.as_deref(), sub.as_deref(), &v.account, &v.signer);
    let ro = field_bool(&app.g, &v.primary, &a.message, "reduceOnly");
    if !app.geo.admits(&d, &v.primary, ro) {
        return Err(match d {
            GeoDecision::Sanctioned => error(451, "SANCTIONED", "this address cannot use this access point", serde_json::json!({"listVersion": app.geo.version})),
            _ => error(
                451,
                "GEO_RESTRICTED",
                "PosternDEX Perpetuals is not available in your jurisdiction; cancelling, reducing and withdrawing stay open.",
                serde_json::json!({"country": country, "listVersion": app.geo.version, "policy": if app.geo.exit_only {"exit_only"} else {"actions_refused"}}),
            ),
        });
    }
    let r = app.seq.lock().unwrap().admit_signed(a, now);
    r.map_err(|e| error(e.http, e.code, &e.message, serde_json::json!({})))
}

async fn post_action(
    State(app): State<Arc<App>>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    q: Q,
    body: Result<axum::body::Bytes, axum::extract::rejection::BytesRejection>,
) -> Response {
    let body = match body {
        Ok(b) => b,
        Err(_) => return error(413, "TOO_LARGE", "body over 16 KiB", serde_json::json!({})),
    };
    let req: serde_json::Value = match serde_json::from_slice(&body) {
        Ok(v) => v,
        Err(e) => return error(400, "MALFORMED", &format!("body is not JSON: {e}"), serde_json::json!({})),
    };
    let ack = match submit(&app, Some(peer), &headers, q.get("country").map(|s| s.as_str()), &req, Via::Rest) {
        Ok(a) => a,
        Err(r) => return r,
    };
    if q.get("wait").map(|s| s.as_str()) == Some("included") {
        let deadline = tokio::time::Instant::now() + Duration::from_secs(2);
        loop {
            let cur = app.seq.lock().unwrap().acks.get(&ack.action_hash).cloned();
            if let Some(c) = &cur {
                if c.status == "included" {
                    return Json(c.to_json()).into_response();
                }
            }
            if tokio::time::Instant::now() >= deadline {
                break;
            }
            let _ = tokio::time::timeout(Duration::from_millis(50), app.new_block.notified()).await;
        }
    }
    Json(ack.to_json()).into_response()
}

async fn get_action(State(app): State<Arc<App>>, Path(h): Path<String>) -> Response {
    let Ok(h) = parse_hex_fixed::<32>(&h) else { return error(400, "MALFORMED", "action hash", serde_json::json!({})) };
    match app.seq.lock().unwrap().acks.get(&h) {
        Some(a) => Json(a.to_json()).into_response(),
        None => not_found("unknown or pruned action"),
    }
}

async fn account(State(app): State<Arc<App>>, Path(a): Path<String>) -> Response {
    let a = match addr_param(&a) {
        Ok(a) => a,
        Err(r) => return r,
    };
    Json(account_shape(app.seq.lock().unwrap().engine.account_json(&a))).into_response()
}

/// API.md `Account` from the engine's account JSON: fields the engine adapter does not compute yet are given
/// conservative values (margins 0, withdrawable = max(0, equity)); listed as a gap in the node report.
pub fn account_shape(e: serde_json::Value) -> serde_json::Value {
    let g = |k: &str, d: &str| e.get(k).and_then(|v| v.as_str()).unwrap_or(d).to_string();
    let collateral = g("collateral", "0.000000");
    let equity = g("equity", &collateral);
    let withdrawable = if equity.starts_with('-') { "0.000000".to_string() } else { equity.clone() };
    serde_json::json!({
        "address": e["address"], "height": e["height"], "time": e["time"], "collateral": collateral,
        "unrealizedPnl": g("unrealizedPnl", "0.000000"), "equity": equity.clone(),
        "initialMargin": g("initialMargin", "0.000000"), "maintenanceMargin": g("maintenanceMargin", "0.000000"),
        "freeCollateral": g("freeCollateral", &equity), "withdrawable": g("withdrawable", &withdrawable),
        "marginRatio": g("marginRatio", "0"),
        "feeTier": {"tier": 0, "makerFeeBps": "1.5", "takerFeeBps": "4.5", "volume30d": "0"},
        "positions": e.get("positions").cloned().unwrap_or(serde_json::json!([])),
        "openOrders": e.get("openOrders").cloned().unwrap_or(serde_json::json!(0)),
    })
}

async fn account_orders(State(app): State<Arc<App>>, Path(a): Path<String>) -> Response {
    let a = match addr_param(&a) {
        Ok(a) => a,
        Err(r) => return r,
    };
    Json(app.seq.lock().unwrap().engine.orders_json(&a)).into_response()
}

async fn account_agents(State(app): State<Arc<App>>, Path(a): Path<String>) -> Response {
    let a = match addr_param(&a) {
        Ok(a) => a,
        Err(r) => return r,
    };
    Json(app.seq.lock().unwrap().engine.agents_json(&a)).into_response()
}

async fn account_fills(Path(a): Path<String>) -> Response {
    if let Err(r) = addr_param(&a) {
        return r;
    }
    // fills are engine events; the node's fill index is not built yet (stated gap)
    Json(serde_json::json!({"fills": []})).into_response()
}

async fn account_funding(Path(a): Path<String>) -> Response {
    if let Err(r) = addr_param(&a) {
        return r;
    }
    Json(serde_json::json!({"payments": []})).into_response()
}

async fn account_nonce(State(app): State<Arc<App>>, Path(a): Path<String>) -> Response {
    let a = match addr_param(&a) {
        Ok(a) => a,
        Err(r) => return r,
    };
    let s = app.seq.lock().unwrap();
    let hi = s.engine.highest_nonce(&a);
    let agents: Vec<serde_json::Value> = s.engine.agents_json(&a)["agents"]
        .as_array()
        .cloned()
        .unwrap_or_default()
        .iter()
        .filter_map(|x| x["agent"].as_str().and_then(|h| parse_address(h).ok()))
        .map(|ag| serde_json::json!({"agent": to_hex(&ag), "nextNonce": s.engine.highest_nonce(&ag).map(|x| x + 1).unwrap_or(0).to_string()}))
        .collect();
    // nextNonce = highest used + 1 (a lower bound: SPEC 2.5 wants a millisecond clock value in the window)
    Json(serde_json::json!({"account": to_hex(&a), "nextNonce": hi.map(|x| (x + 1).to_string()).unwrap_or_else(|| "0".into()), "agents": agents}))
        .into_response()
}

fn withdrawal_json(app: &App, id: u64) -> Option<serde_json::Value> {
    let s = app.seq.lock().unwrap();
    let rec = s.engine.withdrawal(id)?;
    let cert = s.certificate(id);
    let status = if cert.is_some() { "certified" } else { "included" };
    Some(serde_json::json!({
        "withdrawalId": id.to_string(), "account": to_hex(&rec.account), "recipient": to_hex(&rec.recipient),
        "amount": crate::util::scaled_to_decimal(rec.amount as u128, 6), "fee": crate::util::scaled_to_decimal(rec.fee as u128, 6),
        "status": status, "requestedHeight": rec.height, "includedHeight": rec.height,
        "checkpointHeight": cert.as_ref().and_then(|c| c["checkpoint"]["height"].as_u64()),
        "certificate": cert, "l2TxHash": null, "availableAt": null,
        "note": "submitted / delayed / paid need the vault (agent 390): not tracked by this prototype"
    }))
}

async fn withdrawal(State(app): State<Arc<App>>, Path(id): Path<u64>) -> Response {
    match withdrawal_json(&app, id) {
        Some(j) => Json(j).into_response(),
        None => not_found("unknown withdrawal"),
    }
}

async fn account_withdrawals(State(app): State<Arc<App>>, Path(a): Path<String>) -> Response {
    let a = match addr_param(&a) {
        Ok(a) => a,
        Err(r) => return r,
    };
    let n = app.seq.lock().unwrap().engine.withdrawal_count();
    let mut out = Vec::new();
    for id in (0..n).rev() {
        let is_mine = app.seq.lock().unwrap().engine.withdrawal(id).map(|w| w.account == a).unwrap_or(false);
        if is_mine {
            if let Some(j) = withdrawal_json(&app, id) {
                out.push(j);
            }
        }
        if out.len() >= 1000 {
            break;
        }
    }
    Json(serde_json::json!({"withdrawals": out})).into_response()
}

fn deposit_status_json(app: &App, d: &crate::types::DepositEvent) -> serde_json::Value {
    let s = app.seq.lock().unwrap();
    let credited = d.index < s.engine.next_deposit_index();
    let mut j = deposit_json(d);
    j["depositId"] = j["index"].clone();
    j["amount"] = serde_json::Value::String(crate::util::scaled_to_decimal(d.amount as u128, 6));
    j["status"] = serde_json::Value::String(if credited { "credited" } else { "l2Finalized" }.into());
    j["creditedHeight"] = s.deposit_heights.get(&d.index).copied().into();
    j
}

async fn account_deposits(State(app): State<Arc<App>>, Path(a): Path<String>) -> Response {
    let a = match addr_param(&a) {
        Ok(a) => a,
        Err(r) => return r,
    };
    let ds: Vec<_> = app.view.lock().unwrap().deposits.values().filter(|d| d.account == a).cloned().collect();
    Json(serde_json::json!({"deposits": ds.iter().rev().map(|d| deposit_status_json(&app, d)).collect::<Vec<_>>()})).into_response()
}

async fn deposits_by_tx(State(app): State<Arc<App>>, Path(tx): Path<String>) -> Response {
    let Ok(tx) = parse_hex_fixed::<32>(&tx) else { return error(400, "MALFORMED", "tx hash", serde_json::json!({})) };
    let ds: Vec<_> = app.view.lock().unwrap().deposits.values().filter(|d| d.l2_tx_hash == tx).cloned().collect();
    Json(serde_json::json!({"deposits": ds.iter().map(|d| deposit_status_json(&app, d)).collect::<Vec<_>>()})).into_response()
}

async fn forced(State(app): State<Arc<App>>, q: Q) -> Response {
    let acc = match q.get("account").map(|s| parse_address(s)) {
        Some(Ok(a)) => Some(a),
        Some(Err(e)) => return error(400, "MALFORMED", &e, serde_json::json!({})),
        None => None,
    };
    let fs: Vec<_> = app.view.lock().unwrap().forced.values().filter(|f| acc.is_none_or(|a| f.account == a)).cloned().collect();
    let s = app.seq.lock().unwrap();
    let next = s.engine.next_forced_index();
    let t = s.engine.time();
    let out: Vec<_> = fs
        .iter()
        .map(|f| {
            let mut j = forced_json(f);
            j["forcedId"] = j["index"].clone();
            j["deadlineTime"] = f.deadline.into();
            j["status"] = (if f.index < next { "included" } else if f.deadline < t { "expired" } else { "queued" }).into();
            j["includedHeight"] = s.forced_heights.get(&f.index).copied().into();
            j
        })
        .collect();
    Json(serde_json::json!({"forced": out})).into_response()
}

async fn geo(State(app): State<Arc<App>>, ConnectInfo(peer): ConnectInfo<SocketAddr>, headers: HeaderMap, q: Q) -> Response {
    let (country, sub) = app.geo.country(Some(peer.ip()), &headers, q.get("country").map(|s| s.as_str()));
    let d = app.geo.decide(country.as_deref(), sub.as_deref(), &[0u8; 20], &[0u8; 20]);
    let restricted = !matches!(d, GeoDecision::Allow);
    Json(serde_json::json!({
        "country": country, "restricted": restricted,
        "policy": if !restricted {"open"} else if app.geo.exit_only {"exit_only"} else {"actions_refused"},
        "listVersion": app.geo.version, "reasonCode": if restricted {serde_json::Value::from("JURISDICTION")} else {serde_json::Value::Null},
        "message": if restricted {"PosternDEX Perpetuals is not available in your jurisdiction."} else {""},
        "sanctioned": false
    }))
    .into_response()
}

// ---------------- WebSocket ----------------

async fn ws(State(app): State<Arc<App>>, ConnectInfo(peer): ConnectInfo<SocketAddr>, headers: HeaderMap, up: WebSocketUpgrade) -> Response {
    up.max_message_size(16 * 1024).on_upgrade(move |s| ws_session(app, s, peer, headers))
}

async fn ws_session(app: Arc<App>, mut sock: WebSocket, peer: SocketAddr, headers: HeaderMap) {
    let mut rx = app.blocks_tx.subscribe();
    let mut status_sub = false;
    let mut accounts: Vec<Address> = Vec::new();
    let mut ping = tokio::time::interval(Duration::from_secs(15));
    let mut last_in = tokio::time::Instant::now();
    let mut reply_override: Option<serde_json::Value> = None;
    loop {
        tokio::select! {
            m = sock.recv() => {
                let Some(Ok(m)) = m else { return };
                last_in = tokio::time::Instant::now();
                let txt = match m { Message::Text(t) => t.to_string(), Message::Close(_) => return, _ => continue };
                let j: serde_json::Value = match serde_json::from_str(&txt) {
                    Ok(j) => j,
                    Err(_) => { let _ = sock.send(Message::Text(serde_json::json!({"op":"error","error":{"code":"MALFORMED","message":"not JSON","details":{}}}).to_string().into())).await; continue; }
                };
                let id = j.get("id").cloned().unwrap_or(serde_json::Value::Null);
                let reply = match j.get("op").and_then(|v| v.as_str()) {
                    Some("ping") => serde_json::json!({"op":"pong","time": now_ms()}),
                    Some("subscribe") => {
                        let ch = j.get("channel").and_then(|v| v.as_str()).unwrap_or("");
                        match ch {
                            "status" => {
                                status_sub = true;
                                let _ = sock.send(Message::Text(serde_json::json!({"op":"subscribed","id":id,"channel":"status"}).to_string().into())).await;
                                let (lb, lf) = { let s = app.seq.lock().unwrap(); (s.block(s.log.last_height).ok().flatten().map(|b| block_summary(&b)), s.latest_final().and_then(|h| s.block(h).ok().flatten()).map(|b| block_summary(&b))) };
                                serde_json::json!({"channel":"status","type":"update","data":{"latestBlock": lb, "latestFinalized": lf}})
                            }
                            "account" => match j.get("account").and_then(|v| v.as_str()).map(parse_address) {
                                Some(Ok(a)) => {
                                    if accounts.len() >= 100 { serde_json::json!({"op":"error","id":id,"error":{"code":"RATE_LIMITED","message":"subscriptions per connection","details":{}}}) }
                                    else {
                                        accounts.push(a);
                                        let snap = { let s = app.seq.lock().unwrap(); serde_json::json!({"account": account_shape(s.engine.account_json(&a)), "orders": s.engine.orders_json(&a)["orders"], "agents": s.engine.agents_json(&a)["agents"]}) };
                                        let _ = sock.send(Message::Text(serde_json::json!({"op":"subscribed","id":id,"channel":"account","account":to_hex(&a)}).to_string().into())).await;
                                        serde_json::json!({"channel":"account","type":"snapshot","account":to_hex(&a),"data":snap})
                                    }
                                }
                                _ => serde_json::json!({"op":"error","id":id,"error":{"code":"MALFORMED","message":"account","details":{}}}),
                            },
                            "book" | "trades" | "ticker" | "candles" => {
                                let sym = j.get("symbol").and_then(|v| v.as_str()).unwrap_or("");
                                if sym != "*" && app.g.market_by_symbol(sym).is_none() {
                                    serde_json::json!({"op":"error","id":id,"error":{"code":"NOT_FOUND","message":"unknown market","details":{}}})
                                } else {
                                    let _ = sock.send(Message::Text(serde_json::json!({"op":"subscribed","id":id,"channel":ch,"symbol":sym}).to_string().into())).await;
                                    if ch == "ticker" {
                                        let ms: Vec<&crate::genesis::Market> = if sym == "*" { app.g.markets.iter().collect() } else { app.g.market_by_symbol(sym).into_iter().collect() };
                                        let last = ms.len().saturating_sub(1);
                                        for (i, m) in ms.iter().enumerate() {
                                            let msg = serde_json::json!({"channel":"ticker","symbol":m.symbol,"type":"update","data":ticker_json(&app, m)});
                                            if i < last { let _ = sock.send(Message::Text(msg.to_string().into())).await; } else { reply_override = Some(msg); }
                                        }
                                        reply_override.take().unwrap_or(serde_json::json!({"op":"pong","time":now_ms()}))
                                    } else {
                                        let (h, t) = { let s = app.seq.lock().unwrap(); (s.engine.height(), s.engine.time()) };
                                        let data = match ch {
                                            "book" => serde_json::json!({"symbol":sym,"seq":0,"height":h,"time":t,"bids":[],"asks":[]}),
                                            "trades" => serde_json::json!({"trades":[]}),
                                            _ => serde_json::json!({"candles":[]}),
                                        };
                                        let mut m = serde_json::json!({"channel":ch,"symbol":sym,"type":"snapshot","data":data});
                                        if ch == "candles" { m["interval"] = j.get("interval").cloned().unwrap_or("1m".into()); }
                                        m
                                    }
                                }
                            }
                            _ => serde_json::json!({"op":"error","id":id,"error":{"code":"MALFORMED","message":"unknown channel","details":{}}}),
                        }
                    }
                    Some("unsubscribe") => {
                        let ch = j.get("channel").and_then(|v| v.as_str()).unwrap_or("");
                        if ch == "status" { status_sub = false; }
                        if ch == "account" { if let Some(Ok(a)) = j.get("account").and_then(|v| v.as_str()).map(parse_address) { accounts.retain(|x| *x != a); } }
                        serde_json::json!({"op":"unsubscribed","id":id,"channel":ch})
                    }
                    Some("action") => {
                        // the frame's own assertion; stripped before the request is parsed (never reaches the engine)
                        let req = j.get("request").cloned().unwrap_or(serde_json::Value::Null);
                        let eg = j.get(crate::geo::EDGE_GEO_FIELD).or_else(|| req.get(crate::geo::EDGE_GEO_FIELD)).cloned();
                        let mut req = req;
                        if let Some(o) = req.as_object_mut() { o.remove(crate::geo::EDGE_GEO_FIELD); }
                        match submit(&app, Some(peer), &headers, None, &req, Via::Ws(eg.as_ref())) {
                            Ok(a) => serde_json::json!({"op":"actionResult","id":id,"ack":a.to_json()}),
                            Err(r) => {
                                let body = axum::body::to_bytes(r.into_body(), 64 * 1024).await.unwrap_or_default();
                                let e: serde_json::Value = serde_json::from_slice(&body).unwrap_or(serde_json::json!({}));
                                serde_json::json!({"op":"error","id":id,"error":e["error"]})
                            }
                        }
                    }
                    _ => serde_json::json!({"op":"error","id":id,"error":{"code":"MALFORMED","message":"unknown op","details":{}}}),
                };
                if sock.send(Message::Text(reply.to_string().into())).await.is_err() { return; }
            }
            b = rx.recv() => {
                let info = match b { Ok(i) => i, Err(broadcast::error::RecvError::Lagged(_)) => {
                    let _ = sock.send(Message::Text(serde_json::json!({"op":"error","error":{"code":"SLOW_CONSUMER","message":"fell behind","details":{}}}).to_string().into())).await;
                    return;
                } Err(_) => return };
                if status_sub {
                    let fin = app.seq.lock().unwrap().latest_final();
                    let lf = fin.and_then(|h| app.seq.lock().unwrap().block(h).ok().flatten()).map(|b| block_summary(&b));
                    let m = serde_json::json!({"channel":"status","type":"update","data":{"latestBlock": block_summary(&info.block), "latestFinalized": lf}});
                    if sock.send(Message::Text(m.to_string().into())).await.is_err() { return; }
                }
                if !accounts.is_empty() {
                    let inputs = decode_inputs(&app.g.types, &info.block.inputs_encoded).unwrap_or_default();
                    let touched: Vec<Address> = accounts.iter().copied().filter(|a| inputs.iter().any(|i| match i {
                        crate::types::Input::Deposit(d) => d.account == *a,
                        crate::types::Input::Forced(f) => f.account == *a,
                        crate::types::Input::Action(x) => crate::actions::field_addr(&app.g, app.g.types.name(x.type_index).unwrap_or(""), &x.message, "account") == Some(*a),
                        _ => false,
                    })).collect();
                    for a in touched {
                        let acks: Vec<serde_json::Value> = { let s = app.seq.lock().unwrap(); s.acks.values().filter(|k| k.account == a && k.height == Some(info.block.header.height)).map(|k| k.to_json()).collect() };
                        let data = { let s = app.seq.lock().unwrap(); serde_json::json!({"height": info.block.header.height, "time": info.block.header.time, "account": account_shape(s.engine.account_json(&a)), "orders": s.engine.orders_json(&a)["orders"], "actions": acks}) };
                        let m = serde_json::json!({"channel":"account","type":"update","account":to_hex(&a),"data":data});
                        if sock.send(Message::Text(m.to_string().into())).await.is_err() { return; }
                    }
                }
            }
            _ = ping.tick() => {
                if last_in.elapsed() > Duration::from_secs(60) { let _ = sock.send(Message::Close(None)).await; return; }
                if sock.send(Message::Ping(Vec::new().into())).await.is_err() { return; }
            }
        }
    }
}

use tokio::sync::broadcast;

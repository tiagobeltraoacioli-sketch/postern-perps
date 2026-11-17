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
        .route("/v1/withdrawals/{id}", get(withdrawal))
        .route("/v1/deposits/{tx}", get(deposits_by_tx))
        .route("/v1/forced", get(forced))
        .route("/v1/geo", get(geo))
        .route("/v1/ws", get(ws))
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
    Json(serde_json::json!({
        "name": "postern-perps-node", "version": env!("CARGO_PKG_VERSION"), "apiVersion": API_VERSION,
        "mode": app.mode.name(), "unaudited": true, "engine": s.engine.name(),
        "network": {"eip712": app.g.engine_domain.to_json(), "collateralChainId": 700770, "vault": to_hex(&app.g.vault),
            "collateralToken": "TBD-deploy", "collateralSymbol": "bUSDC", "collateralDecimals": 6},
        "time": now_ms(),
        "latestBlock": latest.as_ref().map(block_summary),
        "latestFinalized": fin.as_ref().map(block_summary),
        "sequencer": {"state": if s.halted.is_some() {"halted"} else {"running"}, "blockIntervalMs": app.block_interval_ms,
            "pendingActions": s.mempool_len(), "haltReason": s.halted},
        "committee": {"n": app.g.committee.len(), "threshold": app.g.threshold, "online": online},
        "validators": vals.values().map(|x| serde_json::json!({"address": x.address, "height": x.height, "signed": x.signed, "last": x.last, "latched": x.latched})).collect::<Vec<_>>(),
        "vaultMode": "unknown",
        "l2": {"configured": app.l2_configured, "finalizedBlock": v.finalized_block, "scannedTo": v.scanned_to,
            "lastDepositIndex": v.deposits.keys().next_back(), "endpointsAgree": v.endpoints_agree, "lastError": v.last_error},
        "recovery": {"snapshotHeight": s.recovered.snapshot_height, "replayedBlocks": s.recovered.replayed_blocks,
            "tornTailBytes": s.recovered.torn_tail_bytes, "checkpointsLoaded": s.recovered.checkpoints_loaded},
        "genesisLogHash": to_hex(&app.g.genesis_log_hash()),
    }))
    .into_response()
}

async fn eip712(State(app): State<Arc<App>>) -> Response {
    let td = &app.g.typed_data_json;
    Json(serde_json::json!({
        "domain": app.g.engine_domain.to_json(), "types": td["types"], "actions": app.g.types.actions,
        "agentAllowed": app.g.types.agent_allowed, "enums": td.get("enums"), "specVersion": td.get("specVersion"),
        "typedDataHash": to_hex(&app.g.typed_data_hash),
    }))
    .into_response()
}

fn market_json(app: &App, m: &crate::genesis::Market) -> serde_json::Value {
    let mut j = m.json.clone();
    if let Some(o) = j.as_object_mut() {
        o.retain(|k, _| !k.starts_with("mock"));
        o.insert("status".into(), "active".into());
        o.insert("quote".into(), "USD".into());
    }
    let _ = app;
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
    serde_json::json!({"symbol": m.symbol, "markPrice": null, "indexPrice": null, "lastPrice": null,
        "height": s.engine.height(), "time": s.engine.time(), "engine": s.engine.name(),
        "note": "the node stub engine has no mark, funding or trades"})
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

async fn book(State(app): State<Arc<App>>, Path(sym): Path<String>) -> Response {
    if app.g.market_by_symbol(&sym).is_none() {
        return not_found("unknown market");
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
async fn empty_list_candles(State(app): State<Arc<App>>, Path(sym): Path<String>) -> Response {
    if app.g.market_by_symbol(&sym).is_none() {
        return not_found("unknown market");
    }
    Json(serde_json::json!({"candles": []})).into_response()
}
async fn empty_list_funding(State(app): State<Arc<App>>, Path(sym): Path<String>) -> Response {
    if app.g.market_by_symbol(&sym).is_none() {
        return not_found("unknown market");
    }
    Json(serde_json::json!({"funding": []})).into_response()
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
        "inputs": inputs.iter().map(|i| input_json(&app.g.types, i)).collect::<Vec<_>>(),
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
            body.push_str(&block_json(&app, &b, true).to_string());
            body.push('\n');
        }
    }
    ([("content-type", "application/x-ndjson")], body).into_response()
}

/// Admission path shared by REST and WebSocket: parse, recover the signer, geoblock, then the sequencer.
pub fn submit(app: &App, peer: Option<SocketAddr>, headers: &HeaderMap, query_country: Option<&str>, req: &serde_json::Value) -> Result<crate::sequencer::Ack, Response> {
    let now = now_ms();
    if let Some(p) = peer {
        if !app.ip_rate.lock().unwrap().allow(&ip_key(p.ip()), now) {
            return Err(error(429, "RATE_LIMITED", "per-IP action rate", serde_json::json!({})));
        }
    }
    let a = parse_request(&app.g, req).map_err(|e| error(400, "MALFORMED", &e, serde_json::json!({})))?;
    let v = verify_signed_action(&app.g, &a).map_err(|e| error(400, "BAD_SIGNATURE", &e.to_string(), serde_json::json!({})))?;
    let (country, sub) = app.geo.country(peer.map(|p| p.ip()), headers, query_country);
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
    body: axum::body::Bytes,
) -> Response {
    let req: serde_json::Value = match serde_json::from_slice(&body) {
        Ok(v) => v,
        Err(e) => return error(400, "MALFORMED", &format!("body is not JSON: {e}"), serde_json::json!({})),
    };
    let ack = match submit(&app, Some(peer), &headers, q.get("country").map(|s| s.as_str()), &req) {
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
    Json(app.seq.lock().unwrap().engine.account_json(&a)).into_response()
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

async fn account_nonce(State(app): State<Arc<App>>, Path(a): Path<String>) -> Response {
    let a = match addr_param(&a) {
        Ok(a) => a,
        Err(r) => return r,
    };
    let s = app.seq.lock().unwrap();
    let hi = s.engine.highest_nonce(&a);
    Json(serde_json::json!({"account": to_hex(&a), "highestUsed": hi.map(|x| x.to_string()),
        "nextNonce": hi.map(|x| (x + 1).to_string()).unwrap_or_else(|| "0".into()),
        "rule": "nonce = milliseconds clock, strictly above the highest used, within blockTime-120000..blockTime+30000"}))
    .into_response()
}

fn withdrawal_json(app: &App, id: u64) -> Option<serde_json::Value> {
    let s = app.seq.lock().unwrap();
    let rec = s.engine.withdrawal(id)?;
    let cert = s.certificate(id);
    let status = if cert.is_some() { "certified" } else { "included" };
    Some(serde_json::json!({
        "withdrawalId": id.to_string(), "account": to_hex(&rec.account), "recipient": to_hex(&rec.recipient),
        "amount": crate::util::scaled_to_decimal(rec.amount, 6), "fee": crate::util::scaled_to_decimal(rec.fee, 6),
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
    j["amount"] = serde_json::Value::String(crate::util::scaled_to_decimal(d.amount, 6));
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
                            "status" => { status_sub = true; serde_json::json!({"op":"subscribed","id":id,"channel":"status"}) }
                            "account" => match j.get("account").and_then(|v| v.as_str()).map(parse_address) {
                                Some(Ok(a)) => {
                                    if accounts.len() >= 100 { serde_json::json!({"op":"error","id":id,"error":{"code":"RATE_LIMITED","message":"subscriptions per connection","details":{}}}) }
                                    else {
                                        accounts.push(a);
                                        let snap = { let s = app.seq.lock().unwrap(); serde_json::json!({"account": s.engine.account_json(&a), "orders": s.engine.orders_json(&a)["orders"], "agents": s.engine.agents_json(&a)["agents"]}) };
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
                                    let data = match ch { "book" => serde_json::json!({"symbol":sym,"seq":0,"bids":[],"asks":[]}), "trades" => serde_json::json!({"trades":[]}), "candles" => serde_json::json!({"candles":[]}), _ => serde_json::json!({}) };
                                    serde_json::json!({"channel":ch,"symbol":sym,"type":"snapshot","data":data})
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
                        let req = j.get("request").cloned().unwrap_or(serde_json::Value::Null);
                        match submit(&app, Some(peer), &headers, None, &req) {
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
                    let m = serde_json::json!({"channel":"status","type":"update","data":{"latestBlock": block_summary(&info.block), "latestFinalizedHeight": fin}});
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
                        let data = { let s = app.seq.lock().unwrap(); serde_json::json!({"height": info.block.header.height, "time": info.block.header.time, "account": s.engine.account_json(&a), "orders": s.engine.orders_json(&a)["orders"], "actions": acks}) };
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

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
use crate::types::{decode_inputs, input_json};
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

pub const API_VERSION: &str = "0.3";

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
        .route("/v1/genesis", get(genesis_doc))
        .route("/v1/markets", get(markets))
        .route("/v1/markets/{symbol}", get(market))
        .route("/v1/tickers", get(tickers))
        .route("/v1/markets/{symbol}/ticker", get(ticker))
        .route("/v1/markets/{symbol}/book", get(book))
        .route("/v1/markets/{symbol}/trades", get(market_trades))
        .route("/v1/markets/{symbol}/candles", get(market_candles))
        .route("/v1/markets/{symbol}/funding", get(market_funding))
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
        .route("/v1/accounts/{address}/proof", get(account_proof))
        .route("/v1/withdrawals/{id}", get(withdrawal))
        .route("/v1/withdrawals/{id}/proof", get(withdrawal_proof))
        .route("/v1/orders/{id}", get(order_by_id))
        .route("/v1/oracle/observations", post(oracle_observation))
        .route("/v1/oracle/updates", post(oracle_update))
        .route("/v1/oracle/rounds/latest", get(oracle_rounds))
        .route("/v1/deposits/{tx}", get(deposits_by_tx))
        .route("/v1/forced", get(forced))
        .route("/v1/geo", get(geo))
        .route("/v1/ws", get(ws))
        .route("/v1/oracle/{symbol}", get(oracle_status))
        .fallback(|| async { not_found("unknown endpoint") })
        .layer(DefaultBodyLimit::max(16 * 1024))
        .layer(axum::middleware::from_fn_with_state(app.clone(), cors))
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
            "collateralToken": to_hex(&app.collateral_token), "collateralSymbol": "bUSDC", "collateralDecimals": 6},
        "time": now_ms(),
        "latestBlock": latest.as_ref().map(block_summary).unwrap_or(genesis_ref),
        "latestFinalized": fin.as_ref().map(block_summary),
        "sequencer": {"state": if s.halted.is_some() {"halted"} else {"running"}, "blockIntervalMs": app.block_interval_ms,
            "pendingActions": s.mempool_len()},
        "committee": {"n": app.g.committee.len(), "threshold": app.g.threshold, "online": online},
        "vaultMode": app.relay.lock().unwrap().vault_mode,
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
        "engine": s.engine.name(), "haltReason": s.halted,
        "index": s.index.as_ref().map(|ix| serde_json::json!({"fromHeight": ix.from_height, "height": ix.height, "mismatches": ix.mismatches,
            "lastMismatch": ix.last_mismatch, "nextTradeId": ix.next_trade_id.to_string(), "nextFillId": ix.next_fill_id.to_string()})),
        "oracleIngest": {"enabled": app.oracle_token.is_some(), "pool": s.oracle_pool.to_json(&app.g)},
        "proofFrames": {"held": s.frames.keys().collect::<Vec<_>>(), "pinned": s.pinned_frames.iter().collect::<Vec<_>>()},
        "relayer": app.relay.lock().unwrap().to_json(),
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

/// CORS (API.md section 1): every response carries the configured origin; a preflight is answered here. The API
/// has no cookies and no sessions, so the header grants nothing a non-browser client does not already have.
///
/// The same layer applies the per-IP READ limit of section 8 (R436-6: reads were unlimited): every GET except the
/// WebSocket upgrade takes a token of the caller's bucket; an empty bucket is `429 RATE_LIMITED`.
async fn cors(State(app): State<Arc<App>>, req: axum::extract::Request, next: axum::middleware::Next) -> Response {
    let origin = app.cors_origin.lock().unwrap().clone();
    let preflight = req.method() == axum::http::Method::OPTIONS;
    let limited = req.method() == axum::http::Method::GET
        && req.uri().path() != "/v1/ws"
        && req.extensions().get::<ConnectInfo<SocketAddr>>().is_some_and(|ci| !app.read_rate.lock().unwrap().allow(&ip_key(ci.0.ip()), now_ms()));
    let mut resp = if preflight {
        StatusCode::NO_CONTENT.into_response()
    } else if limited {
        error(429, "RATE_LIMITED", "per-IP read rate", serde_json::json!({}))
    } else {
        next.run(req).await
    };
    let h = resp.headers_mut();
    if let Ok(v) = HeaderValue::from_str(&origin) {
        h.insert("access-control-allow-origin", v);
    }
    h.insert("vary", HeaderValue::from_static("origin"));
    if preflight {
        h.insert("access-control-allow-methods", HeaderValue::from_static("GET, POST, OPTIONS"));
        h.insert("access-control-allow-headers", HeaderValue::from_static("content-type, x-postern-geo, x-postern-geo-mac, x-mock-country"));
        h.insert("access-control-max-age", HeaderValue::from_static("600"));
    }
    resp
}

/// R435-1: the engine's genesis document, byte for byte what `logHash(0)` commits to
/// (`keccak256("PosternPerps/genesis/v1" || document)`), so anyone can replay `/v1/blocks/stream` with the engine
/// CLI from published data only.
async fn genesis_doc(State(app): State<Arc<App>>) -> Response {
    let doc = app.seq.lock().unwrap().engine.genesis_document();
    match doc {
        Some(d) => ([("content-type", "application/json")], d).into_response(),
        None => not_found("the stub engine has no genesis document"),
    }
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

/// R393-N1: the published table IS the engine's (when the engine has one); the fixture only for the stub.
fn market_list(app: &App) -> Vec<serde_json::Value> {
    if let Some(t) = app.seq.lock().unwrap().engine.market_table(&app.g) {
        return t;
    }
    app.g.markets.iter().map(|m| market_json(app, m)).collect()
}

async fn markets(State(app): State<Arc<App>>) -> Response {
    Json(serde_json::json!({"markets": market_list(&app)})).into_response()
}

async fn market(State(app): State<Arc<App>>, Path(sym): Path<String>) -> Response {
    match market_list(&app).into_iter().find(|m| m["symbol"] == sym.as_str()) {
        Some(m) => Json(m).into_response(),
        None => not_found("unknown market"),
    }
}

fn ticker_json(app: &App, m: &crate::genesis::Market) -> serde_json::Value {
    let s = app.seq.lock().unwrap();
    if let Some(j) = s.ticker_json(m.market_id) {
        return j;
    }
    let t = s.engine.time();
    // the stub engine has no markets: zero values
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
    let depth: usize = q.get("depth").and_then(|d| d.parse().ok()).unwrap_or(50);
    let s = app.seq.lock().unwrap();
    Json(s.book_json(&sym, depth).unwrap_or(serde_json::Value::Null)).into_response()
}

/// `limit` (default 100, 1 to 1000) and `before` (exclusive cursor: a decimal id from `nextCursor`).
fn page(q: &HashMap<String, String>) -> Result<(usize, Option<u64>), Response> {
    let limit = match q.get("limit") {
        None => 100,
        Some(l) => match l.parse::<usize>() {
            Ok(n) if (1..=1000).contains(&n) => n,
            _ => return Err(error(400, "MALFORMED", "limit must be 1 to 1000", serde_json::json!({"path": "limit"}))),
        },
    };
    let before = match q.get("before") {
        None => None,
        Some(b) => match b.parse::<u64>() {
            Ok(n) => Some(n),
            Err(_) => return Err(error(400, "MALFORMED", "before must be a cursor taken from nextCursor", serde_json::json!({"path": "before"}))),
        },
    };
    Ok((limit, before))
}

fn with_cursor(key: &str, items: Vec<serde_json::Value>, cursor: Option<String>) -> Response {
    let mut j = serde_json::json!({ key: items });
    if let Some(c) = cursor {
        j["nextCursor"] = c.into();
    }
    Json(j).into_response()
}

/// `symbol` query filter -> market id; an unknown symbol is 404.
fn symbol_filter(app: &App, q: &HashMap<String, String>) -> Result<Option<u32>, Response> {
    match q.get("symbol") {
        None => Ok(None),
        Some(sym) => app.g.market_by_symbol(sym).map(|m| Some(m.market_id)).ok_or_else(|| not_found("unknown market")),
    }
}

async fn market_trades(State(app): State<Arc<App>>, Path(sym): Path<String>, q: Q) -> Response {
    let Some(m) = app.g.market_by_symbol(&sym) else { return not_found("unknown market") };
    let (limit, before) = match page(&q) {
        Ok(x) => x,
        Err(r) => return r,
    };
    let s = app.seq.lock().unwrap();
    let (items, cursor) = s.index.as_ref().map(|ix| ix.trades(m.market_id, limit, before)).unwrap_or_default();
    with_cursor("trades", items, cursor)
}

async fn market_candles(State(app): State<Arc<App>>, Path(sym): Path<String>, q: Q) -> Response {
    let Some(m) = app.g.market_by_symbol(&sym) else { return not_found("unknown market") };
    let Some(iv) = crate::index::interval_index(q.get("interval").map(|s| s.as_str()).unwrap_or("1m")) else {
        return error(400, "MALFORMED", "unknown interval (1m 5m 15m 1h 4h 1d)", serde_json::json!({"path": "interval"}));
    };
    let num = |k: &str| -> Result<Option<u64>, Response> {
        match q.get(k) {
            None => Ok(None),
            Some(v) => v.parse::<u64>().map(Some).map_err(|_| error(400, "MALFORMED", "from / to / limit are unsigned integers", serde_json::json!({"path": k}))),
        }
    };
    let (from, to, limit) = match (num("from"), num("to"), num("limit")) {
        (Ok(f), Ok(t), Ok(l)) => (f, t, l.unwrap_or(300)),
        (Err(r), _, _) | (_, Err(r), _) | (_, _, Err(r)) => return r,
    };
    if !(1..=1500).contains(&limit) {
        return error(400, "MALFORMED", "limit must be 1 to 1500", serde_json::json!({"path": "limit"}));
    }
    let s = app.seq.lock().unwrap();
    let items = s.index.as_ref().map(|ix| ix.candles(m.market_id, iv, from, to, limit as usize)).unwrap_or_default();
    Json(serde_json::json!({"candles": items})).into_response()
}

async fn market_funding(State(app): State<Arc<App>>, Path(sym): Path<String>, q: Q) -> Response {
    let Some(m) = app.g.market_by_symbol(&sym) else { return not_found("unknown market") };
    let (limit, before) = match page(&q) {
        Ok(x) => x,
        Err(r) => return r,
    };
    let s = app.seq.lock().unwrap();
    let (items, cursor) = s.index.as_ref().map(|ix| ix.funding_history(m.market_id, limit, before)).unwrap_or_default();
    with_cursor("funding", items, cursor)
}

async fn oracle_status(State(app): State<Arc<App>>, Path(sym): Path<String>) -> Response {
    let Some(m) = app.g.market_by_symbol(&sym) else { return not_found("unknown market") };
    let s = app.seq.lock().unwrap();
    if let Some(j) = s.engine.real().and_then(|e| crate::engine::view::oracle_json(e, m.market_id, app.g.oracle_quorum, s.oracle_pool.newest_held())) {
        return Json(j).into_response();
    }
    Json(serde_json::json!({"symbol": m.symbol, "status": "halted", "price": "0", "time": s.engine.time(),
        "round": s.engine.last_oracle_round().to_string(), "sources": [], "signers": 0, "threshold": app.g.oracle_quorum,
        "haltReason": "the stub engine has no oracle state"}))
    .into_response()
}

/// Operator channel (agent 432's feeder): the bearer token of `--oracle-token-file`. Without a configured token
/// the ingest paths do not exist for anyone (503); a wrong or missing token is 401. Never behind the geoblock.
fn oracle_auth(app: &App, headers: &HeaderMap) -> Result<(), Response> {
    let Some(tok) = &app.oracle_token else {
        return Err(error(503, "ORACLE_INGEST_DISABLED", "this node has no oracle ingest token configured (--oracle-token-file)", serde_json::json!({})));
    };
    let presented = headers.get("authorization").and_then(|v| v.to_str().ok()).and_then(|v| v.strip_prefix("Bearer "));
    match presented {
        Some(p) if crate::oracle_pool::token_ok(tok, p.as_bytes()) => Ok(()),
        _ => Err(error(401, "UNAUTHORIZED", "Authorization: Bearer <oracle ingest token>", serde_json::json!({}))),
    }
}

fn round_of(v: &serde_json::Value) -> Result<u64, Response> {
    match v {
        serde_json::Value::Number(n) => n.as_u64(),
        serde_json::Value::String(x) if x.bytes().all(|c| c.is_ascii_digit()) && !x.is_empty() => x.parse().ok(),
        _ => None,
    }
    .ok_or_else(|| error(400, "ORACLE_MALFORMED", "round: an unsigned integer", serde_json::json!({})))
}

fn offer_json(app: &App, round: u64, signer: u8, r: Result<crate::oracle_pool::Held, crate::oracle_pool::Refusal>) -> Result<serde_json::Value, Response> {
    use crate::oracle_pool::Held;
    match r {
        Ok(h) => {
            let (held, status) = match h {
                Held::Held(k) => (k, "held"),
                Held::Ready(k) => (k, "ready"),
            };
            Ok(serde_json::json!({"round": round.to_string(), "signer": signer, "held": held, "quorum": app.g.oracle_quorum, "status": status}))
        }
        Err(e) => Err(error(e.http, e.code, &e.message, serde_json::json!({"round": round.to_string(), "signer": signer}))),
    }
}

/// `{"round": R, "observation": {signer, timeMs, prices, signature}}`: ONE signer's observation.
async fn oracle_observation(State(app): State<Arc<App>>, headers: HeaderMap, body: axum::body::Bytes) -> Response {
    if let Err(r) = oracle_auth(&app, &headers) {
        return r;
    }
    let j: serde_json::Value = match serde_json::from_slice(&body) {
        Ok(j) => j,
        Err(e) => return error(400, "ORACLE_MALFORMED", &format!("body is not JSON: {e}"), serde_json::json!({})),
    };
    if j.as_object().is_none_or(|o| o.len() != 2 || !o.contains_key("round") || !o.contains_key("observation")) {
        return error(400, "ORACLE_MALFORMED", "body: exactly {round, observation}", serde_json::json!({}));
    }
    let round = match round_of(&j["round"]) {
        Ok(r) => r,
        Err(r) => return r,
    };
    let o = match crate::oracle_pool::parse_observation(&j["observation"]) {
        Ok(o) => o,
        Err(e) => return error(e.http, e.code, &e.message, serde_json::json!({})),
    };
    let signer = o.signer;
    let mut s = app.seq.lock().unwrap();
    let last = s.engine.last_oracle_round();
    let r = s.oracle_pool.offer(&app.g, round, o, last, now_ms());
    drop(s);
    match offer_json(&app, round, signer, r) {
        Ok(j) => Json(j).into_response(),
        Err(r) => r,
    }
}

/// `{"round": R, "observations": [...]}`: a whole update (a feeder that runs several signers, a replay source).
/// Each observation is offered exactly as through the single path; the answer lists each outcome, and the HTTP
/// status is 200 when at least one was held, else the first refusal's.
async fn oracle_update(State(app): State<Arc<App>>, headers: HeaderMap, body: axum::body::Bytes) -> Response {
    if let Err(r) = oracle_auth(&app, &headers) {
        return r;
    }
    let j: serde_json::Value = match serde_json::from_slice(&body) {
        Ok(j) => j,
        Err(e) => return error(400, "ORACLE_MALFORMED", &format!("body is not JSON: {e}"), serde_json::json!({})),
    };
    if j.as_object().is_none_or(|o| o.len() != 2 || !o.contains_key("round") || !o.contains_key("observations")) {
        return error(400, "ORACLE_MALFORMED", "body: exactly {round, observations}", serde_json::json!({}));
    }
    let round = match round_of(&j["round"]) {
        Ok(r) => r,
        Err(r) => return r,
    };
    let Some(arr) = j["observations"].as_array().filter(|a| !a.is_empty() && a.len() <= 255) else {
        return error(400, "ORACLE_MALFORMED", "observations: 1 to 255 entries", serde_json::json!({}));
    };
    let mut parsed = Vec::new();
    for v in arr {
        match crate::oracle_pool::parse_observation(v) {
            Ok(o) => parsed.push(o),
            Err(e) => return error(e.http, e.code, &e.message, serde_json::json!({})),
        }
    }
    let mut results = Vec::new();
    let mut first_refusal: Option<crate::oracle_pool::Refusal> = None;
    let mut any = false;
    let mut last_status = "held";
    let mut held = 0;
    {
        let mut s = app.seq.lock().unwrap();
        let last = s.engine.last_oracle_round();
        for o in parsed {
            let signer = o.signer;
            match s.oracle_pool.offer(&app.g, round, o, last, now_ms()) {
                Ok(h) => {
                    any = true;
                    let (k, st) = match h {
                        crate::oracle_pool::Held::Held(k) => (k, "held"),
                        crate::oracle_pool::Held::Ready(k) => (k, "ready"),
                    };
                    held = k;
                    last_status = st;
                    results.push(serde_json::json!({"signer": signer, "status": st}));
                }
                Err(e) => {
                    results.push(serde_json::json!({"signer": signer, "status": "refused", "code": e.code, "message": e.message}));
                    first_refusal.get_or_insert(e);
                }
            }
        }
    }
    if !any {
        let e = first_refusal.expect("at least one observation");
        return error(e.http, e.code, &e.message, serde_json::json!({"round": round.to_string(), "observations": results}));
    }
    Json(serde_json::json!({"round": round.to_string(), "held": held, "quorum": app.g.oracle_quorum, "status": last_status, "observations": results})).into_response()
}

async fn oracle_rounds(State(app): State<Arc<App>>) -> Response {
    let s = app.seq.lock().unwrap();
    let mut j = s.oracle_pool.to_json(&app.g);
    j["enabled"] = app.oracle_token.is_some().into();
    j["lastAcceptedRound"] = s.engine.last_oracle_round().to_string().into();
    Json(j).into_response()
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
    block_json_g(&app.g, b, raw)
}

/// API.md `Block` (also served by a validator's own read service, `crate::vserve`).
pub fn block_json_g(g: &crate::genesis::Genesis, b: &crate::types::Block, raw: bool) -> serde_json::Value {
    let inputs = decode_inputs(&g.types, &b.inputs_encoded).unwrap_or_default();
    let mut j = serde_json::json!({
        "height": b.header.height, "time": b.header.time, "prevLogHash": to_hex(&b.header.prev_log_hash),
        "logHash": to_hex(&b.header.log_hash), "stateRoot": to_hex(&b.header.state_root),
        "inputs": inputs.iter().map(|i| {
            let mut j = input_json(&g.types, i);
            if let crate::types::Input::Action(a) = i {
                if let Ok(v) = verify_signed_action(g, a) {
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

/// Blocks `from..from + limit` as stored (cloned under the sequencer lock; nothing else is done under it).
fn take_blocks(app: &App, from: u64, limit: u64) -> Vec<crate::types::Block> {
    let s = app.seq.lock().unwrap();
    let mut out = Vec::new();
    for h in from..from.saturating_add(limit) {
        match s.block(h) {
            Ok(Some(b)) => out.push(b),
            _ => break,
        }
    }
    out
}

/// R436-6: decoding the inputs and recovering every action's signer is done OUTSIDE the sequencer lock.
async fn blocks_range(State(app): State<Arc<App>>, q: Q) -> Response {
    let from: u64 = q.get("from").and_then(|s| s.parse().ok()).unwrap_or(1).max(1);
    let limit: u64 = q.get("limit").and_then(|s| s.parse().ok()).unwrap_or(100).clamp(1, 500);
    let raw = q.get("raw").is_some();
    let blocks = take_blocks(&app, from, limit);
    let out: Vec<serde_json::Value> = blocks.iter().map(|b| block_json(&app, b, raw)).collect();
    Json(serde_json::json!({"blocks": out})).into_response()
}

/// At most 10,001 blocks per request (continue with `from` = the last height + 1), read 200 at a time so the
/// sequencer lock is held only for the copies.
async fn blocks_stream(State(app): State<Arc<App>>, q: Q) -> Response {
    let from: u64 = q.get("from").and_then(|s| s.parse().ok()).unwrap_or(1).max(1);
    let raw = q.get("raw").is_some();
    let end = from.saturating_add(10_001);
    let mut body = String::new();
    let mut next = from;
    while next < end {
        let chunk = take_blocks(&app, next, (end - next).min(200));
        if chunk.is_empty() {
            break;
        }
        next += chunk.len() as u64;
        for b in &chunk {
            body.push_str(&block_json(&app, b, raw).to_string());
            body.push('\n');
        }
        tokio::task::yield_now().await;
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

async fn account(State(app): State<Arc<App>>, Path(a): Path<String>, q: Q) -> Response {
    let a = match addr_param(&a) {
        Ok(a) => a,
        Err(r) => return r,
    };
    let finalized = match q.get("finalized").map(|s| s.as_str()) {
        None | Some("false") => false,
        Some("true") => true,
        Some(_) => return error(400, "MALFORMED", "finalized must be true or false", serde_json::json!({"path": "finalized"})),
    };
    let r = app.seq.lock().unwrap().account_view(&a, finalized);
    match r {
        Ok(j) => Json(account_shape(j)).into_response(),
        Err(e) => not_found(&e),
    }
}

/// API.md `Account`: exactly the schema's fields (the stub engine's extra `engine` label is dropped; values the
/// stub does not have are zero).
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
        "feeTier": e.get("feeTier").cloned().unwrap_or(serde_json::json!({"tier": 0, "makerFeeBps": "1.5", "takerFeeBps": "4.5", "volume30d": "0"})),
        "positions": e.get("positions").cloned().unwrap_or(serde_json::json!([])),
        "openOrders": e.get("openOrders").cloned().unwrap_or(serde_json::json!(0)),
    })
}

/// Open orders of an account as API `Order` objects (index), or the stub's list.
fn open_orders(s: &crate::sequencer::Sequencer, a: &Address) -> serde_json::Value {
    match &s.index {
        Some(ix) => serde_json::Value::Array(ix.orders(a, true, None, 1000, None).0),
        None => s.engine.orders_json(a)["orders"].clone(),
    }
}

async fn account_orders(State(app): State<Arc<App>>, Path(a): Path<String>, q: Q) -> Response {
    let a = match addr_param(&a) {
        Ok(a) => a,
        Err(r) => return r,
    };
    let open_only = match q.get("status").map(|s| s.as_str()) {
        None | Some("open") => true,
        Some("all") => false,
        Some(_) => return error(400, "MALFORMED", "status must be open or all", serde_json::json!({"path": "status"})),
    };
    let (limit, before) = match page(&q) {
        Ok(x) => x,
        Err(r) => return r,
    };
    let market = match symbol_filter(&app, &q) {
        Ok(m) => m,
        Err(r) => return r,
    };
    let s = app.seq.lock().unwrap();
    match &s.index {
        Some(ix) => {
            let (items, cursor) = ix.orders(&a, open_only, market, limit, before);
            with_cursor("orders", items, cursor)
        }
        None => Json(s.engine.orders_json(&a)).into_response(),
    }
}

async fn order_by_id(State(app): State<Arc<App>>, Path(id): Path<u64>) -> Response {
    let s = app.seq.lock().unwrap();
    match s.index.as_ref().and_then(|ix| ix.order(id).map(|o| ix.order_json(o))) {
        Some(j) => Json(j).into_response(),
        None => not_found("unknown or pruned order"),
    }
}

async fn account_agents(State(app): State<Arc<App>>, Path(a): Path<String>) -> Response {
    let a = match addr_param(&a) {
        Ok(a) => a,
        Err(r) => return r,
    };
    Json(app.seq.lock().unwrap().agents_view(&a)).into_response()
}

async fn account_fills(State(app): State<Arc<App>>, Path(a): Path<String>, q: Q) -> Response {
    let a = match addr_param(&a) {
        Ok(a) => a,
        Err(r) => return r,
    };
    let (limit, before) = match page(&q) {
        Ok(x) => x,
        Err(r) => return r,
    };
    let market = match symbol_filter(&app, &q) {
        Ok(m) => m,
        Err(r) => return r,
    };
    let s = app.seq.lock().unwrap();
    let (items, cursor) = s.index.as_ref().map(|ix| ix.fills(&a, market, limit, before)).unwrap_or_default();
    with_cursor("fills", items, cursor)
}

async fn account_funding(State(app): State<Arc<App>>, Path(a): Path<String>, q: Q) -> Response {
    let a = match addr_param(&a) {
        Ok(a) => a,
        Err(r) => return r,
    };
    let (limit, before) = match page(&q) {
        Ok(x) => x,
        Err(r) => return r,
    };
    let market = match symbol_filter(&app, &q) {
        Ok(m) => m,
        Err(r) => return r,
    };
    let s = app.seq.lock().unwrap();
    let (items, cursor) = s.index.as_ref().map(|ix| ix.payments(&a, market, limit, before)).unwrap_or_default();
    with_cursor("payments", items, cursor)
}

/// `GET /v1/accounts/{address}/proof[?height=H]`: the `claimEscape` input against a FINAL checkpoint this node
/// still holds the state of (default: the latest final).
async fn account_proof(State(app): State<Arc<App>>, Path(a): Path<String>, q: Q) -> Response {
    let a = match addr_param(&a) {
        Ok(a) => a,
        Err(r) => return r,
    };
    let height = match q.get("height").map(|h| h.parse::<u64>()) {
        None => None,
        Some(Ok(h)) => Some(h),
        Some(Err(_)) => return error(400, "MALFORMED", "height", serde_json::json!({"path": "height"})),
    };
    // the engine state of that checkpoint is taken under the lock (an Arc clone); the proof (hashing every
    // account leaf) is built outside it (R436-6)
    // R436-11: a FINAL checkpoint whose state is no longer held is rebuilt from the log (the log holds every
    // block since genesis), one rebuild at a time, checked against the checkpoint's state root, then kept
    if let Some(h) = height {
        if app.seq.lock().unwrap().final_without_frame(h) {
            if let Err(e) = rebuild_frame(&app, h).await {
                return error(503, "SEQUENCER_UNAVAILABLE", &format!("the state at checkpoint {h} could not be rebuilt from the log: {e}"), serde_json::json!({"height": h}));
            }
        }
    }
    let parts = app.seq.lock().unwrap().account_proof_parts(height);
    match parts.and_then(|(e, root, cp)| crate::sequencer::build_account_proof(&e, &a, &root, cp)) {
        Ok(j) => Json(j).into_response(),
        Err(e) => not_found(&e),
    }
}

/// Rebuilds the engine state at final checkpoint `h` by replaying the log from the nearest held state.
pub async fn rebuild_frame(app: &Arc<App>, h: u64) -> Result<(), String> {
    let _one_at_a_time = app.rebuild_gate.lock().await;
    let (base_h, base, want_root) = {
        let s = app.seq.lock().unwrap();
        if !s.final_without_frame(h) {
            return Ok(()); // rebuilt meanwhile, or not final
        }
        let (bh, b) = s.rebuild_base(h).ok_or("this engine has no rebuildable state")?;
        (bh, b, s.checkpoint_for(h).map(|c| c.state_root()).ok_or("checkpoint vanished")?)
    };
    let app2 = app.clone();
    let built = tokio::task::spawn_blocking(move || {
        let take = |from: u64, limit: u64| take_blocks(&app2, from, limit);
        crate::archive::rebuild(&app2.g, base_h, base, h, &take)
    })
    .await
    .map_err(|e| e.to_string())??;
    if built.state_root() != want_root {
        return Err("the rebuilt state does not have the final checkpoint's state root".into());
    }
    let real = built.real().cloned().ok_or("not a real engine")?;
    app.seq.lock().unwrap().insert_rebuilt_frame(h, real);
    Ok(())
}

/// `GET /v1/withdrawals/{id}/proof[?count=N | ?height=H]`: the `withdraw(seq, w, siblings, want)` input for the
/// accepted checkpoint whose `withdrawalCountAt(seq)` is N (or the final checkpoint at height H; default: the
/// latest final).
async fn withdrawal_proof(State(app): State<Arc<App>>, Path(id): Path<u64>, q: Q) -> Response {
    let num = |k: &str| -> Result<Option<u64>, Response> {
        match q.get(k).map(|h| h.parse::<u64>()) {
            None => Ok(None),
            Some(Ok(h)) => Ok(Some(h)),
            Some(Err(_)) => Err(error(400, "MALFORMED", k, serde_json::json!({"path": k}))),
        }
    };
    let (count, height) = match (num("count"), num("height")) {
        (Ok(c), Ok(h)) => (c, h),
        (Err(r), _) | (_, Err(r)) => return r,
    };
    let r = app.seq.lock().unwrap().withdrawal_proof(id, count, height);
    match r {
        Ok(j) => Json(j).into_response(),
        Err(e) => not_found(&e),
    }
}

async fn account_nonce(State(app): State<Arc<App>>, Path(a): Path<String>) -> Response {
    let a = match addr_param(&a) {
        Ok(a) => a,
        Err(r) => return r,
    };
    let s = app.seq.lock().unwrap();
    let hi = s.engine.highest_nonce(&a);
    let agents: Vec<serde_json::Value> = s.agents_view(&a)["agents"]
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
    // API.md 4.1 certificate: the record, the 32 siblings against the newest final checkpoint that contains it,
    // that checkpoint and its signatures (`GET /v1/withdrawals/{id}/proof` serves any other accepted checkpoint)
    let cert = s.certificate(id).map(|c| {
        serde_json::json!({"digest": c["digest"], "record": c["record"], "checkpoint": c["checkpoint"],
            "proof": c["proof"]["siblings"], "signatures": c["signatures"]})
    });
    let relay = app.relay.lock().unwrap();
    let paid = relay.paid.get(&id).cloned();
    let status = match (&paid, &cert) {
        (Some(_), _) => "paid",
        (None, Some(_)) => "certified",
        (None, None) => "included",
    };
    let time = s.block(rec.height).ok().flatten().map(|b| b.header.time).unwrap_or(0);
    Some(serde_json::json!({
        "withdrawalId": id.to_string(), "account": to_hex(&rec.account), "recipient": to_hex(&rec.recipient),
        "amount": crate::util::scaled_to_decimal(rec.amount as u128, 6), "fee": crate::util::scaled_to_decimal(rec.fee as u128, 6),
        "status": status, "requestedHeight": rec.height,
        "checkpointHeight": cert.as_ref().and_then(|c| c["checkpoint"]["height"].as_u64()),
        "certificate": cert, "l2TxHash": paid, "availableAt": null, "time": time,
    }))
}

async fn withdrawal(State(app): State<Arc<App>>, Path(id): Path<u64>) -> Response {
    match withdrawal_json(&app, id) {
        Some(j) => Json(j).into_response(),
        None => not_found("unknown withdrawal"),
    }
}

/// Newest first, at most 1,000. The ids come from the per-account index and each certificate proof costs 32
/// hashes (R436-6: this used to scan every withdrawal and rebuild the whole tree per record).
async fn account_withdrawals(State(app): State<Arc<App>>, Path(a): Path<String>) -> Response {
    let a = match addr_param(&a) {
        Ok(a) => a,
        Err(r) => return r,
    };
    let ids: Vec<u64> = app.seq.lock().unwrap().wd_by_account.get(&a).map(|v| v.iter().rev().take(1000).copied().collect()).unwrap_or_default();
    let out: Vec<serde_json::Value> = ids.into_iter().filter_map(|id| withdrawal_json(&app, id)).collect();
    Json(serde_json::json!({"withdrawals": out})).into_response()
}

/// API.md `Deposit`: exactly the schema's fields. `time` is the engine time of the crediting block (0 until
/// credited: the L2 block's own timestamp is not part of the vault event).
fn deposit_status_json(app: &App, d: &crate::types::DepositEvent) -> serde_json::Value {
    let s = app.seq.lock().unwrap();
    let credited = d.index < s.engine.next_deposit_index();
    let ch = s.deposit_heights.get(&d.index).copied();
    let time = ch.and_then(|h| s.block(h).ok().flatten()).map(|b| b.header.time).unwrap_or(0);
    serde_json::json!({
        "depositId": d.index.to_string(), "account": to_hex(&d.account), "amount": crate::util::scaled_to_decimal(d.amount as u128, 6),
        "l2TxHash": to_hex(&d.l2_tx_hash), "logIndex": d.log_index, "l2Block": d.l2_block,
        "status": if credited { "credited" } else { "l2Finalized" }, "creditedHeight": ch, "time": time,
    })
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
    let (fs, l2_time_s): (Vec<_>, u64) = {
        let v = app.view.lock().unwrap();
        (v.forced.values().filter(|f| acc.is_none_or(|a| f.account == a)).cloned().collect(), v.l2_time_s)
    };
    let s = app.seq.lock().unwrap();
    let next = s.engine.next_forced_index();
    let out: Vec<_> = fs
        .iter()
        .map(|f| {
            // the engine's own verdict (SPEC 14): served, rejected (consumed, user-caused), pending (waits at the
            // head of the queue: the vault's deadline keeps running)
            let outcome = s.index.as_ref().and_then(|ix| ix.forced_outcome(f.index));
            let done = matches!(outcome, Some(("served", _, _)) | Some(("rejected", _, _)));
            let status = if !done && l2_time_s > f.deadline {
                "expired"
            } else if f.index < next {
                "included"
            } else {
                "queued"
            };
            serde_json::json!({
                "forcedId": f.index.to_string(), "account": to_hex(&f.account), "kind": f.kind.name(), "l2TxHash": to_hex(&f.l2_tx_hash),
                "l2Block": f.l2_block, "deadlineTime": f.deadline.saturating_mul(1000), "status": status,
                "includedHeight": s.forced_heights.get(&f.index).copied(),
                "arg": f.arg.to_string(), "outcome": outcome.map(|o| o.0), "reason": outcome.and_then(|o| o.1),
                "processedHeight": outcome.filter(|_| done).map(|o| o.2),
            })
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

fn ws_err(id: &serde_json::Value, code: &str, message: &str) -> serde_json::Value {
    serde_json::json!({"op":"error","id":id,"error":{"code":code,"message":message,"details":{}}})
}

/// The `AccountUpdate` of one block for one subscribed account (API.md section 9): the account, the orders whose
/// state changed, the block's fills and funding payments, deposits credited and withdrawals created, and the
/// acknowledgements of the account's actions.
fn account_update(app: &App, info: &crate::sequencer::BlockInfo, a: &Address, inputs: &[crate::types::Input]) -> serde_json::Value {
    let h = info.block.header.height;
    let deposits: Vec<crate::types::DepositEvent> = inputs.iter().filter_map(|i| match i { crate::types::Input::Deposit(d) if d.account == *a => Some(d.clone()), _ => None }).collect();
    let mut data = {
        let s = app.seq.lock().unwrap();
        let acks: Vec<serde_json::Value> = info.acks.iter().filter(|(x, _)| x == a).map(|(_, j)| j.clone()).collect();
        let mut data = serde_json::json!({"height": h, "time": info.block.header.time,
            "account": s.account_view(a, false).map(account_shape).unwrap_or(serde_json::Value::Null)});
        match (&info.delta, &s.index) {
            (Some(d), Some(ix)) => {
                let orders: Vec<serde_json::Value> = d.orders.iter().filter_map(|id| ix.order(*id)).filter(|o| o.account == *a).map(|o| ix.order_json(o)).collect();
                let fills: Vec<serde_json::Value> = d.fills.iter().filter(|(x, _)| x == a).map(|(_, f)| ix.fill_json(a, f)).collect();
                let pays: Vec<serde_json::Value> = d.payments.iter().filter(|(x, _)| x == a).map(|(_, p)| ix.payment_json(p)).collect();
                if !orders.is_empty() { data["orders"] = orders.into(); }
                if !fills.is_empty() { data["fills"] = fills.into(); }
                if !pays.is_empty() { data["funding"] = pays.into(); }
            }
            _ => data["orders"] = open_orders(&s, a),
        }
        if !acks.is_empty() { data["actions"] = acks.into(); }
        data
    };
    if !deposits.is_empty() {
        data["deposits"] = deposits.iter().map(|d| deposit_status_json(app, d)).collect::<Vec<_>>().into();
    }
    let wds: Vec<serde_json::Value> = info.outcome.withdrawals.iter().filter_map(|id| withdrawal_json(app, *id)).filter(|w| w["account"] == to_hex(a).as_str()).collect();
    if !wds.is_empty() {
        data["withdrawals"] = wds.into();
    }
    data
}

async fn ws_session(app: Arc<App>, mut sock: WebSocket, peer: SocketAddr, headers: HeaderMap) {
    let mut rx = app.blocks_tx.subscribe();
    let mut status_sub = false;
    let mut accounts: Vec<Address> = Vec::new();
    let mut ping = tokio::time::interval(Duration::from_secs(15));
    let mut last_in = tokio::time::Instant::now();
    let mut books: Vec<String> = Vec::new();
    let mut trades: Vec<u32> = Vec::new();
    let mut candles: Vec<(u32, usize)> = Vec::new();
    // ticker subscriptions: market id -> (last time a ticker was sent, a block happened since). At most one per
    // market per second; a change inside that second is sent when the second ends (`flush`), funding at once.
    let mut tickers: HashMap<u32, (Option<tokio::time::Instant>, bool)> = HashMap::new();
    let mut flush = tokio::time::interval(Duration::from_millis(200));
    flush.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    macro_rules! send {
        ($v:expr) => {
            if sock.send(Message::Text($v.to_string().into())).await.is_err() {
                return;
            }
        };
    }
    loop {
        tokio::select! {
            m = sock.recv() => {
                let Some(Ok(m)) = m else { return };
                last_in = tokio::time::Instant::now();
                let txt = match m { Message::Text(t) => t.to_string(), Message::Close(_) => return, _ => continue };
                let j: serde_json::Value = match serde_json::from_str(&txt) {
                    Ok(j) => j,
                    Err(_) => { send!(ws_err(&serde_json::Value::Null, "MALFORMED", "not JSON")); continue; }
                };
                let id = j.get("id").cloned().unwrap_or(serde_json::Value::Null);
                let subs = accounts.len() + books.len() + trades.len() + candles.len() + tickers.len() + status_sub as usize;
                match j.get("op").and_then(|v| v.as_str()) {
                    Some("ping") => send!(serde_json::json!({"op":"pong","time": now_ms()})),
                    Some("subscribe") => {
                        let ch = j.get("channel").and_then(|v| v.as_str()).unwrap_or("");
                        if subs >= 100 {
                            send!(ws_err(&id, "RATE_LIMITED", "subscriptions per connection"));
                            continue;
                        }
                        match ch {
                            "status" => {
                                status_sub = true;
                                send!(serde_json::json!({"op":"subscribed","id":id,"channel":"status"}));
                                let (lb, lf) = { let s = app.seq.lock().unwrap(); (s.block(s.log.last_height).ok().flatten().map(|b| block_summary(&b)), s.latest_final().and_then(|h| s.block(h).ok().flatten()).map(|b| block_summary(&b))) };
                                send!(serde_json::json!({"channel":"status","type":"update","data":{"latestBlock": lb, "latestFinalized": lf}}));
                            }
                            "account" => match j.get("account").and_then(|v| v.as_str()).map(parse_address) {
                                Some(Ok(a)) => {
                                    if !accounts.contains(&a) { accounts.push(a); }
                                    let snap = { let s = app.seq.lock().unwrap(); serde_json::json!({"account": s.account_view(&a, false).map(account_shape).unwrap_or(serde_json::Value::Null), "orders": open_orders(&s, &a), "agents": s.agents_view(&a)["agents"]}) };
                                    send!(serde_json::json!({"op":"subscribed","id":id,"channel":"account","account":to_hex(&a)}));
                                    send!(serde_json::json!({"channel":"account","type":"snapshot","account":to_hex(&a),"data":snap}));
                                }
                                _ => send!(ws_err(&id, "MALFORMED", "account")),
                            },
                            "book" | "trades" | "ticker" | "candles" => {
                                let sym = j.get("symbol").and_then(|v| v.as_str()).unwrap_or("");
                                let market = app.g.market_by_symbol(sym).map(|m| m.market_id);
                                if market.is_none() && !(ch == "ticker" && sym == "*") {
                                    send!(ws_err(&id, "NOT_FOUND", "unknown market"));
                                    continue;
                                }
                                match ch {
                                    "book" => {
                                        let depth = match j.get("depth") {
                                            None => 50,
                                            Some(d) => match d.as_u64() { Some(n) if (1..=500).contains(&n) => n as usize, _ => { send!(ws_err(&id, "MALFORMED", "depth must be 1 to 500")); continue; } },
                                        };
                                        if !books.iter().any(|b| b == sym) { books.push(sym.to_string()); }
                                        let data = app.seq.lock().unwrap().book_json(sym, depth).unwrap_or(serde_json::Value::Null);
                                        send!(serde_json::json!({"op":"subscribed","id":id,"channel":ch,"symbol":sym}));
                                        send!(serde_json::json!({"channel":ch,"symbol":sym,"type":"snapshot","data":data}));
                                    }
                                    "trades" => {
                                        let mid = market.unwrap();
                                        if !trades.contains(&mid) { trades.push(mid); }
                                        let items = app.seq.lock().unwrap().index.as_ref().map(|ix| ix.trades(mid, 50, None).0).unwrap_or_default();
                                        send!(serde_json::json!({"op":"subscribed","id":id,"channel":ch,"symbol":sym}));
                                        send!(serde_json::json!({"channel":ch,"symbol":sym,"type":"snapshot","data":{"trades": items}}));
                                    }
                                    "candles" => {
                                        let mid = market.unwrap();
                                        let name = j.get("interval").and_then(|v| v.as_str()).unwrap_or("1m");
                                        let Some(iv) = crate::index::interval_index(name) else { send!(ws_err(&id, "MALFORMED", "unknown interval (1m 5m 15m 1h 4h 1d)")); continue; };
                                        if !candles.contains(&(mid, iv)) { candles.push((mid, iv)); }
                                        let items = app.seq.lock().unwrap().index.as_ref().map(|ix| ix.candles(mid, iv, None, None, 300)).unwrap_or_default();
                                        send!(serde_json::json!({"op":"subscribed","id":id,"channel":ch,"symbol":sym,"interval":name}));
                                        send!(serde_json::json!({"channel":ch,"symbol":sym,"interval":name,"type":"snapshot","data":{"candles": items}}));
                                    }
                                    _ => {
                                        let ms: Vec<&crate::genesis::Market> = if sym == "*" { app.g.markets.iter().collect() } else { app.g.market_by_symbol(sym).into_iter().collect() };
                                        send!(serde_json::json!({"op":"subscribed","id":id,"channel":ch,"symbol":sym}));
                                        for m in ms {
                                            tickers.insert(m.market_id, (Some(tokio::time::Instant::now()), false));
                                            send!(serde_json::json!({"channel":"ticker","symbol":m.symbol,"type":"update","data":ticker_json(&app, m)}));
                                        }
                                    }
                                }
                            }
                            _ => send!(ws_err(&id, "MALFORMED", "unknown channel")),
                        }
                    }
                    Some("unsubscribe") => {
                        let ch = j.get("channel").and_then(|v| v.as_str()).unwrap_or("");
                        let sym = j.get("symbol").and_then(|v| v.as_str()).unwrap_or("");
                        let market = app.g.market_by_symbol(sym).map(|m| m.market_id);
                        match ch {
                            "status" => status_sub = false,
                            "account" => { if let Some(Ok(a)) = j.get("account").and_then(|v| v.as_str()).map(parse_address) { accounts.retain(|x| *x != a); } }
                            "book" => books.retain(|b| b != sym),
                            "trades" => trades.retain(|m| Some(*m) != market),
                            "candles" => {
                                let iv = j.get("interval").and_then(|v| v.as_str()).and_then(crate::index::interval_index);
                                candles.retain(|(m, i)| !(Some(*m) == market && (iv.is_none() || iv == Some(*i))));
                            }
                            "ticker" => { if sym == "*" { tickers.clear(); } else if let Some(m) = market { tickers.remove(&m); } }
                            _ => {}
                        }
                        send!(serde_json::json!({"op":"unsubscribed","id":id,"channel":ch}));
                    }
                    Some("action") => {
                        // the frame's own assertion; stripped before the request is parsed (never reaches the engine)
                        let req = j.get("request").cloned().unwrap_or(serde_json::Value::Null);
                        let eg = j.get(crate::geo::EDGE_GEO_FIELD).or_else(|| req.get(crate::geo::EDGE_GEO_FIELD)).cloned();
                        let mut req = req;
                        if let Some(o) = req.as_object_mut() { o.remove(crate::geo::EDGE_GEO_FIELD); }
                        let reply = match submit(&app, Some(peer), &headers, None, &req, Via::Ws(eg.as_ref())) {
                            Ok(a) => serde_json::json!({"op":"actionResult","id":id,"ack":a.to_json()}),
                            Err(r) => {
                                let body = axum::body::to_bytes(r.into_body(), 64 * 1024).await.unwrap_or_default();
                                let e: serde_json::Value = serde_json::from_slice(&body).unwrap_or(serde_json::json!({}));
                                serde_json::json!({"op":"error","id":id,"error":e["error"]})
                            }
                        };
                        send!(reply);
                    }
                    _ => send!(ws_err(&id, "MALFORMED", "unknown op")),
                }
            }
            b = rx.recv() => {
                let info = match b { Ok(i) => i, Err(broadcast::error::RecvError::Lagged(_)) => {
                    let _ = sock.send(Message::Text(serde_json::json!({"op":"error","error":{"code":"SLOW_CONSUMER","message":"fell behind","details":{}}}).to_string().into())).await;
                    return;
                } Err(_) => return };
                for d in &info.book_deltas {
                    if books.iter().any(|b| d["symbol"] == b.as_str()) {
                        send!(serde_json::json!({"channel":"book","symbol":d["symbol"],"type":"delta","data":d}));
                    }
                }
                if let Some(d) = &info.delta {
                    let mut out: Vec<serde_json::Value> = Vec::new();
                    {
                        let s = app.seq.lock().unwrap();
                        if let Some(ix) = &s.index {
                            for mid in &trades {
                                let ts: Vec<serde_json::Value> = d.trades.iter().filter(|t| t.market == *mid).map(|t| ix.trade_json(t)).collect();
                                if !ts.is_empty() {
                                    out.push(serde_json::json!({"channel":"trades","symbol":ts[0]["symbol"],"type":"update","data":{"trades": ts}}));
                                }
                            }
                            for (mid, iv) in &candles {
                                if d.candles.contains(&(*mid, *iv)) {
                                    if let (Some(c), Some(m)) = (ix.last_candle(*mid, *iv), app.g.market(*mid)) {
                                        out.push(serde_json::json!({"channel":"candles","symbol":m.symbol,"interval":crate::index::INTERVALS[*iv].0,"type":"update","data":c}));
                                    }
                                }
                            }
                        }
                    }
                    for m in out { send!(m); }
                }
                if !tickers.is_empty() {
                    let now = tokio::time::Instant::now();
                    let funded: Vec<u32> = info.delta.as_ref().map(|d| d.funding.iter().map(|f| f.market).collect()).unwrap_or_default();
                    for (_, v) in tickers.iter_mut() { v.1 = true; }
                    let due: Vec<u32> = tickers.iter().filter(|(m, (last, _))| funded.contains(m) || last.is_none_or(|t| now.duration_since(t) >= Duration::from_secs(1))).map(|(m, _)| *m).collect();
                    for mid in due {
                        if let Some(m) = app.g.market(mid) {
                            tickers.insert(mid, (Some(now), false));
                            send!(serde_json::json!({"channel":"ticker","symbol":m.symbol,"type":"update","data":ticker_json(&app, m)}));
                        }
                    }
                }
                if status_sub {
                    let lf = { let s = app.seq.lock().unwrap(); s.latest_final().and_then(|h| s.block(h).ok().flatten()).map(|b| block_summary(&b)) };
                    send!(serde_json::json!({"channel":"status","type":"update","data":{"latestBlock": block_summary(&info.block), "latestFinalized": lf}}));
                }
                if !accounts.is_empty() {
                    let inputs = decode_inputs(&app.g.types, &info.block.inputs_encoded).unwrap_or_default();
                    let touched: Vec<Address> = match &info.delta {
                        Some(d) => accounts.iter().copied().filter(|a| d.accounts.contains(a)).collect(),
                        None => accounts.iter().copied().filter(|a| inputs.iter().any(|i| match i {
                            crate::types::Input::Deposit(d) => d.account == *a,
                            crate::types::Input::Forced(f) => f.account == *a,
                            crate::types::Input::Action(x) => crate::actions::field_addr(&app.g, app.g.types.name(x.type_index).unwrap_or(""), &x.message, "account") == Some(*a),
                            _ => false,
                        })).collect(),
                    };
                    for a in touched {
                        let data = account_update(&app, &info, &a, &inputs);
                        send!(serde_json::json!({"channel":"account","type":"update","account":to_hex(&a),"data":data}));
                    }
                }
            }
            _ = flush.tick(), if tickers.values().any(|v| v.1) => {
                let now = tokio::time::Instant::now();
                let due: Vec<u32> = tickers.iter().filter(|(_, (last, pending))| *pending && last.is_none_or(|t| now.duration_since(t) >= Duration::from_secs(1))).map(|(m, _)| *m).collect();
                for mid in due {
                    if let Some(m) = app.g.market(mid) {
                        tickers.insert(mid, (Some(now), false));
                        send!(serde_json::json!({"channel":"ticker","symbol":m.symbol,"type":"update","data":ticker_json(&app, m)}));
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

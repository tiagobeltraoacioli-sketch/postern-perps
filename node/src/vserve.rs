//! A validator's OWN store and read service (R436-11 (b)).
//!
//! The escape hatch needs, for the vault's latest accepted checkpoint, each claimant's account leaf and 32
//! siblings; the siblings depend on every account's state at that height. Until round 2 the only source was the
//! sequencer's API, i.e. exactly the party whose silence the hatch exists for. Now every validator process:
//!
//! - keeps its own copy of the input log (`<data>/vlog/blocks.log`, the sequencer's log format, fsynced): only
//!   blocks its `Validator::process` accepted in full are appended;
//! - restarts from that log without the sequencer (`Validator::replay_own`), then follows the sequencer again;
//! - serves, from its own replay and on its own listener (`validator --listen`): the blocks
//!   (`/v1/blocks`, `/v1/blocks/stream`, `/v1/blocks/{h}`), the engine genesis (`/v1/genesis`), its status
//!   (`/v1/validator`), and `GET /v1/accounts/{address}/proof?height=H` for ANY height it has applied: from a held
//!   frame, else by rebuilding the state from its log (`crate::archive`).
//!
//! Bounds of the service (R436-14): reads are rate-limited per client address (`read_rate`, 20 per second, burst
//! 40); `/v1/blocks/stream` is produced as it is read, two at once, one per client address, slow consumers
//! disconnected (`api::ndjson_block_stream`); a state rebuild is done ONCE: requests for a height being rebuilt
//! wait for it (one rebuild at a time), the rebuilt state is kept (the newest `REBUILT_KEEP`), and a rebuild starts
//! from the nearest state held: a checkpoint frame, a kept rebuild, or an ANCHOR (the state at every
//! `ANCHOR_EVERY`-th block, the newest `ANCHORS_KEEP`), so it replays at most `ANCHOR_EVERY` blocks while the
//! height is inside the anchored range, instead of the whole log on every request.
//!
//! A proof from a validator carries the roots it computed (`stateRoot`, `accountsRoot`) and, when the height is a
//! checkpoint it signed, the checkpoint body and its own signature. It is not a certificate: the claimant checks
//! `accountsRoot` against the root the VAULT holds (`latest()` on the L2), which is what `claimEscape` verifies.
//!
//! UNAUDITED PROTOTYPE.

use crate::checkpoint::CheckpointBody;
use crate::engine::Engine;
use crate::genesis::Genesis;
use crate::log::InputLog;
use crate::types::Block;
use crate::util::{parse_address, to_hex};
use axum::extract::{Path, Query, State};
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use axum::{Json, Router};
use postern_perps_engine as pe;
use std::collections::{BTreeMap, HashMap};
use std::sync::{Arc, Mutex};

/// Engine states kept at the newest checkpoint heights (each is a whole state; older heights are rebuilt).
pub const VALIDATOR_FRAMES: usize = 8;
pub const VALIDATOR_BODIES: usize = 10_000;
pub const REBUILT_KEEP: usize = 4;
pub const ANCHOR_EVERY: u64 = 1_000;
pub const ANCHORS_KEEP: usize = 16;
pub const VALIDATOR_STREAMS: usize = 2;

pub struct ValidatorStore {
    pub g: Arc<Genesis>,
    pub log: InputLog,
    pub genesis_engine: Box<dyn Engine>,
    pub frames: BTreeMap<u64, Arc<pe::Engine>>,
    pub frames_keep: usize,
    /// states rebuilt for proof requests, oldest first (the newest `REBUILT_KEEP`)
    pub rebuilt: std::collections::VecDeque<(u64, Arc<pe::Engine>)>,
    /// the state at every `anchor_every`-th applied block (the newest `ANCHORS_KEEP`)
    pub anchors: BTreeMap<u64, Arc<pe::Engine>>,
    pub anchor_every: u64,
    /// checkpoint height -> the body this validator computed and its own signature
    pub bodies: BTreeMap<u64, (CheckpointBody, [u8; 65])>,
    pub address: String,
    pub height: u64,
    pub signed: u64,
    pub latched: Option<String>,
    pub last: String,
    /// blocks re-applied from the own log at start
    pub replayed_own: u64,
    /// state rebuilds from the log done for proof requests, and the blocks they replayed in all
    pub rebuilds: u64,
    pub rebuild_blocks: u64,
    /// per-client-address limit of this service's reads (R436-14)
    pub read_rate: crate::sequencer::RateLimit,
}

pub type SharedStore = Arc<Mutex<ValidatorStore>>;

impl ValidatorStore {
    /// Opens (or creates) the validator's own log under `<data>/vlog` (fully re-verified on open).
    pub fn open(g: Arc<Genesis>, data_dir: &std::path::Path, genesis_engine: Box<dyn Engine>, address: String) -> Result<ValidatorStore, String> {
        let (log, _) = InputLog::open(&data_dir.join("vlog"), &g)?;
        Ok(ValidatorStore { g, log, genesis_engine, frames: BTreeMap::new(), frames_keep: VALIDATOR_FRAMES, rebuilt: Default::default(), anchors: BTreeMap::new(), anchor_every: ANCHOR_EVERY, bodies: BTreeMap::new(), address, height: 0, signed: 0, latched: None, last: String::new(), replayed_own: 0, rebuilds: 0, rebuild_blocks: 0, read_rate: crate::sequencer::RateLimit::new(20.0, 40.0) })
    }

    /// Records a block this validator applied (appended to the own log, fsynced) and, at a checkpoint it signed,
    /// the body, the signature and the state.
    pub fn applied(&mut self, b: &Block, checkpoint: Option<&(CheckpointBody, [u8; 65])>, state: Option<&pe::Engine>) -> Result<(), String> {
        if b.header.height == self.log.last_height + 1 {
            self.log.append(&self.g, b)?;
        }
        if let Some(e) = state {
            self.anchor(b.header.height, e);
        }
        if let (Some((body, sig)), Some(e)) = (checkpoint, state) {
            self.bodies.insert(body.height, (body.clone(), *sig));
            while self.bodies.len() > VALIDATOR_BODIES {
                let k = *self.bodies.keys().next().unwrap();
                self.bodies.remove(&k);
            }
            self.frames.insert(body.height, Arc::new(e.clone()));
            while self.frames.len() > self.frames_keep.max(1) {
                let k = *self.frames.keys().next().unwrap();
                self.frames.remove(&k);
            }
        }
        Ok(())
    }

    /// Keeps the state at an anchor height (every `anchor_every`-th block): the base of later rebuilds.
    pub fn anchor(&mut self, height: u64, e: &pe::Engine) {
        if self.anchor_every == 0 || height % self.anchor_every != 0 {
            return;
        }
        self.anchors.insert(height, Arc::new(e.clone()));
        while self.anchors.len() > ANCHORS_KEEP {
            let k = *self.anchors.keys().next().unwrap();
            self.anchors.remove(&k);
        }
    }

    /// The state held for exactly `height`, if any (checkpoint frame, kept rebuild, anchor).
    pub fn held(&self, height: u64) -> Option<Arc<pe::Engine>> {
        self.frames.get(&height).or_else(|| self.anchors.get(&height)).cloned().or_else(|| self.rebuilt.iter().find(|(h, _)| *h == height).map(|(_, e)| e.clone()))
    }

    /// The nearest state held at or below `height`.
    pub fn nearest(&self, height: u64) -> Option<(u64, Arc<pe::Engine>)> {
        let a = self.frames.range(..=height).next_back().map(|(h, e)| (*h, e.clone()));
        let b = self.anchors.range(..=height).next_back().map(|(h, e)| (*h, e.clone()));
        let c = self.rebuilt.iter().filter(|(h, _)| *h <= height).max_by_key(|(h, _)| *h).cloned();
        [a, b, c].into_iter().flatten().max_by_key(|(h, _)| *h)
    }

    fn take_blocks(&self, from: u64, limit: u64) -> Vec<Block> {
        let mut out = Vec::new();
        for h in from..from.saturating_add(limit) {
            match self.log.read(h) {
                Ok(Some(b)) => out.push(b),
                _ => break,
            }
        }
        out
    }
}

fn err(status: u16, code: &str, message: &str) -> Response {
    crate::api::error(status, code, message, serde_json::json!({}))
}

type Q = Query<HashMap<String, String>>;

/// The service's state: the store, the gate that makes rebuilds one at a time, the stream limits.
#[derive(Clone)]
pub struct VState {
    pub store: SharedStore,
    gate: Arc<tokio::sync::Mutex<()>>,
    streams: Arc<crate::api::StreamLimits>,
}

/// The read service. Serve it with `into_make_service_with_connect_info::<SocketAddr>()`: the limits are per
/// client address (without it every caller shares one bucket and one stream slot).
pub fn router(store: SharedStore) -> Router {
    let vs = VState { store, gate: Arc::new(tokio::sync::Mutex::new(())), streams: Arc::new(crate::api::StreamLimits::new(VALIDATOR_STREAMS)) };
    Router::new()
        .route("/v1/validator", get(status))
        .route("/v1/genesis", get(genesis))
        .route("/v1/blocks", get(blocks))
        .route("/v1/blocks/stream", get(blocks_stream))
        .route("/v1/blocks/{height}", get(block_at))
        .route("/v1/accounts/{address}/proof", get(account_proof))
        .fallback(|| async { err(404, "NOT_FOUND", "a validator serves /v1/validator, /v1/genesis, /v1/blocks, /v1/accounts/{address}/proof") })
        .layer(axum::middleware::from_fn_with_state(vs.clone(), limit))
        .with_state(vs)
}

fn peer_ip(req: &axum::extract::Request) -> std::net::IpAddr {
    req.extensions().get::<axum::extract::ConnectInfo<std::net::SocketAddr>>().map(|c| c.0.ip()).unwrap_or(std::net::IpAddr::V4(std::net::Ipv4Addr::UNSPECIFIED))
}

/// R436-14: every read of the service takes a token of the caller's bucket; an empty bucket is 429.
async fn limit(State(vs): State<VState>, req: axum::extract::Request, next: axum::middleware::Next) -> Response {
    let ip = peer_ip(&req);
    if !vs.store.lock().unwrap().read_rate.allow(&crate::node::ip_key(ip), crate::node::now_ms()) {
        return err(429, "RATE_LIMITED", "per-address read rate of this validator's service");
    }
    next.run(req).await
}

async fn status(State(vs): State<VState>) -> Response {
    let s = vs.store.lock().unwrap();
    Json(serde_json::json!({"role": "validator", "unaudited": true, "address": s.address, "height": s.height, "logHeight": s.log.last_height,
        "signed": s.signed, "latched": s.latched, "last": s.last, "replayedOwnLog": s.replayed_own, "rebuilds": s.rebuilds,
        "frames": s.frames.keys().collect::<Vec<_>>(), "anchors": s.anchors.keys().collect::<Vec<_>>(),
        "rebuilt": s.rebuilt.iter().map(|x| x.0).collect::<Vec<_>>(), "rebuildBlocks": s.rebuild_blocks, "checkpointsSigned": s.bodies.len(),
        "genesisLogHash": to_hex(&s.g.genesis_log_hash())}))
    .into_response()
}

async fn genesis(State(vs): State<VState>) -> Response {
    match vs.store.lock().unwrap().genesis_engine.genesis_document() {
        Some(d) => ([("content-type", "application/json")], d).into_response(),
        None => err(404, "NOT_FOUND", "no genesis document"),
    }
}

async fn block_at(State(vs): State<VState>, Path(h): Path<u64>, q: Q) -> Response {
    let (g, b) = {
        let s = vs.store.lock().unwrap();
        (s.g.clone(), s.log.read(h))
    };
    match b {
        Ok(Some(b)) => Json(crate::api::block_json_g(&g, &b, q.get("raw").is_some())).into_response(),
        _ => err(404, "NOT_FOUND", "no block at this height in this validator's log"),
    }
}

async fn blocks(State(vs): State<VState>, q: Q) -> Response {
    let from: u64 = q.get("from").and_then(|s| s.parse().ok()).unwrap_or(1).max(1);
    let limit: u64 = q.get("limit").and_then(|s| s.parse().ok()).unwrap_or(100).clamp(1, 500);
    let (g, bs) = {
        let s = vs.store.lock().unwrap();
        (s.g.clone(), s.take_blocks(from, limit))
    };
    let raw = q.get("raw").is_some();
    Json(serde_json::json!({"blocks": bs.iter().map(|b| crate::api::block_json_g(&g, b, raw)).collect::<Vec<_>>()})).into_response()
}

async fn blocks_stream(State(vs): State<VState>, req: axum::extract::Request) -> Response {
    let q: HashMap<String, String> = req.uri().query().map(|q| q.split('&').filter_map(|kv| kv.split_once('=').or(Some((kv, "")))).map(|(k, v)| (k.to_string(), v.to_string())).collect()).unwrap_or_default();
    let from: u64 = q.get("from").and_then(|s| s.parse().ok()).unwrap_or(1).max(1);
    let raw = q.contains_key("raw");
    let g = vs.store.lock().unwrap().g.clone();
    let st = vs.store.clone();
    crate::api::ndjson_block_stream(&vs.streams, crate::api::ClientKey::peer(peer_ip(&req)), g, from, raw, Arc::new(move |f, n| st.lock().unwrap().take_blocks(f, n)))
}

/// The engine state at `height` from this validator's own replay: a held state, else rebuilt ONCE from the
/// nearest held state (requests for the same height wait for that one rebuild) and kept.
pub async fn state_at(vs: &VState, height: u64) -> Result<Arc<pe::Engine>, String> {
    if let Some(e) = vs.store.lock().unwrap().held(height) {
        return Ok(e);
    }
    let _one_at_a_time = vs.gate.lock().await;
    let (g, base_h, base) = {
        let s = vs.store.lock().unwrap();
        // built by the request that held the gate before this one
        if let Some(e) = s.held(height) {
            return Ok(e);
        }
        if height == 0 || height > s.log.last_height {
            return Err(format!("this validator has applied blocks 1..={}", s.log.last_height));
        }
        let base = s.nearest(height).and_then(|(h, e)| s.genesis_engine.with_real_state((*e).clone()).map(|b| (h, b)));
        let (bh, b) = base.unwrap_or_else(|| (0, s.genesis_engine.clone_box()));
        (s.g.clone(), bh, b)
    };
    let st2 = vs.store.clone();
    let built = tokio::task::spawn_blocking(move || {
        let take = |from: u64, limit: u64| st2.lock().unwrap().take_blocks(from, limit);
        crate::archive::rebuild(&g, base_h, base, height, &take)
    })
    .await
    .map_err(|e| e.to_string())??;
    let e = built.real().cloned().map(Arc::new).ok_or_else(|| "this validator does not run the real engine".to_string())?;
    let mut s = vs.store.lock().unwrap();
    s.rebuilds += 1;
    s.rebuild_blocks += height - base_h;
    s.rebuilt.push_back((height, e.clone()));
    while s.rebuilt.len() > REBUILT_KEEP {
        s.rebuilt.pop_front();
    }
    Ok(e)
}

async fn account_proof(State(vs): State<VState>, Path(a): Path<String>, q: Q) -> Response {
    let st = &vs.store;
    let Ok(addr) = parse_address(&a) else { return err(400, "MALFORMED", "address") };
    let height = match q.get("height").map(|h| h.parse::<u64>()) {
        Some(Ok(h)) => h,
        Some(Err(_)) => return err(400, "MALFORMED", "height"),
        None => match st.lock().unwrap().bodies.keys().next_back() {
            Some(h) => *h,
            None => return err(404, "NOT_FOUND", "this validator has signed no checkpoint yet: pass ?height=H"),
        },
    };
    let e = match state_at(&vs, height).await {
        Ok(e) => e,
        Err(m) => return err(404, "NOT_FOUND", &m),
    };
    let accounts_root = pe::merkle::root(&e.account_leaves(), pe::engine::ACCOUNT_DEPTH);
    let cp = {
        let s = st.lock().unwrap();
        s.bodies.get(&height).map(|(body, sig)| {
            let mut j = body.to_json();
            j["digest"] = to_hex(&body.digest(&s.g.vault_domain)).into();
            j["threshold"] = s.g.threshold.into();
            j["final"] = false.into();
            j["blockHash"] = to_hex(&body.log_hash).into();
            j["signatures"] = serde_json::json!([{"validator": s.address, "signature": to_hex(sig)}]);
            j
        })
    };
    match crate::sequencer::build_account_proof(&e, &addr, &accounts_root, cp.unwrap_or(serde_json::Value::Null)) {
        Ok(mut j) => {
            j["source"] = "validator".into();
            j["height"] = height.into();
            j["accountsRoot"] = to_hex(&accounts_root).into();
            j["stateRoot"] = to_hex(&e.state_root()).into();
            Json(j).into_response()
        }
        Err(m) => err(404, "NOT_FOUND", &m),
    }
}

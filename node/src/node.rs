//! Wiring: the shared application state, the block-production loop, in-process validators (dev mode), the
//! remote validator client, and the L2 watcher tasks.
//!
//! UNAUDITED PROTOTYPE.

use crate::engine::Engine;
use crate::genesis::Genesis;
use crate::geo::GeoPolicy;
use crate::sequencer::{BlockInfo, RateLimit, Sequencer};
use crate::validator::{VResult, Validator};
use crate::watcher::SharedView;
use std::collections::BTreeMap;
use std::net::IpAddr;
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::sync::{broadcast, Notify};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Mode {
    Dev,
    Live,
}

impl Mode {
    pub fn name(&self) -> &'static str {
        match self {
            Mode::Dev => "dev",
            Mode::Live => "live",
        }
    }
}

#[derive(Clone, Debug, Default)]
pub struct ValidatorStatus {
    pub address: String,
    pub height: u64,
    pub signed: u64,
    pub last: String,
    pub latched: Option<String>,
}

pub struct App {
    pub g: Arc<Genesis>,
    pub seq: Mutex<Sequencer>,
    pub geo: GeoPolicy,
    pub mode: Mode,
    pub view: SharedView,
    pub new_block: Notify,
    pub blocks_tx: broadcast::Sender<Arc<BlockInfo>>,
    pub ip_rate: Mutex<RateLimit>,
    /// Per-IP limit of REST reads (API.md section 8: 100 per second, burst 200; `--read-rate`, 0 = off)
    pub read_rate: Mutex<RateLimit>,
    pub validators: Mutex<BTreeMap<String, ValidatorStatus>>,
    pub block_interval_ms: u64,
    pub started_ms: u64,
    pub l2_configured: bool,
    /// The collateral token (bUSDC) address of the deployment (display only; zero when not configured).
    pub collateral_token: crate::util::Address,
    /// Bearer token of the oracle ingest paths (`--oracle-token-file`); `None` = ingest disabled.
    pub oracle_token: Option<Vec<u8>>,
    /// Limits of `/v1/blocks/stream` (R436-14)
    pub streams: Arc<crate::api::StreamLimits>,
    /// `GET /v1/blocks` is rendered off the runtime, a few at a time, up to a byte bound (R436-14 residue)
    pub block_renders: crate::api::BlockRenders,
    /// used attestation read signatures (R436-17 residue)
    pub read_replay: Mutex<crate::attest::ReadReplay>,
    /// `--edge-client-header`: the header in which the EDGE names the client's address. Believed only on a
    /// request that carries a valid edge assertion (R436-14 residue); None = never.
    pub edge_client_header: Mutex<Option<String>>,
    /// the engine clock (the wall clock unless a dev stack was started with a dev clock offset)
    pub clock: Mutex<EngineClock>,
    /// Bearer token of the operator's reads (`--operator-token-file`; `None`: no operator path exists)
    pub operator_token: Mutex<Option<Vec<u8>>>,
    /// Attestation records (`crate::attest`)
    pub attestations: Mutex<crate::attest::Attestations>,
    /// One state rebuild from the log at a time (`api::rebuild_frame`)
    pub rebuild_gate: tokio::sync::Mutex<()>,
    /// F434-2: the origins a browser page may call this API from (`--cors-origin`, repeatable). EMPTY by default:
    /// no CORS header is sent and no preflight answered. Exact match only; never `*`, never a reflected origin.
    pub cors_origins: Mutex<Vec<String>>,
    /// The checkpoint relayer's view of the vault (`crate::relayer`). Lock order: `seq` before `relay`.
    pub relay: Mutex<crate::relayer::RelayState>,
}

pub fn now_ms() -> u64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_millis() as u64).unwrap_or(0)
}

#[cfg(all(feature = "dev-clock", not(debug_assertions)))]
compile_error!("the dev-clock feature is DEV ONLY and must never be built into a release (no debug assertions)");

/// The largest dev clock offset (400 days: far more than any rehearsal needs, far from any overflow).
pub const DEV_CLOCK_MAX_OFFSET_MS: u64 = 400 * 86_400_000;

/// The ENGINE clock: the time block production, the validators' block-time rules (R395-29, R395-37/-38), the
/// oracle ingest and the nonce window are judged against. It is the wall clock, always, except in a DEV stack
/// built with the `dev-clock` feature and started with `--dev-clock-offset-ms`, where it is the wall clock plus a
/// fixed, forward offset. The offset is private and can only be set through `dev_offset`, which holds the gate.
/// Everything that is not engine time (edge assertions, attestations, rate limits, alarms) stays on `now_ms`.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct EngineClock {
    offset_ms: u64,
}

impl EngineClock {
    /// The wall clock (every non-dev configuration).
    pub fn wall() -> EngineClock {
        EngineClock { offset_ms: 0 }
    }
    pub fn now(&self) -> u64 {
        now_ms().saturating_add(self.offset_ms)
    }
    pub fn offset_ms(&self) -> u64 {
        self.offset_ms
    }
    /// Start-time gate of the dev clock (the R393 gate, as for the dev oracle): a non-zero offset exists only in a
    /// binary built with the `dev-clock` feature (which a release build refuses to compile) AND in dev mode.
    pub fn dev_offset(mode: Mode, offset_ms: u64) -> Result<EngineClock, String> {
        if offset_ms == 0 {
            return Ok(EngineClock::wall());
        }
        #[cfg(not(feature = "dev-clock"))]
        {
            let _ = mode;
            Err("a dev clock offset is refused: this binary is built without the dev-clock feature".into())
        }
        #[cfg(feature = "dev-clock")]
        {
            if mode != Mode::Dev {
                return Err("a dev clock offset is refused outside dev mode".into());
            }
            if offset_ms > DEV_CLOCK_MAX_OFFSET_MS {
                return Err(format!("a dev clock offset above {DEV_CLOCK_MAX_OFFSET_MS} ms is refused"));
            }
            Ok(EngineClock { offset_ms })
        }
    }
}

pub fn ip_key(ip: IpAddr) -> [u8; 20] {
    let mut k = [0u8; 20];
    match ip {
        IpAddr::V4(v) => k[..4].copy_from_slice(&v.octets()),
        IpAddr::V6(v) => k[..16].copy_from_slice(&v.octets()),
    }
    k[19] = 0xee;
    k
}

impl App {
    pub fn new(g: Arc<Genesis>, seq: Sequencer, geo: GeoPolicy, mode: Mode, view: SharedView, block_interval_ms: u64, l2_configured: bool) -> Arc<App> {
        Self::with(g, seq, geo, mode, view, block_interval_ms, l2_configured, [0u8; 20], None)
    }

    #[allow(clippy::too_many_arguments)]
    pub fn with(g: Arc<Genesis>, seq: Sequencer, geo: GeoPolicy, mode: Mode, view: SharedView, block_interval_ms: u64, l2_configured: bool, collateral_token: crate::util::Address, oracle_token: Option<Vec<u8>>) -> Arc<App> {
        let (tx, _) = broadcast::channel(4096);
        Arc::new(App {
            collateral_token,
            oracle_token,
            relay: Mutex::new(crate::relayer::RelayState::default()),
            cors_origins: Mutex::new(Vec::new()),
            rebuild_gate: tokio::sync::Mutex::new(()),
            attestations: Mutex::new(crate::attest::Attestations::default()),
            operator_token: Mutex::new(None),
            streams: Arc::new(crate::api::StreamLimits::new(crate::api::STREAM_SLOTS)),
            block_renders: crate::api::BlockRenders::new(),
            read_replay: Mutex::new(Default::default()),
            edge_client_header: Mutex::new(None),
            clock: Mutex::new(EngineClock::wall()),
            g,
            seq: Mutex::new(seq),
            geo,
            mode,
            view,
            new_block: Notify::new(),
            blocks_tx: tx,
            ip_rate: Mutex::new(RateLimit::new(50.0, 100.0)),
            read_rate: Mutex::new(RateLimit::new(100.0, 200.0)),
            validators: Mutex::new(BTreeMap::new()),
            block_interval_ms,
            started_ms: now_ms(),
            l2_configured,
        })
    }

    /// Engine time now (`EngineClock`).
    pub fn engine_now(&self) -> u64 {
        self.clock.lock().unwrap().now()
    }

    /// One production step (also used directly by tests).
    pub fn produce_once(&self) -> Result<Option<Arc<BlockInfo>>, String> {
        let now = self.engine_now();
        let r = self.seq.lock().unwrap().produce(now);
        if let Ok(Some(info)) = &r {
            let _ = self.blocks_tx.send(info.clone());
            self.new_block.notify_waiters();
        }
        r
    }
}

pub async fn run_producer(app: Arc<App>, mut stop: tokio::sync::watch::Receiver<bool>) {
    let mut iv = tokio::time::interval(Duration::from_millis(app.block_interval_ms));
    iv.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    loop {
        tokio::select! {
            _ = iv.tick() => {}
            _ = stop.changed() => return,
        }
        if let Err(e) = app.produce_once() {
            eprintln!("producer: {e}");
        }
    }
}

/// An in-process validator (dev mode): own engine, own L2 view, reads blocks from the sequencer's log.
pub async fn run_local_validator(app: Arc<App>, mut v: Validator, mut stop: tokio::sync::watch::Receiver<bool>) {
    let name = crate::util::to_hex(&v.address);
    loop {
        if *stop.borrow() {
            return;
        }
        let next = v.height + 1;
        let b = app.seq.lock().unwrap().block(next);
        let b = match b {
            Ok(Some(b)) => b,
            _ => {
                tokio::select! {
                    _ = app.new_block.notified() => {}
                    _ = tokio::time::sleep(Duration::from_millis(200)) => {}
                    _ = stop.changed() => return,
                }
                continue;
            }
        };
        // the in-process validators share the sequencer's engine clock (one process, one configuration)
        let r = v.process(&b, app.engine_now());
        let last = match &r {
            VResult::Applied { checkpoint: Some((body, sig)) } => {
                let res = app.seq.lock().unwrap().add_signature(body.height, *sig);
                match res {
                    Ok(_) => format!("signed {}", body.height),
                    Err(e) => format!("signature at {} not accepted: {e}", body.height),
                }
            }
            VResult::Applied { checkpoint: None } => format!("applied {next}"),
            VResult::AppliedNotSigned(s) => s.clone(),
            VResult::Wait(s) => {
                tokio::time::sleep(Duration::from_millis(100)).await;
                s.clone()
            }
            VResult::Refused { reason, .. } => reason.clone(),
        };
        {
            let mut st = app.validators.lock().unwrap();
            st.insert(
                name.clone(),
                ValidatorStatus { address: name.clone(), height: v.height, signed: v.signed_count, last, latched: v.latched.clone() },
            );
        }
        if let VResult::Refused { latched: true, .. } = r {
            // latched: stay alive for status, sign nothing
            let _ = stop.changed().await;
            return;
        }
    }
}

/// Spawns `n` in-process validators with keys from `keys`, each with its own engine and the given views.
pub fn spawn_local_validators(
    app: &Arc<App>,
    keys: Vec<k256::ecdsa::SigningKey>,
    engines: Vec<Box<dyn Engine>>,
    views: Vec<SharedView>,
    stop: &tokio::sync::watch::Receiver<bool>,
) -> Result<Vec<tokio::task::JoinHandle<()>>, String> {
    let mut hs = Vec::new();
    for ((k, e), view) in keys.into_iter().zip(engines).zip(views) {
        let v = Validator::new(app.g.clone(), k, e, view, None)?;
        hs.push(tokio::spawn(run_local_validator(app.clone(), v, stop.clone())));
    }
    Ok(hs)
}

/// A validator in its own process (dev mode): pulls blocks from the sequencer's public API (`/v1/blocks?raw=1`),
/// replays them with its own engine and its own L2 view, and posts its checkpoint signatures to
/// `POST /v1/finality/signatures`. It trusts the sequencer's API for nothing but bytes: every block is verified
/// exactly as the in-process validators do (`Validator::process`).
pub async fn run_remote_validator(base: String, v: Validator, stop: tokio::sync::watch::Receiver<bool>) -> Result<(), String> {
    run_remote_validator_with(base, v, stop, None).await
}

/// As `run_remote_validator`, with the validator's own store (`crate::vserve`): at start the blocks of its own log
/// are re-applied without the sequencer (`Validator::replay_own`), and every block it applies afterwards is
/// appended to that log, so its read service can serve blocks and account proofs from its own replay (R436-11).
pub async fn run_remote_validator_with(base: String, v: Validator, stop: tokio::sync::watch::Receiver<bool>, store: Option<crate::vserve::SharedStore>) -> Result<(), String> {
    run_remote_validator_clocked(base, v, stop, store, EngineClock::wall()).await
}

/// The dev clock offset the sequencer at `base` announces in `GET /v1/node` (`engineClock.devOffsetMs`; a node
/// that names none has none). None while the sequencer cannot be asked.
pub async fn announced_clock_offset(base: &str) -> Option<u64> {
    match crate::watcher::http_call("GET", &format!("{base}/v1/node"), None, &[]).await {
        Ok((200, body)) => serde_json::from_slice::<serde_json::Value>(&body).ok().map(|j| j["engineClock"]["devOffsetMs"].as_u64().unwrap_or(0)),
        _ => None,
    }
}

/// As `run_remote_validator_with`, on the engine clock `clock`. The validator's block-time rules (not too far
/// ahead, not first seen too late) are applied to ITS OWN clock. Before it processes any block it compares its
/// clock offset with the one the sequencer announces: a difference is a configuration error of a dev stack and
/// ends the validator (it signs nothing); and whatever is announced, a block whose time does not fit this
/// validator's clock is refused or left unsigned by the unchanged rules.
pub async fn run_remote_validator_clocked(base: String, mut v: Validator, mut stop: tokio::sync::watch::Receiver<bool>, store: Option<crate::vserve::SharedStore>, clock: EngineClock) -> Result<(), String> {
    use crate::watcher::http_call;
    loop {
        if *stop.borrow() {
            return Ok(());
        }
        match announced_clock_offset(&base).await {
            Some(theirs) if theirs == clock.offset_ms() => break,
            Some(theirs) => {
                return Err(format!(
                    "ENGINE CLOCK MISMATCH: the sequencer runs with a dev clock offset of {theirs} ms, this validator with {} ms: not following, nothing is signed (start both with the same --dev-clock-offset-ms, or neither)",
                    clock.offset_ms()
                ));
            }
            None => {
                tokio::select! {
                    _ = tokio::time::sleep(Duration::from_millis(200)) => {}
                    _ = stop.changed() => return Ok(()),
                }
            }
        }
    }
    if let Some(st) = &store {
        let last = st.lock().unwrap().log.last_height;
        for h in 1..=last {
            let b = st.lock().unwrap().log.read(h)?.ok_or("own log read")?;
            v.replay_own(&b)?;
            if let Some(e) = v.engine.real() {
                st.lock().unwrap().anchor(h, e);
            }
        }
        let mut s = st.lock().unwrap();
        s.replayed_own = last;
        s.height = v.height;
        s.last = format!("replayed {last} blocks from the own log");
    }
    let note = |v: &Validator, last: String| {
        if let Some(st) = &store {
            let mut s = st.lock().unwrap();
            s.height = v.height;
            s.signed = v.signed_count;
            s.latched = v.latched.clone();
            s.last = last;
        }
    };
    let mut clock_checked = std::time::Instant::now();
    loop {
        if *stop.borrow() {
            return Ok(());
        }
        // the sequencer may have been restarted with another offset: ask again every few seconds
        if clock_checked.elapsed() >= Duration::from_secs(3) {
            clock_checked = std::time::Instant::now();
            if let Some(theirs) = announced_clock_offset(&base).await {
                if theirs != clock.offset_ms() {
                    return Err(format!("ENGINE CLOCK MISMATCH: the sequencer now runs with a dev clock offset of {theirs} ms, this validator with {} ms: stopped, nothing more is signed", clock.offset_ms()));
                }
            }
        }
        let from = v.height + 1;
        let r = http_call("GET", &format!("{base}/v1/blocks?from={from}&limit=200&raw=1"), None, &[]).await;
        let blocks: Vec<crate::types::Block> = match r {
            Ok((200, body)) => {
                let j: serde_json::Value = serde_json::from_slice(&body).map_err(|e| e.to_string())?;
                j["blocks"]
                    .as_array()
                    .map(|a| a.iter().filter_map(|b| b["raw"].as_str().and_then(|h| crate::util::parse_hex_vec(h).ok())).filter_map(|raw| crate::types::Block::from_bytes(&raw).ok()).collect())
                    .unwrap_or_default()
            }
            _ => Vec::new(),
        };
        if blocks.is_empty() {
            tokio::select! {
                _ = tokio::time::sleep(Duration::from_millis(200)) => {}
                _ = stop.changed() => return Ok(()),
            }
            continue;
        }
        for b in blocks {
            let r = v.process(&b, clock.now());
            // a block this validator applied goes into its own log (with the checkpoint it signed, if any)
            if let (Some(st), VResult::Applied { .. } | VResult::AppliedNotSigned(_)) = (&store, &r) {
                let cp = match &r {
                    VResult::Applied { checkpoint: Some(c) } => Some(c),
                    _ => None,
                };
                st.lock().unwrap().applied(&b, cp, v.engine.real())?;
            }
            match r {
                VResult::Applied { checkpoint: Some((body, sig)) } => {
                    let req = serde_json::json!({"height": body.height, "signature": crate::util::to_hex(&sig)}).to_string();
                    let _ = http_call("POST", &format!("{base}/v1/finality/signatures"), Some(&req), &[]).await;
                    note(&v, format!("signed {}", body.height));
                }
                VResult::Applied { checkpoint: None } => note(&v, format!("applied {}", b.header.height)),
                VResult::AppliedNotSigned(s) => note(&v, s),
                VResult::Wait(s) => {
                    note(&v, s);
                    tokio::time::sleep(Duration::from_millis(100)).await;
                    break;
                }
                VResult::Refused { reason, latched } => {
                    note(&v, reason.clone());
                    if latched {
                        return Err(format!("validator latched: {reason}"));
                    }
                    break;
                }
            }
        }
    }
}

#[cfg(all(feature = "dev-oracle", not(debug_assertions)))]
compile_error!("the dev-oracle feature is DEV ONLY and must never be built into a release (no debug assertions)");

/// Start-time gate of the dev oracle (independent of the cargo feature): dev mode only, and only when the genesis
/// oracle committee is EXACTLY the throwaway committee these keys form (a real committee configuration refuses).
pub fn dev_oracle_allowed(mode: Mode, g: &crate::genesis::Genesis, keys: &[k256::ecdsa::SigningKey]) -> Result<(), String> {
    if mode != Mode::Dev {
        return Err("the dev oracle is refused outside dev mode".into());
    }
    let mut addrs: Vec<crate::util::Address> = keys.iter().map(crate::sig::key_address).collect();
    addrs.sort();
    if addrs != g.oracle_committee {
        return Err("the dev oracle's throwaway keys are not the genesis oracle committee: refused (a real committee is configured)".into());
    }
    Ok(())
}

/// Builds one oracle round signed by every given (THROWAWAY) oracle key: the observation of agent 394's format,
/// digest from the ENGINE crate's own `oracle_struct_hash` under the node's engine domain.
#[cfg(feature = "dev-oracle")]
pub fn dev_oracle_update(g: &crate::genesis::Genesis, keys: &[k256::ecdsa::SigningKey], t_ms: u64, prices: &[(u32, u64)]) -> crate::types::OracleUpdate {
    use postern_perps_engine as pe;
    let round = t_ms / 3000;
    let time_ms = round * 3000 + (t_ms % 3000).min(1);
    let pe_prices: Vec<pe::OraclePrice> = prices.iter().map(|(m, p)| pe::OraclePrice { market_id: *m, price: *p, sources: 5 }).collect();
    let d = pe::eip712::digest(&g.engine_domain.separator(), &pe::eip712::oracle_struct_hash(round, time_ms, &pe_prices));
    let observations = keys
        .iter()
        .enumerate()
        .map(|(i, k)| crate::types::SignedObservation {
            signer: i as u8,
            time_ms,
            prices: prices.iter().map(|(m, p)| crate::types::OraclePrice { market_id: *m, price: *p, sources: 5 }).collect(),
            signature: crate::sig::sign(k, &d),
        })
        .collect();
    crate::types::OracleUpdate { round, observations }
}

/// DEV ONLY: a stand-in oracle committee (throwaway keys, FIXED prices) so a dev node's markets are active and
/// its book moves; agent 394's feeder is the real source. Not a price source of any kind.
#[cfg(feature = "dev-oracle")]
pub fn start_dev_oracle(app: Arc<App>, keys: Vec<k256::ecdsa::SigningKey>, prices: Vec<(u32, u64)>, stop: tokio::sync::watch::Receiver<bool>) -> Result<tokio::task::JoinHandle<()>, String> {
    dev_oracle_allowed(app.mode, &app.g, &keys)?;
    Ok(tokio::spawn(run_dev_oracle(app, keys, prices, stop)))
}

#[cfg(feature = "dev-oracle")]
async fn run_dev_oracle(app: Arc<App>, keys: Vec<k256::ecdsa::SigningKey>, prices: Vec<(u32, u64)>, mut stop: tokio::sync::watch::Receiver<bool>) {
    let mut iv = tokio::time::interval(Duration::from_millis(3000));
    loop {
        tokio::select! {
            _ = iv.tick() => {}
            _ = stop.changed() => return,
        }
        let u = dev_oracle_update(&app.g, &keys, app.engine_now(), &prices);
        let _ = app.seq.lock().unwrap().submit_oracle(u);
    }
}

//! Round 2 (agent 431): the node against a REAL JSON-RPC implementation and the REAL PerpsVault bytecode.
//!
//! One scenario on a private anvil (chain id 700770, loopback, THROWAWAY keys, finality depth 2) with agent 433's
//! local deployment (`contracts/deploy/local.sh`: mock bUSDC + PerpsVault, committee = this test's node committee):
//!
//!  A. L2 watcher, finality: a deposit in a block that is not final is not seen; two blocks later it is, with the
//!     head the VAULT computed (`depositHead(1)`), and the engine credits it.
//!  B. reorg: a deposit in an unfinalized block that is reorganised away (snapshot / revert) is never seen and
//!     never credited.
//!  C. two-endpoint agreement: endpoint B is a pass-through proxy that can lie (an altered log, another hash for
//!     the finalized block): while it lies the watcher makes no progress and counts the disagreement; honest
//!     again, it advances.
//!  D. checkpoint relayer (VAULT.md 8a): a withdrawal makes the newest final checkpoint due; the relayer submits
//!     it with its gas-only key (EIP-155 legacy transaction, padded gas); the vault accepts it; the withdrawal is
//!     then PAID by the vault against `GET /v1/withdrawals/{id}/proof?count=` (record + 32 siblings).
//!  E. escape hatch: the L2 clock moves past `escapeDelay`; `triggerEscape`; the node's account proofs for
//!     INSURANCE and BACKSTOP satisfy `proveSystemShares`, and the user's proof `claimEscape`: the vault pays the
//!     engine's escape value to the unit.
//!
//! Needs anvil, cast, forge and python3: run on the build host with
//!   PERPS_ANVIL_DIR=~/.foundry/bin PERPS_CONTRACTS_DIR=<checkout>/contracts PERPS_ANVIL_PORT=53110 \
//!     cargo test --test anvil -- --ignored --nocapture
//! UNAUDITED PROTOTYPE.

mod common;

use axum::extract::State;
use axum::routing::post;
use axum::{Json, Router};
use common::*;
use perps_node::dev::{dev_genesis_real_with, DevOptions};
use perps_node::engine::real::RealEngine;
use perps_node::engine::Engine;
use perps_node::geo::GeoPolicy;
use perps_node::node::{run_producer, spawn_local_validators, App, Mode};
use perps_node::sequencer::Sequencer;
use perps_node::sig::{key_address, throwaway_key};
use perps_node::util::{parse_address, to_hex, Address};
use perps_node::watcher::{poll_once, rpc, WatcherConfig};
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::sync::{Arc, Mutex};
use std::time::Duration;

struct Anvil {
    child: Child,
    url: String,
}

impl Drop for Anvil {
    fn drop(&mut self) {
        // only the process this test started
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

struct Tools {
    dir: PathBuf,
    rpc: String,
    from: String,
}

impl Tools {
    fn cast(&self, args: &[&str]) -> String {
        let out = Command::new(self.dir.join("cast")).args(args).arg("--rpc-url").arg(&self.rpc).output().expect("cast");
        assert!(out.status.success(), "cast {args:?} failed: {}", String::from_utf8_lossy(&out.stderr));
        String::from_utf8_lossy(&out.stdout).trim().to_string()
    }
    /// A transaction from anvil's unlocked account 0; returns whether it succeeded and the output.
    fn try_send(&self, to: &str, sig: &str, args: &[&str]) -> (bool, String) {
        let mut a = vec!["send", to, sig];
        a.extend_from_slice(args);
        a.extend_from_slice(&["--from", &self.from, "--unlocked", "--gas-limit", "3000000"]);
        let out = Command::new(self.dir.join("cast")).args(&a).arg("--rpc-url").arg(&self.rpc).output().expect("cast");
        let text = format!("{}{}", String::from_utf8_lossy(&out.stdout), String::from_utf8_lossy(&out.stderr));
        // with an explicit gas limit cast does not simulate: a reverted transaction is mined and reported with
        // status 0, and cast still exits 0
        let mined_ok = text.lines().any(|l| l.starts_with("status") && l.split_whitespace().nth(1) == Some("1"));
        (out.status.success() && mined_ok, text)
    }
    fn send(&self, to: &str, sig: &str, args: &[&str]) {
        let (ok, out) = self.try_send(to, sig, args);
        assert!(ok, "cast send {sig} {args:?} failed: {out}");
    }
    fn call(&self, to: &str, sig: &str, args: &[&str]) -> String {
        let mut a = vec!["call", to, sig];
        a.extend_from_slice(args);
        self.cast(&a)
    }
    /// The first whitespace-separated token of a `cast call` answer as an integer (cast prints `123 [1.23e2]`).
    fn call_u(&self, to: &str, sig: &str, args: &[&str]) -> u128 {
        self.call(to, sig, args).split_whitespace().next().unwrap().parse().unwrap()
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Lie {
    Honest,
    /// one byte of every log's data altered
    AlterLogs,
    /// another hash for every block asked by tag or number
    AlterBlockHash,
}

#[derive(Clone)]
struct Proxy {
    upstream: String,
    lie: Arc<Mutex<Lie>>,
}

/// Endpoint B: forwards every JSON-RPC request to anvil and, when told to, lies about the answer.
async fn proxy_rpc(State(p): State<Proxy>, Json(req): Json<serde_json::Value>) -> Json<serde_json::Value> {
    let method = req["method"].as_str().unwrap_or("").to_string();
    let mut result = match rpc(&p.upstream, &method, req["params"].clone()).await {
        Ok(r) => r,
        Err(e) => return Json(serde_json::json!({"jsonrpc": "2.0", "id": req["id"], "error": {"code": -32000, "message": e}})),
    };
    let lie = *p.lie.lock().unwrap();
    if lie == Lie::AlterLogs && method == "eth_getLogs" {
        if let Some(logs) = result.as_array_mut() {
            for l in logs {
                if let Some(d) = l["data"].as_str() {
                    let mut s = d.to_string();
                    let last = s.pop().unwrap();
                    s.push(if last == '0' { '1' } else { '0' });
                    l["data"] = s.into();
                }
            }
        }
    }
    if lie == Lie::AlterBlockHash && method == "eth_getBlockByNumber" && result.is_object() {
        result["hash"] = to_hex(&[0xee; 32]).into();
    }
    Json(serde_json::json!({"jsonrpc": "2.0", "id": req["id"], "result": result}))
}

async fn wait_until(what: &str, ms: u64, mut f: impl FnMut() -> bool) {
    let end = tokio::time::Instant::now() + Duration::from_millis(ms);
    while !f() {
        assert!(tokio::time::Instant::now() < end, "timed out waiting for: {what}");
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

fn tuple_list(v: &serde_json::Value) -> String {
    format!("[{}]", v.as_array().unwrap().iter().map(|x| x.as_str().unwrap().to_string()).collect::<Vec<_>>().join(","))
}

/// `(account,index,balance,escapeValue,positionsHash)` of an `AccountProof.leafData` for cast.
fn account_tuple(p: &serde_json::Value) -> String {
    let l = &p["leafData"];
    format!("({},{},{},{},{})", l["account"].as_str().unwrap(), l["index"], l["balance"].as_str().unwrap(), l["escapeValue"].as_str().unwrap(), l["positionsHash"].as_str().unwrap())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "needs anvil, cast, forge, python3 and agent 433's contracts tree: set PERPS_ANVIL_DIR and PERPS_CONTRACTS_DIR (build host) and run with --ignored"]
async fn watcher_relayer_and_proofs_against_anvil_and_the_real_vault() {
    let dir = PathBuf::from(std::env::var("PERPS_ANVIL_DIR").expect("PERPS_ANVIL_DIR"));
    let contracts = PathBuf::from(std::env::var("PERPS_CONTRACTS_DIR").expect("PERPS_CONTRACTS_DIR"));
    let port: u16 = std::env::var("PERPS_ANVIL_PORT").ok().and_then(|p| p.parse().ok()).unwrap_or(53110);
    let work = tmpdir("anvil");

    // ---- a private anvil: chain 700770, loopback, finalized = latest - 2
    let child = Command::new(dir.join("anvil"))
        .args(["--port", &port.to_string(), "--host", "127.0.0.1", "--chain-id", "700770", "--slots-in-an-epoch", "1", "--silent"])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .expect("anvil");
    let anvil = Anvil { child, url: format!("http://127.0.0.1:{port}") };
    let a_url = anvil.url.clone();
    let mut up = false;
    for _ in 0..100 {
        if rpc(&a_url, "eth_chainId", serde_json::json!([])).await.is_ok() {
            up = true;
            break;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    assert!(up, "anvil did not start on {a_url}");
    let accounts = rpc(&a_url, "eth_accounts", serde_json::json!([])).await.unwrap();
    let t = Tools { dir: dir.clone(), rpc: a_url.clone(), from: accounts[0].as_str().unwrap().to_string() };

    // ---- the vault, deployed by 433's script with THIS node's committee (addresses only)
    let seed = format!("anvil-{port}");
    let keys = perps_node::dev::dev_keys(&seed, 7);
    let committee = work.join("committee.json");
    std::fs::write(&committee, serde_json::json!({"validators": keys.validators.iter().map(|k| to_hex(&key_address(k))).collect::<Vec<_>>(), "threshold": 5}).to_string()).unwrap();
    let out_json = work.join("deploy").join("out.json");
    let dep = Command::new("bash")
        .arg(contracts.join("deploy").join("local.sh"))
        .arg(&a_url)
        .arg(&committee)
        .arg(&out_json)
        .env("FOUNDRY_DIR", &dir)
        .output()
        .expect("local.sh");
    assert!(dep.status.success(), "deployment failed: {}{}", String::from_utf8_lossy(&dep.stdout), String::from_utf8_lossy(&dep.stderr));
    let d: serde_json::Value = serde_json::from_slice(&std::fs::read(&out_json).unwrap()).unwrap();
    let (vault_s, token_s) = (d["vault"].as_str().unwrap().to_string(), d["token"].as_str().unwrap().to_string());
    let vault: Address = parse_address(&vault_s).unwrap();
    let deploy_block = d["deployBlock"].as_u64().unwrap();
    let escape_delay = t.call_u(&vault_s, "escapeDelay()(uint64)", &[]) as u64;
    println!("vault {vault_s} token {token_s} deployBlock {deploy_block} escapeDelay {escape_delay}");

    // ---- the node: real engine, genesis vault = the deployed one, seven validators, two L2 endpoints
    let (g, keys, checked) = dev_genesis_real_with(&seed, 7, 5, 5, 0, &DevOptions { vault: Some(vault), ..Default::default() }).unwrap();
    let lie = Arc::new(Mutex::new(Lie::Honest));
    let pl = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let b_url = format!("http://{}", pl.local_addr().unwrap());
    let pr = Router::new().route("/", post(proxy_rpc)).with_state(Proxy { upstream: a_url.clone(), lie: lie.clone() });
    tokio::spawn(async move { axum::serve(pl, pr).await.unwrap() });
    let mut wcfg = WatcherConfig::new([a_url.clone(), b_url.clone()], vault);
    wcfg.start_block = deploy_block;
    let sv = view();
    let mk = || -> Box<dyn Engine> { Box::new(RealEngine::new(g.clone(), checked.clone()).unwrap()) };
    let seq = Sequencer::open(g.clone(), keys.sequencer.clone(), mk(), work.join("node"), sv.clone()).unwrap();
    let geo = GeoPolicy::from_json(&serde_json::json!({"version": "test", "unknown_country_effect": "deny", "tiers": {"allowed": {"effect": "allow"}}, "countries": {"PA": {"tier": "allowed"}}, "denied_policy": {"policy": "exit_only"}}), true).unwrap();
    let app = App::with(g.clone(), seq, geo, Mode::Dev, sv.clone(), 50, true, parse_address(&token_s).unwrap(), None);
    let (stop_tx, stop_rx) = tokio::sync::watch::channel(false);
    let engines: Vec<Box<dyn Engine>> = (0..7).map(|_| mk()).collect();
    spawn_local_validators(&app, keys.validators.clone(), engines, (0..7).map(|_| sv.clone()).collect(), &stop_rx).unwrap();
    tokio::spawn(run_producer(app.clone(), stop_rx.clone()));
    let mine = |n: u64| {
        let a = a_url.clone();
        async move { rpc(&a, "anvil_mine", serde_json::json!([format!("0x{n:x}")])).await.unwrap() }
    };
    let user = user(70);
    let ua = key_address(&user);
    let ua_s = to_hex(&ua);
    let deposits_seen = || sv.lock().unwrap().deposits.len();
    let credited = || app.seq.lock().unwrap().engine.next_deposit_index();

    // ---- A. finality depth
    t.send(&token_s, "mint(address,uint256)", &[&t.from, "10000000000"]);
    t.send(&token_s, "approve(address,uint256)", &[&vault_s, "10000000000"]);
    t.send(&vault_s, "deposit(uint64,address)", &["1000000000", &ua_s]);
    assert_eq!(t.call_u(&vault_s, "depositCount()(uint64)", &[]), 1);
    poll_once(&wcfg, &sv).await.unwrap();
    assert_eq!(deposits_seen(), 0, "A: a deposit in a block that is not final is not seen");
    mine(1).await;
    poll_once(&wcfg, &sv).await.unwrap();
    assert_eq!(deposits_seen(), 0, "A: one block deep is still not final (finalized = latest - 2)");
    mine(1).await;
    poll_once(&wcfg, &sv).await.unwrap();
    assert_eq!(deposits_seen(), 1, "A: final two blocks later: {:?}", sv.lock().unwrap().last_error);
    {
        let v = sv.lock().unwrap();
        let dpt = &v.deposits[&0];
        assert_eq!((dpt.account, dpt.amount), (ua, 1_000_000_000));
        assert_eq!(to_hex(&dpt.head), t.call(&vault_s, "depositHead(uint64)(bytes32)", &["1"]), "A: the watcher's head is the VAULT's own chain head");
        assert!(v.endpoints_agree);
    }
    wait_until("A: the engine credits the final deposit", 5000, || credited() == 1).await;
    assert_eq!(app.seq.lock().unwrap().account_view(&ua, false).unwrap()["collateral"], "1000.000000");

    // ---- B. reorg of an unfinalized deposit
    let snap = rpc(&a_url, "evm_snapshot", serde_json::json!([])).await.unwrap();
    t.send(&vault_s, "deposit(uint64,address)", &["500000000", &ua_s]);
    assert_eq!(t.call_u(&vault_s, "depositCount()(uint64)", &[]), 2);
    poll_once(&wcfg, &sv).await.unwrap();
    assert_eq!(deposits_seen(), 1, "B: the second deposit is in an unfinalized block");
    assert_eq!(rpc(&a_url, "evm_revert", serde_json::json!([snap])).await.unwrap(), true, "the chain is reorganised");
    assert_eq!(t.call_u(&vault_s, "depositCount()(uint64)", &[]), 1, "B: the deposit is gone from the chain");
    mine(6).await;
    poll_once(&wcfg, &sv).await.unwrap();
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert_eq!((deposits_seen(), credited()), (1, 1), "B: a reorganised deposit is never seen and never credited");

    // ---- C. two-endpoint agreement
    t.send(&vault_s, "deposit(uint64,address)", &["700000000", &ua_s]);
    mine(2).await;
    let before = sv.lock().unwrap().disagreements;
    *lie.lock().unwrap() = Lie::AlterLogs;
    let e = poll_once(&wcfg, &sv).await.expect_err("C: endpoint B alters a log");
    println!("C: altered log -> {e}");
    assert_eq!(deposits_seen(), 1, "C: no progress while the endpoints disagree on a log");
    *lie.lock().unwrap() = Lie::AlterBlockHash;
    let e = poll_once(&wcfg, &sv).await.expect_err("C: endpoint B reports another block hash");
    println!("C: other finalized hash -> {e}");
    assert_eq!(deposits_seen(), 1);
    {
        let v = sv.lock().unwrap();
        assert!(!v.endpoints_agree && v.disagreements == before + 2);
    }
    *lie.lock().unwrap() = Lie::Honest;
    poll_once(&wcfg, &sv).await.unwrap();
    assert_eq!(deposits_seen(), 2, "C: honest again, the watcher advances");
    wait_until("C: the second final deposit is credited", 5000, || credited() == 2).await;

    // ---- D. the relayer submits the checkpoint a withdrawal makes due; the vault pays against the node's proof
    let relay_key = throwaway_key(&format!("{seed}/relayer"), 0);
    let key_file = work.join("relay.key");
    std::fs::write(&key_file, hex::encode(relay_key.to_bytes())).unwrap();
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&key_file, std::fs::Permissions::from_mode(0o644)).unwrap();
        assert!(perps_node::relayer::read_key_file(&key_file).is_err(), "a key file readable by others is refused");
        std::fs::set_permissions(&key_file, std::fs::Permissions::from_mode(0o600)).unwrap();
    }
    let relay_addr = to_hex(&key_address(&relay_key));
    rpc(&a_url, "anvil_setBalance", serde_json::json!([relay_addr, "0x8ac7230489e80000"])).await.unwrap();
    let rcfg = perps_node::relayer::RelayConfig { endpoints: vec![b_url.clone(), a_url.clone()], key: perps_node::relayer::read_key_file(&key_file).unwrap(), vault, poll_ms: 200, receipt_timeout_ms: 20_000 };
    let (rstop_tx, rstop_rx) = tokio::sync::watch::channel(false);
    let relayer = tokio::spawn(perps_node::relayer::run(app.clone(), rcfg, rstop_rx));
    // no withdrawal, no processed forced request, no checkpoint on the vault yet: nothing is due
    tokio::time::sleep(Duration::from_millis(800)).await;
    assert_eq!(t.call_u(&vault_s, "checkpointCount()(uint64)", &[]), 0, "D: nothing due before a user waits");
    assert_eq!(app.relay.lock().unwrap().vault_mode, "normal");
    let now_t = now();
    let ack = app.seq.lock().unwrap().admit_signed(withdraw(&g, &user, ua, now_t, 300_000_000, ua), now_t).unwrap();
    wait_until("D: the relayer's checkpoint is accepted by the vault", 20_000, || app.relay.lock().unwrap().accepted >= 1).await;
    assert_eq!(app.seq.lock().unwrap().acks[&ack.action_hash].result.as_ref().unwrap()["withdrawalId"], "0");
    let rs = app.relay.lock().unwrap().to_json();
    println!("D: relayer {rs}");
    assert_eq!(rs["lastReason"], "withdrawal");
    let seqn = t.call_u(&vault_s, "checkpointCount()(uint64)", &[]);
    assert!(seqn >= 1);
    let count = t.call_u(&vault_s, "withdrawalCountAt(uint64)(uint64)", &[&seqn.to_string()]) as u64;
    assert_eq!(count, 1);
    let wp = app.seq.lock().unwrap().withdrawal_proof(0, Some(count), None).unwrap();
    assert_eq!(to_hex(&perps_node::util::parse_hex_fixed::<32>(wp["withdrawalsRoot"].as_str().unwrap()).unwrap()), t.call(&vault_s, "withdrawalsRootAt(uint64)(bytes32)", &[&seqn.to_string()]), "D: the proof's root is the root the VAULT holds");
    let r = &wp["record"];
    let w_tuple = format!("({},{},{},{},{})", r["withdrawalId"].as_str().unwrap(), r["account"].as_str().unwrap(), r["recipient"].as_str().unwrap(), r["amount"].as_str().unwrap(), r["height"].as_str().unwrap());
    let bal0 = t.call_u(&token_s, "balanceOf(address)(uint256)", &[&ua_s]);
    // a tampered record is refused by the vault; the node's is paid
    let bad = w_tuple.replace(",299000000,", ",299000001,");
    assert!(!t.try_send(&vault_s, "withdraw(uint64,(uint64,address,address,uint64,uint64),bytes32[],uint64)", &[&seqn.to_string(), &bad, &tuple_list(&wp["siblings"]), "18446744073709551615"]).0, "D: an altered record does not verify");
    t.send(&vault_s, "withdraw(uint64,(uint64,address,address,uint64,uint64),bytes32[],uint64)", &[&seqn.to_string(), &w_tuple, &tuple_list(&wp["siblings"]), "18446744073709551615"]);
    assert_eq!(t.call_u(&token_s, "balanceOf(address)(uint256)", &[&ua_s]) - bal0, 299_000_000, "D: the vault paid the net amount (300 - 1 USDC fee) against the node's proof");

    // ---- E. escape: the node's account proofs are what the vault accepts
    let _ = rstop_tx.send(true);
    let _ = relayer.await;
    let vault_height = t.call(&vault_s, "latest()(uint64,uint64,uint64,uint64,uint64,uint64,uint128,bytes32,bytes32)", &[]).lines().next().unwrap().split_whitespace().next().unwrap().parse::<u64>().unwrap();
    assert!(app.seq.lock().unwrap().pinned_frames.contains(&vault_height), "E: the frame of the checkpoint the vault holds is pinned");
    rpc(&a_url, "evm_increaseTime", serde_json::json!([format!("0x{:x}", escape_delay + 1)])).await.unwrap();
    mine(1).await;
    assert_eq!(t.call_u(&vault_s, "escapeReason()(uint8)", &[]), 1, "E: silence");
    t.send(&vault_s, "triggerEscape()", &[]);
    assert_eq!(t.call(&vault_s, "inEscape()(bool)", &[]), "true");
    let proof = |a: &Address| app.seq.lock().unwrap().account_proof(a, Some(vault_height)).unwrap();
    let (pi, pb, pu) = (proof(&postern_perps_engine::engine::system_address(1)), proof(&postern_perps_engine::engine::system_address(2)), proof(&ua));
    assert_eq!(pu["checkpoint"]["accountsRoot"].as_str().unwrap(), t.call(&vault_s, "latest()(uint64,uint64,uint64,uint64,uint64,uint64,uint128,bytes32,bytes32)", &[]).lines().nth(7).unwrap().trim(), "E: the proof's checkpoint root is the accountsRoot the VAULT holds");
    const ACCT: &str = "(address,uint32,int128,uint64,bytes32)";
    // no claim before the system shares are proven (R395-33); then the user's claim with the node's proof
    assert!(!t.try_send(&vault_s, &format!("claimEscape({ACCT},bytes32[],uint64)"), &[&account_tuple(&pu), &tuple_list(&pu["proof"]), "18446744073709551615"]).0);
    t.send(&vault_s, &format!("proveSystemShares({ACCT},bytes32[],{ACCT},bytes32[])"), &[&account_tuple(&pi), &tuple_list(&pi["proof"]), &account_tuple(&pb), &tuple_list(&pb["proof"])]);
    let escape_value: u128 = pu["leafData"]["escapeValue"].as_str().unwrap().parse().unwrap();
    assert_eq!(escape_value, 1_000_000_000 + 700_000_000 - 300_000_000, "E: the engine's escape value of the user (no position)");
    let bal1 = t.call_u(&token_s, "balanceOf(address)(uint256)", &[&ua_s]);
    let mut wrong = pu.clone();
    wrong["leafData"]["escapeValue"] = (escape_value + 1).to_string().into();
    assert!(!t.try_send(&vault_s, &format!("claimEscape({ACCT},bytes32[],uint64)"), &[&account_tuple(&wrong), &tuple_list(&pu["proof"]), "18446744073709551615"]).0, "E: an altered leaf does not verify");
    t.send(&vault_s, &format!("claimEscape({ACCT},bytes32[],uint64)"), &[&account_tuple(&pu), &tuple_list(&pu["proof"]), "18446744073709551615"]);
    assert_eq!(t.call_u(&token_s, "balanceOf(address)(uint256)", &[&ua_s]) - bal1, escape_value, "E: the vault paid the escape value to the unit against the node's proof");
    // the relayer's view after escape: mode `escape`, nothing more is due
    let one = perps_node::relayer::RelayConfig { endpoints: vec![a_url.clone()], key: perps_node::relayer::read_key_file(&key_file).unwrap(), vault, poll_ms: 200, receipt_timeout_ms: 5_000 };
    assert_eq!(perps_node::relayer::step(&app, &one, 0).await.unwrap(), None);
    assert_eq!(app.relay.lock().unwrap().vault_mode, "escape");
    assert!(app.relay.lock().unwrap().alarm.as_ref().unwrap().contains("ESCAPE"));
    let _ = stop_tx.send(true);
    drop(anvil);
}

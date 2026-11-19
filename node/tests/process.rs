//! Round 2 (agent 431): the node as real PROCESSES over loopback TCP: `postern-perps-node dev` (sequencer + 4 of
//! the 7 validators in process) and `postern-perps-node validator` (the fifth, its own process, its own log, its
//! own read service). THROWAWAY keys derived from the seed inside each process. UNAUDITED PROTOTYPE.
//!
//! What it shows: finality needs the remote validator (4 local signatures are below the threshold of 5); a
//! signature by a committee key over another body is refused and never counted; the validator serves blocks and
//! account proofs from its OWN replay (R436-11 (b)), also for a height it must rebuild from its log, and also
//! while the sequencer is down; SIGTERM stops both kinds of process cleanly; a restarted validator re-applies its
//! own log without the sequencer; a restarted sequencer recovers the same height and root.

mod common;

use common::*;
use perps_node::checkpoint::{sign_checkpoint, CheckpointBody};
use perps_node::dev::{dev_genesis_real_with, DevOptions};
use perps_node::sig::key_address;
use perps_node::util::{parse_hex_fixed, to_hex};
use perps_node::watcher::http_call;
use std::process::{Child, Command, Stdio};
use std::time::Duration;

const BIN: &str = env!("CARGO_BIN_EXE_postern-perps-node");

struct Proc(Option<Child>);

impl Proc {
    fn pid(&self) -> u32 {
        self.0.as_ref().unwrap().id()
    }
    /// SIGTERM to this child only, then its exit status (within 10 s).
    fn terminate(&mut self) -> std::process::ExitStatus {
        let mut c = self.0.take().unwrap();
        assert!(Command::new("kill").args(["-TERM", &c.id().to_string()]).status().unwrap().success());
        for _ in 0..100 {
            if let Some(st) = c.try_wait().unwrap() {
                return st;
            }
            std::thread::sleep(Duration::from_millis(100));
        }
        let _ = c.kill();
        panic!("the process did not stop within 10 s of SIGTERM");
    }
}

impl Drop for Proc {
    fn drop(&mut self) {
        if let Some(mut c) = self.0.take() {
            let _ = c.kill();
            let _ = c.wait();
        }
    }
}

fn free_port() -> u16 {
    std::net::TcpListener::bind("127.0.0.1:0").unwrap().local_addr().unwrap().port()
}

fn spawn(args: &[&str], log: &std::path::Path) -> Proc {
    let f = std::fs::OpenOptions::new().create(true).append(true).open(log).unwrap();
    Proc(Some(Command::new(BIN).args(args).stdout(Stdio::null()).stderr(f).spawn().expect("spawn the node binary")))
}

async fn get(url: &str) -> (u16, serde_json::Value) {
    match http_call("GET", url, None, &[]).await {
        Ok((s, b)) => (s, serde_json::from_slice(&b).unwrap_or(serde_json::Value::Null)),
        Err(_) => (0, serde_json::Value::Null),
    }
}

async fn post(url: &str, body: &serde_json::Value) -> (u16, serde_json::Value) {
    let (s, b) = http_call("POST", url, Some(&body.to_string()), &[("X-Mock-Country", "ZZ")]).await.unwrap();
    (s, serde_json::from_slice(&b).unwrap_or(serde_json::Value::Null))
}

async fn until<F, Fut>(what: &str, ms: u64, mut f: F) -> serde_json::Value
where
    F: FnMut() -> Fut,
    Fut: std::future::Future<Output = Option<serde_json::Value>>,
{
    let end = tokio::time::Instant::now() + Duration::from_millis(ms);
    loop {
        if let Some(v) = f().await {
            return v;
        }
        assert!(tokio::time::Instant::now() < end, "timed out waiting for: {what}");
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn sequencer_and_remote_validator_as_processes_over_loopback_tcp() {
    let seed = format!("proc-{}", std::process::id());
    let dir = tmpdir("process");
    let (seq_dir, val_dir) = (dir.join("seq"), dir.join("val4"));
    let (p_seq, p_val) = (free_port(), free_port());
    let (seq_url, val_url) = (format!("http://127.0.0.1:{p_seq}"), format!("http://127.0.0.1:{p_val}"));
    let (g, keys, _) = dev_genesis_real_with(&seed, 7, 5, 1, 0, &DevOptions::default()).unwrap();
    let genesis_flags = ["--seed", seed.as_str(), "--checkpoint-interval", "1"];
    let seq_listen = format!("127.0.0.1:{p_seq}");
    let val_listen = format!("127.0.0.1:{p_val}");
    let seq_args: Vec<&str> = [&["dev", "--data", seq_dir.to_str().unwrap(), "--listen", &seq_listen, "--local-validators", "4", "--block-ms", "50", "--dev-default-country", "ZZ"][..], &genesis_flags[..]].concat();
    let val_args: Vec<&str> = [&["validator", "--index", "4", "--sequencer", &seq_url, "--data", val_dir.to_str().unwrap(), "--listen", &val_listen][..], &genesis_flags[..]].concat();
    let mut sequencer = spawn(&seq_args, &dir.join("seq.log"));
    until("the sequencer's API", 15_000, || async { Some(get(&format!("{seq_url}/v1/status")).await).filter(|(s, _)| *s == 200).map(|x| x.1) }).await;

    // an action of an account that never deposited is admitted and makes a block (the engine's outcome is a
    // business rejection); every block is a checkpoint height here
    let u = user(90);
    let ua = key_address(&u);
    let mut n = 0u64;
    let mut act = |g: &perps_node::genesis::Genesis| {
        n += 1;
        request(g, &signed(g, &u, "CancelAll", serde_json::json!({"account": to_hex(&ua), "nonce": (now() + n).to_string(), "marketId": 0})))
    };
    let (s, ack) = post(&format!("{seq_url}/v1/actions?wait=included"), &act(&g)).await;
    assert_eq!((s, ack["status"].as_str(), ack["result"]["error"].as_str()), (200, Some("included"), Some("UNKNOWN_ACCOUNT")), "{ack}");
    tokio::time::sleep(Duration::from_millis(700)).await;
    assert_eq!(get(&format!("{seq_url}/v1/finality/latest")).await.0, 404, "four in-process validators are below the threshold of five");

    // the fifth validator, its own process
    let mut validator = spawn(&val_args, &dir.join("val.log"));
    until("the validator's read service", 15_000, || async { Some(get(&format!("{val_url}/v1/validator")).await).filter(|(s, _)| *s == 200).map(|x| x.1) }).await;
    let remote_addr = to_hex(&key_address(&keys.validators[4]));
    let (s, _) = post(&format!("{seq_url}/v1/actions?wait=included"), &act(&g)).await;
    assert_eq!(s, 200);
    let fin = until("finality with the remote validator's signature", 15_000, || async {
        let (s, j) = get(&format!("{seq_url}/v1/finality/latest")).await;
        (s == 200 && j["height"].as_u64() >= Some(2)).then_some(j)
    })
    .await;
    let signers: Vec<&str> = fin["signatures"].as_array().unwrap().iter().map(|x| x["validator"].as_str().unwrap()).collect();
    assert_eq!(signers.len(), 5);
    assert!(signers.contains(&remote_addr.as_str()), "the remote validator's signature made it final: {signers:?}");
    perps_node::checkpoint::verify_checkpoint_json(&g, &fin).expect("the checkpoint verifies from the committee alone");
    let h = fin["height"].as_u64().unwrap();

    // a rogue signer: a committee key (validator 5 does not run) over ANOTHER body at a height that is not final
    // yet for it: refused, never counted; its signature over the true body is accepted
    let body = CheckpointBody::from_json(&fin).unwrap();
    let mut other = body.clone();
    other.accounts_root[0] ^= 1;
    let rogue = sign_checkpoint(&keys.validators[5], &other, &g.vault_domain);
    let (s, e) = post(&format!("{seq_url}/v1/finality/signatures"), &serde_json::json!({"height": h, "signature": to_hex(&rogue)})).await;
    assert_eq!((s, e["error"]["code"].as_str()), (400, Some("BAD_SIGNATURE")), "{e}");
    let outsider = sign_checkpoint(&user(91), &body, &g.vault_domain);
    let (s, _) = post(&format!("{seq_url}/v1/finality/signatures"), &serde_json::json!({"height": h, "signature": to_hex(&outsider)})).await;
    assert_eq!(s, 400, "a key outside the committee is refused");
    let honest = sign_checkpoint(&keys.validators[5], &body, &g.vault_domain);
    let (s, j) = post(&format!("{seq_url}/v1/finality/signatures"), &serde_json::json!({"height": h, "signature": to_hex(&honest)})).await;
    assert_eq!((s, j["accepted"].as_bool()), (200, Some(true)), "{j}");

    // the validator's own read service: its blocks are the sequencer's, its proof is from its own replay
    let vs = until("the validator at the sequencer's height", 10_000, || async {
        let (_, j) = get(&format!("{val_url}/v1/validator")).await;
        (j["height"].as_u64() >= Some(h) && j["logHeight"].as_u64() >= Some(h)).then_some(j)
    })
    .await;
    assert!(vs["latched"].is_null() && vs["address"] == remote_addr.as_str(), "{vs}");
    let raws = |j: &serde_json::Value| j["blocks"].as_array().unwrap().iter().map(|b| b["raw"].as_str().unwrap().to_string()).collect::<Vec<_>>();
    let (_, sb) = get(&format!("{seq_url}/v1/blocks?from=1&limit={h}&raw=1")).await;
    let (_, vb) = get(&format!("{val_url}/v1/blocks?from=1&limit={h}&raw=1")).await;
    assert_eq!(raws(&sb), raws(&vb), "the validator's own log holds the same blocks, byte for byte");
    assert_eq!(raws(&vb).len() as u64, h);
    let insurance = "0x0000000000000000000000000000000000000002";
    let check_proof = |p: &serde_json::Value, want_state_root: &str| {
        assert_eq!(p["source"], "validator");
        assert_eq!(p["stateRoot"].as_str().unwrap(), want_state_root, "the validator's replayed state root is the block's");
        let leaf = parse_hex_fixed::<32>(p["leaf"].as_str().unwrap()).unwrap();
        let root = parse_hex_fixed::<32>(p["accountsRoot"].as_str().unwrap()).unwrap();
        let sib: Vec<[u8; 32]> = p["proof"].as_array().unwrap().iter().map(|x| parse_hex_fixed::<32>(x.as_str().unwrap()).unwrap()).collect();
        assert!(postern_perps_engine::merkle::verify(&leaf, p["index"].as_str().unwrap().parse().unwrap(), &sib, &root));
        assert_eq!(p["index"], "1");
    };
    let (s, vp) = get(&format!("{val_url}/v1/accounts/{insurance}/proof?height={h}")).await;
    assert_eq!(s, 200, "{vp}");
    check_proof(&vp, fin["stateRoot"].as_str().unwrap());
    assert_eq!(vp["accountsRoot"], fin["accountsRoot"], "the same accountsRoot the committee certified");
    assert_eq!(vp["checkpoint"]["signatures"][0]["validator"], remote_addr.as_str(), "with the validator's own signature");
    let (s, e) = get(&format!("{val_url}/v1/accounts/{insurance}/proof?height={}", h + 1000)).await;
    assert_eq!(s, 404, "{e}");
    let (s, vg) = http_call("GET", &format!("{val_url}/v1/genesis"), None, &[]).await.unwrap();
    assert_eq!((s, to_hex(&perps_node::util::keccak_parts(&[b"PosternPerps/genesis/v1", &vg]))), (200, to_hex(&g.genesis_log_hash())));

    // SIGTERM stops the validator cleanly; the sequencer goes on (not final without it)
    let vpid = validator.pid();
    assert!(validator.terminate().success(), "validator {vpid} exits 0 on SIGTERM");
    let (s, _) = post(&format!("{seq_url}/v1/actions?wait=included"), &act(&g)).await;
    assert_eq!(s, 200);
    let h3 = h + 1;
    tokio::time::sleep(Duration::from_millis(500)).await;
    assert_eq!(get(&format!("{seq_url}/v1/finality/latest")).await.1["height"].as_u64(), Some(h), "no finality while the fifth validator is down");
    // restart on the same data directory: its own log is re-applied without the sequencer, then it follows again
    let mut validator = spawn(&val_args, &dir.join("val.log"));
    let vs = until("the restarted validator caught up", 15_000, || async {
        let (s, j) = get(&format!("{val_url}/v1/validator")).await;
        (s == 200 && j["height"].as_u64() >= Some(h3)).then_some(j)
    })
    .await;
    assert_eq!(vs["replayedOwnLog"].as_u64(), Some(h), "blocks 1..={h} came from its own log: {vs}");
    let (s, _) = post(&format!("{seq_url}/v1/actions?wait=included"), &act(&g)).await;
    assert_eq!(s, 200);
    let fin2 = until("finality resumes with the restarted validator", 15_000, || async {
        let (s, j) = get(&format!("{seq_url}/v1/finality/latest")).await;
        (s == 200 && j["height"].as_u64() > Some(h3)).then_some(j)
    })
    .await;
    let h4 = fin2["height"].as_u64().unwrap();
    // a height the restarted validator holds no frame for (it signed nothing there after the restart): rebuilt
    // from its own log from genesis
    let (_, b1) = get(&format!("{seq_url}/v1/blocks/1")).await;
    let (s, old) = get(&format!("{val_url}/v1/accounts/{insurance}/proof?height=1")).await;
    assert_eq!(s, 200, "{old}");
    check_proof(&old, b1["stateRoot"].as_str().unwrap());
    assert!(old["checkpoint"].is_null(), "no checkpoint of its own at that height in this run: the roots are what the claimant compares with the vault");

    // the sequencer stops on SIGTERM; the validator keeps serving the log and proofs (the escape-hatch case)
    let (_, st_before) = get(&format!("{seq_url}/v1/status")).await;
    assert!(sequencer.terminate().success(), "the sequencer exits 0 on SIGTERM");
    assert_eq!(get(&format!("{seq_url}/v1/status")).await.0, 0, "the sequencer's API is gone");
    until("the validator applied the last block", 5_000, || async {
        let (_, j) = get(&format!("{val_url}/v1/validator")).await;
        (j["logHeight"].as_u64() >= Some(h4)).then_some(j)
    })
    .await;
    let (s, vb) = get(&format!("{val_url}/v1/blocks?from=1&limit=100&raw=1")).await;
    assert_eq!((s, raws(&vb).len() as u64 >= h4), (200, true));
    let (s, vp) = get(&format!("{val_url}/v1/accounts/{insurance}/proof?height={h4}")).await;
    assert_eq!(s, 200, "{vp}");
    check_proof(&vp, fin2["stateRoot"].as_str().unwrap());
    assert_eq!(vp["accountsRoot"], fin2["accountsRoot"]);

    // the sequencer restarts on its data directory: same height, same root, checkpoints reloaded
    let _sequencer = spawn(&seq_args, &dir.join("seq.log"));
    let st_after = until("the restarted sequencer's API", 15_000, || async { Some(get(&format!("{seq_url}/v1/status")).await).filter(|(s, _)| *s == 200).map(|x| x.1) }).await;
    assert_eq!(st_after["latestBlock"], st_before["latestBlock"]);
    assert_eq!(st_after["latestFinalized"]["height"].as_u64(), Some(h4));
    let (_, ni) = get(&format!("{seq_url}/v1/node")).await;
    assert_eq!(ni["recovery"]["replayedBlocks"], st_before["latestBlock"]["height"]);
    assert!(validator.terminate().success());
}

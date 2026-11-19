//! Fixes of agent 436's pass-2 findings on the node (R436-14 to R436-17). Each defect was shown on `cdf1b6b`
//! first: R436-15 by 436's own demonstration (`review/node/r436_node_p2.rs`: 2 passed = defect present, run on
//! the host before the fix), the others by the tests below failing there. THROWAWAY keys; every "endpoint" is a
//! loopback fake. UNAUDITED PROTOTYPE.

mod common;

use axum::extract::State;
use axum::routing::post;
use axum::{Json, Router};
use common::*;
use perps_node::checkpoint::CheckpointBody;
use perps_node::relayer::{alarm, capped_gas_price, due, local_alarm, plausible, read_vault_agreed, selector, submit, RelayConfig, RelayState, VaultView};
use perps_node::sig::throwaway_key;
use perps_node::util::{keccak, parse_hex_vec, to_hex};
use std::sync::{Arc, Mutex};

#[derive(Clone)]
struct Fake {
    gas_price: String,
    sent: Vec<String>,
    /// status of the receipt this endpoint returns (None: it has no receipt)
    receipt: Option<&'static str>,
    /// answer eth_sendRawTransaction with this instead of the transaction's hash
    send_hash: Option<String>,
    finalized: (u64, String),
    view: VaultView,
}

impl Fake {
    fn new(view: VaultView) -> Fake {
        Fake { gas_price: "0x3b9aca00".into(), sent: Vec::new(), receipt: Some("0x1"), send_hash: None, finalized: (40, to_hex(&[0xaa; 32])), view }
    }
}

type Shared = Arc<Mutex<Fake>>;

fn words(v: &[u64]) -> String {
    let mut out = String::from("0x");
    for x in v {
        out.push_str(&format!("{x:064x}"));
    }
    out
}

/// A well-formed L2 endpoint whose every answer comes from `Fake` (honest or not).
async fn fake_rpc(State(f): State<Shared>, Json(req): Json<serde_json::Value>) -> Json<serde_json::Value> {
    let mut f = f.lock().unwrap();
    let v = f.view.clone();
    let result = match req["method"].as_str().unwrap_or("") {
        "eth_chainId" => serde_json::json!("0xab162"),
        "eth_getCode" => serde_json::json!("0x6080"),
        "eth_getBlockByNumber" => serde_json::json!({"number": format!("0x{:x}", f.finalized.0), "hash": f.finalized.1, "timestamp": format!("0x{:x}", v.l2_time_s)}),
        "eth_call" => {
            let data = parse_hex_vec(req["params"][0]["data"].as_str().unwrap()).unwrap();
            let is = |sig: &str| data[..4] == selector(sig);
            if is("latest()") {
                serde_json::json!(words(&[v.latest_height, 0, 0, v.latest_next_forced, v.latest_withdrawal_count, 0, 0, 0, 0]))
            } else {
                let x = if is("checkpointCount()") {
                    v.checkpoint_count
                } else if is("lastCheckpointAt()") {
                    v.last_checkpoint_at
                } else if is("forcedCount()") {
                    v.forced_count
                } else if is("forcedDelay()") {
                    v.forced_delay
                } else if is("escapeDelay()") {
                    v.escape_delay
                } else if is("paused()") {
                    v.paused as u64
                } else if is("inEscape()") {
                    v.in_escape as u64
                } else {
                    v.open_forced_deadline.unwrap_or(0)
                };
                serde_json::json!(words(&[x]))
            }
        }
        "eth_estimateGas" => serde_json::json!("0x30000"),
        "eth_gasPrice" => serde_json::json!(f.gas_price),
        "eth_getTransactionCount" => serde_json::json!("0x0"),
        "eth_sendRawTransaction" => {
            let raw = req["params"][0].as_str().unwrap_or("").to_string();
            let h = f.send_hash.clone().unwrap_or_else(|| to_hex(&keccak(&parse_hex_vec(&raw).unwrap())));
            f.sent.push(raw);
            serde_json::json!(h)
        }
        "eth_getTransactionReceipt" => match f.receipt {
            Some(st) => serde_json::json!({"status": st, "gasUsed": "0x20000"}),
            None => serde_json::Value::Null,
        },
        _ => serde_json::Value::Null,
    };
    Json(serde_json::json!({"jsonrpc": "2.0", "id": req["id"], "result": result}))
}

async fn endpoint(f: Fake) -> (String, Shared) {
    let st: Shared = Arc::new(Mutex::new(f));
    let l = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}/", l.local_addr().unwrap());
    let s2 = st.clone();
    tokio::spawn(async move { axum::serve(l, Router::new().route("/", post(fake_rpc)).with_state(s2)).await.unwrap() });
    (url, st)
}

fn body(height: u64, wc: u64, nf: u64) -> CheckpointBody {
    CheckpointBody {
        height, time: 1_800_000_000_000, log_hash: [1; 32], accounts_root: [2; 32], markets_root: [3; 32], withdrawals_root: [4; 32],
        next_deposit_index: 1, next_forced_index: nf, next_order_id: 1, total_escape: 0, dust: 0, aux_hash: [5; 32], deposits_head: [6; 32],
        forced_head: [7; 32], withdrawal_count: wc, withdrawals_total: 1,
    }
}

fn truth() -> VaultView {
    VaultView { checkpoint_count: 3, latest_height: 50, latest_withdrawal_count: 0, last_checkpoint_at: 1_000_000, forced_delay: 172_800, escape_delay: 604_800, l2_time_s: 1_400_000, ..Default::default() }
}

fn cfg(endpoints: Vec<String>) -> RelayConfig {
    let mut c = RelayConfig::new(endpoints, throwaway_key("r436-15-relayer", 0), [0x42; 20]).unwrap();
    c.poll_ms = 50;
    c.receipt_timeout_ms = 700;
    c
}

/// R436-15 (a): the relayer signed whatever gas price ONE endpoint named (436: gasPrice 0xfffffffffffffffe).
/// Property: twice the highest price ANY endpoint names, never above the configured cap; above it nothing is
/// signed and nothing is handed to any endpoint.
#[tokio::test]
async fn r436_15_gas_price_is_capped_and_above_the_cap_nothing_is_signed() {
    let (a, sa) = endpoint(Fake::new(truth())).await;
    let (b, sb) = endpoint(Fake::new(truth())).await;
    let c = cfg(vec![a.clone(), b.clone()]);
    assert_eq!(capped_gas_price(&c).await, Ok(2_000_000_000), "1 gwei named by both: 2 gwei signed");
    // one endpoint (either one) names an absurd price: refused, whichever endpoint the transaction would go through
    for liar in [&sa, &sb] {
        liar.lock().unwrap().gas_price = "0x7fffffffffffffff".into();
        for via in [0, 1] {
            let e = submit(&c, via, &body(100, 1, 0), &[[9u8; 65]; 5]).await.expect_err("above the cap");
            assert!(e.starts_with("GAS PRICE ABOVE THE CAP"), "{e}");
        }
        liar.lock().unwrap().gas_price = "0x3b9aca00".into();
    }
    // a price that does not fit 128 bits, and one just above the cap (25 gwei x 2 = 50 gwei is the cap itself)
    sa.lock().unwrap().gas_price = format!("0x{}", "f".repeat(40));
    assert!(capped_gas_price(&c).await.is_err());
    sa.lock().unwrap().gas_price = format!("0x{:x}", 25_000_000_001u64);
    assert!(capped_gas_price(&c).await.is_err());
    sa.lock().unwrap().gas_price = format!("0x{:x}", 25_000_000_000u64);
    assert_eq!(capped_gas_price(&c).await, Ok(50_000_000_000));
    assert!(sa.lock().unwrap().sent.is_empty() && sb.lock().unwrap().sent.is_empty(), "no signed transaction left the relayer");
    // with honest prices the submission goes through and carries 2 x the highest price (RLP 0x84 77359400 = 2 gwei)
    sa.lock().unwrap().gas_price = "0x3b9aca00".into();
    let h = submit(&c, 0, &body(100, 1, 0), &[[9u8; 65]; 5]).await.expect("submitted");
    let raw = sa.lock().unwrap().sent[0].clone();
    assert_eq!(h, to_hex(&keccak(&parse_hex_vec(&raw).unwrap())));
    assert!(raw.contains("8477359400"), "gas price 2 gwei in the signed transaction: {}", &raw[..40]);
    // one endpoint is never enough
    assert!(RelayConfig::new(vec![a.clone()], throwaway_key("x", 0), [0x42; 20]).is_err());
    assert!(RelayConfig::new(vec![a.clone(), a], throwaway_key("x", 0), [0x42; 20]).is_err(), "the same endpoint twice is one endpoint");
}

/// R436-15: a submission counts only when EVERY endpoint returns the receipt with status 1, and the endpoint
/// cannot name another transaction as the one that was sent.
#[tokio::test]
async fn r436_15_a_receipt_from_one_endpoint_is_not_a_confirmation() {
    let (a, sa) = endpoint(Fake::new(truth())).await;
    let (b, sb) = endpoint(Fake::new(truth())).await;
    let c = cfg(vec![a, b]);
    sb.lock().unwrap().receipt = None;
    let e = submit(&c, 0, &body(100, 1, 0), &[[9u8; 65]; 5]).await.expect_err("endpoint B never saw it");
    assert!(e.contains("1 of 2 endpoints"), "{e}");
    sb.lock().unwrap().receipt = Some("0x0");
    let e = submit(&c, 0, &body(100, 1, 0), &[[9u8; 65]; 5]).await.expect_err("endpoint B saw it fail");
    assert!(e.contains("failed on"), "{e}");
    sb.lock().unwrap().receipt = Some("0x1");
    sa.lock().unwrap().send_hash = Some(to_hex(&[0x11; 32]));
    let e = submit(&c, 0, &body(100, 1, 0), &[[9u8; 65]; 5]).await.expect_err("a hash that is not the signed transaction's");
    assert!(e.contains("not the hash of the signed transaction"), "{e}");
    sa.lock().unwrap().send_hash = None;
    assert!(submit(&c, 1, &body(100, 1, 0), &[[9u8; 65]; 5]).await.is_ok(), "both confirm: done (sent through B)");
}

/// R436-15 (b): one lying endpoint silenced submission and alarm (436: `due` and `alarm` both None under "the
/// vault is already ahead"). Properties: (1) the reading is taken only when every endpoint agrees on the
/// finalized block and on the vault's state at it; (2) what they agree on must be a checkpoint THIS node
/// finalized, with its counts, or it is not believed; (3) the relayer's own clock alarms when it has had no
/// agreed and plausible reading, or a due checkpoint is not confirmed, whatever the endpoints say.
#[tokio::test]
async fn r436_15_a_lying_endpoint_can_silence_neither_the_decision_nor_the_alarm() {
    let cp = body(100, 1, 0);
    let lie = VaultView { latest_height: 1_000_000, last_checkpoint_at: truth().l2_time_s, ..truth() };
    // 436's observation still holds for the pure 8a rule: it is why the rule is no longer asked alone
    assert_eq!((due(&lie, &cp), alarm(&lie)), (None, None));
    // (1) one endpoint tells the truth, one lies: no reading at all
    let (a, sa) = endpoint(Fake::new(truth())).await;
    let (b, sb) = endpoint(Fake::new(lie.clone())).await;
    let eps = vec![a, b];
    let e = read_vault_agreed(&eps, &[0x42; 20]).await.expect_err("they disagree");
    assert!(e.contains("disagree on the vault's state"), "{e}");
    sb.lock().unwrap().view = truth();
    assert_eq!(read_vault_agreed(&eps, &[0x42; 20]).await, Ok(truth()));
    sb.lock().unwrap().finalized = (40, to_hex(&[0xbb; 32]));
    let e = read_vault_agreed(&eps, &[0x42; 20]).await.expect_err("another finalized block hash");
    assert!(e.contains("disagree on the finalized block"), "{e}");
    sb.lock().unwrap().finalized = (41, to_hex(&[0xaa; 32]));
    assert!(read_vault_agreed(&eps, &[0x42; 20]).await.is_err(), "another finalized height");
    assert!(read_vault_agreed(&eps[..1], &[0x42; 20]).await.is_err(), "one endpoint is no agreement");
    // (2) BOTH lie the same lie: the reading agrees, and this node does not believe it
    sb.lock().unwrap().finalized = sa.lock().unwrap().finalized.clone();
    sa.lock().unwrap().view = lie.clone();
    sb.lock().unwrap().view = lie.clone();
    let agreed = read_vault_agreed(&eps, &[0x42; 20]).await.unwrap();
    // this node finalized checkpoints at 50 (no withdrawal) and 100 (one withdrawal), nothing at 1,000,000
    let mine = |h: u64| match h {
        50 => Some(body(50, 0, 0)),
        100 => Some(body(100, 1, 0)),
        _ => None,
    };
    let e = plausible(&agreed, &mine).expect_err("a vault checkpoint this node never finalized");
    assert!(e.contains("never finalized"), "{e}");
    assert_eq!(plausible(&truth(), &mine), Ok(()));
    assert_eq!(due(&truth(), &cp), Some("withdrawal"), "and on the believed reading the checkpoint is due");
    // a real height with counts that are not that checkpoint's, an acceptance time in the future, a phantom latest
    assert!(plausible(&VaultView { latest_withdrawal_count: 1, ..truth() }, &mine).is_err());
    assert!(plausible(&VaultView { latest_next_forced: 4, ..truth() }, &mine).is_err());
    assert!(plausible(&VaultView { last_checkpoint_at: truth().l2_time_s + 1, ..truth() }, &mine).is_err());
    assert!(plausible(&VaultView { checkpoint_count: 0, ..truth() }, &mine).is_err());
    assert_eq!(plausible(&VaultView { forced_delay: 172_800, escape_delay: 604_800, l2_time_s: 5, ..Default::default() }, &mine), Ok(()), "a vault without a checkpoint");
    // (3) the alarms of the relayer's own clock
    let c = cfg(eps);
    let fresh = RelayState { started_ms: 1_000_000, ..Default::default() };
    assert_eq!(local_alarm(&fresh, &c, 1_000_000 + c.stall_alarm_ms - 1), None);
    assert!(local_alarm(&fresh, &c, 1_000_000 + c.stall_alarm_ms).unwrap().contains("no agreed and plausible reading"), "never a good reading since the start");
    let reading = RelayState { started_ms: 1_000_000, last_good_read_ms: Some(2_000_000), ..Default::default() };
    assert_eq!(local_alarm(&reading, &c, 2_000_000 + c.stall_alarm_ms - 1), None);
    assert!(local_alarm(&reading, &c, 2_000_000 + c.stall_alarm_ms).is_some(), "the endpoints went dark, disagreed or lied since");
    let owed = RelayState { started_ms: 1_000_000, last_good_read_ms: Some(3_000_000), due_since_ms: Some(2_500_000), ..Default::default() };
    assert_eq!(local_alarm(&owed, &c, 3_000_000), None);
    let later = RelayState { last_good_read_ms: Some(2_500_000 + c.delivery_alarm_ms), ..owed.clone() };
    assert!(local_alarm(&later, &c, 2_500_000 + c.delivery_alarm_ms).unwrap().contains("due for"), "readings are fine, delivery is not");
    let gas = RelayState { started_ms: 1, last_good_read_ms: Some(10), gas_refused: Some("GAS PRICE ABOVE THE CAP: x".into()), ..Default::default() };
    assert!(local_alarm(&gas, &c, 11).unwrap().starts_with("NOT SUBMITTING"));
}

/// R436-15 end to end with a node: two endpoints tell the SAME well-formed lie ("the vault is ahead"); the
/// relayer's step refuses the reading, counts the failure, and its loop raises the alarm from its own clock.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn r436_15_the_relayer_loop_is_loud_when_its_endpoints_lie_in_unison() {
    let n = net("r436-15-loop", 1);
    let geo = perps_node::geo::GeoPolicy::from_json(&serde_json::json!({"version": "t", "unknown_country_effect": "deny", "tiers": {"allowed": {"effect": "allow"}}, "countries": {}, "denied_policy": {"policy": "exit_only"}}), true).unwrap();
    let app = perps_node::node::App::new(n.g.clone(), n.seq, geo, perps_node::node::Mode::Dev, n.seq_view.clone(), 50, true);
    let lie = VaultView { latest_height: 1_000_000, last_checkpoint_at: truth().l2_time_s, ..truth() };
    let (a, _sa) = endpoint(Fake::new(lie.clone())).await;
    let (b, _sb) = endpoint(Fake::new(lie)).await;
    let mut c = cfg(vec![a, b]);
    c.stall_alarm_ms = 400;
    let e = perps_node::relayer::step(&app, &c, 0).await.expect_err("not believed");
    assert!(e.contains("never finalized"), "{e}");
    let (tx, rx) = tokio::sync::watch::channel(false);
    let h = tokio::spawn(perps_node::relayer::run(app.clone(), c, rx));
    tokio::time::sleep(std::time::Duration::from_millis(1200)).await;
    {
        let r = app.relay.lock().unwrap();
        assert!(r.failures >= 2 && r.accepted == 0 && r.submitted == 0, "{}", r.to_json());
        assert!(r.alarm.as_ref().is_some_and(|a| a.contains("no agreed and plausible reading")), "the relayer is loud: {}", r.to_json());
        assert_eq!(r.vault_mode, "unknown", "a reading that is not believed changes nothing the API reports");
    }
    let _ = tx.send(true);
    let _ = h.await;
}

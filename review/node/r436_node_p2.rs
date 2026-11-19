//! Agent 436, pass 2: demonstration against node `c66d601` (candidate `3b3259b`). Copy into `node/tests/`:
//! `cargo test --release --test r436_node_p2 -- --nocapture`. PASSES when the DEFECT IS PRESENT.
//! THROWAWAY keys only; the "endpoint" is a loopback fake. UNAUDITED review tooling, never merged.
use axum::extract::State;
use axum::routing::post;
use axum::{Json, Router};
use perps_node::checkpoint::CheckpointBody;
use perps_node::relayer::{alarm, due, submit, RelayConfig, VaultView};
use perps_node::sig::throwaway_key;
use std::sync::{Arc, Mutex};

type Sent = Arc<Mutex<Vec<String>>>;

/// A hostile (or broken) L2 endpoint: every answer is well formed.
async fn hostile(State(sent): State<Sent>, Json(req): Json<serde_json::Value>) -> Json<serde_json::Value> {
    let result = match req["method"].as_str().unwrap_or("") {
        "eth_estimateGas" => serde_json::json!("0x30000"),
        // 2^63 - 1 wei per gas: the relayer doubles it
        "eth_gasPrice" => serde_json::json!("0x7fffffffffffffff"),
        "eth_getTransactionCount" => serde_json::json!("0x0"),
        "eth_sendRawTransaction" => {
            sent.lock().unwrap().push(req["params"][0].as_str().unwrap_or("").to_string());
            serde_json::json!(format!("0x{}", "11".repeat(32)))
        }
        "eth_getTransactionReceipt" => serde_json::json!({"status": "0x1", "gasUsed": "0x20000"}),
        _ => serde_json::Value::Null,
    };
    Json(serde_json::json!({"jsonrpc": "2.0", "id": req["id"], "result": result}))
}

fn body() -> CheckpointBody {
    CheckpointBody {
        height: 100, time: 1_800_000_000_000, log_hash: [1; 32], accounts_root: [2; 32], markets_root: [3; 32], withdrawals_root: [4; 32],
        next_deposit_index: 1, next_forced_index: 0, next_order_id: 1, total_escape: 0, dust: 0, aux_hash: [5; 32], deposits_head: [6; 32],
        forced_head: [7; 32], withdrawal_count: 1, withdrawals_total: 1,
    }
}

/// R436-15 (a). The relayer takes the gas price from the ONE endpoint it is talking to, doubles it and signs: no
/// cap. The signed transaction it hands to that endpoint carries gasPrice 0xfffffffffffffffe (1.8e19 wei per gas):
/// whoever broadcasts it spends the whole balance of the relayer's gas key on one checkpoint.
#[tokio::test]
async fn r436_15a_the_relayer_signs_whatever_gas_price_one_endpoint_names() {
    let sent: Sent = Arc::new(Mutex::new(Vec::new()));
    let l = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}/", l.local_addr().unwrap());
    let s2 = sent.clone();
    tokio::spawn(async move { axum::serve(l, Router::new().route("/", post(hostile)).with_state(s2)).await.unwrap() });
    let cfg = RelayConfig { endpoints: vec![url.clone()], key: throwaway_key("r436-15-relayer", 0), vault: [0x42; 20], poll_ms: 100, receipt_timeout_ms: 2000 };
    let r = submit(&cfg, &url, &body(), &[[9u8; 65]; 5]).await;
    println!("submit: {r:?}");
    assert!(r.is_ok(), "the relayer reports success");
    let raw = sent.lock().unwrap().first().cloned().expect("a signed transaction was handed to the endpoint");
    println!("signed raw transaction starts {}...", &raw[..60.min(raw.len())]);
    assert!(raw.contains("88fffffffffffffffe"), "RLP of gasPrice = 2 * (2^63 - 1): signed without a cap");
}

/// R436-15 (b). The decision to submit and the alarm both rest on one endpoint's answers. An endpoint that says
/// the vault is already ahead silences both (the 8a rule returns None, the alarm stays quiet); it is rotated only
/// when it returns an ERROR, which a lying endpoint never does.
#[test]
fn r436_15b_one_lying_endpoint_silences_submission_and_the_alarm() {
    let cp = body();
    // the truth: the vault's last checkpoint is old, a withdrawal waits, silence is past escapeDelay / 2
    let truth = VaultView { checkpoint_count: 3, latest_height: 50, latest_withdrawal_count: 0, last_checkpoint_at: 1_000_000, forced_delay: 172_800, escape_delay: 604_800, l2_time_s: 1_000_000 + 400_000, ..Default::default() };
    assert_eq!(due(&truth, &cp), Some("withdrawal"));
    assert!(alarm(&truth).is_some());
    // the lie: "the vault already holds a newer checkpoint, accepted just now"
    let lie = VaultView { latest_height: 1_000_000, last_checkpoint_at: truth.l2_time_s, ..truth.clone() };
    println!("truth: due {:?}, alarm {:?}\nlie:   due {:?}, alarm {:?}", due(&truth, &cp), alarm(&truth).is_some(), due(&lie, &cp), alarm(&lie));
    assert_eq!(due(&lie, &cp), None, "nothing is submitted");
    assert_eq!(alarm(&lie), None, "and nothing is said");
}

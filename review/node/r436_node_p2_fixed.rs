//! Agent 436, pass 2: fixed-property port of `r436_node_p2.rs` for node `531750e` (431's R436-15 fix `d8fabd4`).
//! Same hostile endpoint, same lie. Copy into `node/tests/`. THROWAWAY keys only; loopback fakes.
use axum::extract::State;
use axum::routing::post;
use axum::{Json, Router};
use perps_node::checkpoint::CheckpointBody;
use perps_node::relayer::{plausible, submit, RelayConfig, VaultView};
use perps_node::sig::throwaway_key;
use std::sync::{Arc, Mutex};

type Sent = Arc<Mutex<(String, Vec<String>)>>; // (gas price this endpoint names, raw transactions it was handed)

async fn endpoint_fn(State(s): State<Sent>, Json(req): Json<serde_json::Value>) -> Json<serde_json::Value> {
    let result = match req["method"].as_str().unwrap_or("") {
        "eth_estimateGas" => serde_json::json!("0x30000"),
        "eth_gasPrice" => serde_json::json!(s.lock().unwrap().0.clone()),
        "eth_getTransactionCount" => serde_json::json!("0x0"),
        "eth_sendRawTransaction" => {
            s.lock().unwrap().1.push(req["params"][0].as_str().unwrap_or("").to_string());
            serde_json::json!(format!("0x{}", "11".repeat(32)))
        }
        "eth_getTransactionReceipt" => serde_json::json!({"status": "0x1", "gasUsed": "0x20000"}),
        _ => serde_json::Value::Null,
    };
    Json(serde_json::json!({"jsonrpc": "2.0", "id": req["id"], "result": result}))
}

async fn endpoint(price: &str) -> (String, Sent) {
    let s: Sent = Arc::new(Mutex::new((price.to_string(), Vec::new())));
    let l = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}/", l.local_addr().unwrap());
    let s2 = s.clone();
    tokio::spawn(async move { axum::serve(l, Router::new().route("/", post(endpoint_fn)).with_state(s2)).await.unwrap() });
    (url, s)
}

fn body() -> CheckpointBody {
    CheckpointBody {
        height: 100, time: 1_800_000_000_000, log_hash: [1; 32], accounts_root: [2; 32], markets_root: [3; 32], withdrawals_root: [4; 32],
        next_deposit_index: 1, next_forced_index: 0, next_order_id: 1, total_escape: 0, dust: 0, aux_hash: [5; 32], deposits_head: [6; 32],
        forced_head: [7; 32], withdrawal_count: 1, withdrawals_total: 1,
    }
}

/// The hostile endpoint of my demonstration (gasPrice 2^63 - 1) beside an honest one (1 gwei), tried as the
/// sending endpoint and as the other one: nothing is signed, nothing reaches either endpoint. A single endpoint is
/// refused at configuration.
#[tokio::test]
async fn r436_15a_fixed_the_hostile_gas_price_is_refused_and_nothing_is_signed() {
    let key = throwaway_key("r436-15-relayer", 0);
    assert!(RelayConfig::new(vec!["http://127.0.0.1:1/".into()], key.clone(), [0x42; 20]).is_err(), "one endpoint is not a configuration");
    let (bad, sb) = endpoint("0x7fffffffffffffff").await;
    let (good, sg) = endpoint("0x3b9aca00").await;
    for (order, via) in [(vec![bad.clone(), good.clone()], 0usize), (vec![bad.clone(), good.clone()], 1), (vec![good.clone(), bad.clone()], 0)] {
        let cfg = RelayConfig::new(order, key.clone(), [0x42; 20]).unwrap();
        let r = submit(&cfg, via, &body(), &[[9u8; 65]; 5]).await;
        println!("submit via endpoint {via}: {r:?}");
        assert!(r.is_err());
    }
    assert!(sb.lock().unwrap().1.is_empty() && sg.lock().unwrap().1.is_empty(), "no signed transaction was handed to any endpoint");
}

/// The lie of my demonstration ("the vault already holds a newer checkpoint") is not believed: this node never
/// finalized that height.
#[test]
fn r436_15b_fixed_the_lie_is_not_believed() {
    let truth = VaultView { checkpoint_count: 3, latest_height: 50, latest_withdrawal_count: 0, last_checkpoint_at: 1_000_000, forced_delay: 172_800, escape_delay: 604_800, l2_time_s: 1_400_000, ..Default::default() };
    let lie = VaultView { latest_height: 1_000_000, last_checkpoint_at: truth.l2_time_s, ..truth.clone() };
    let mine = |h: u64| if h == 50 { Some(CheckpointBody { height: 50, withdrawal_count: 0, next_forced_index: 0, ..body() }) } else { None };
    assert!(plausible(&truth, &mine).is_ok());
    let r = plausible(&lie, &mine);
    println!("the lie: {r:?}");
    assert!(r.is_err());
}

#![allow(dead_code)]
//! Test helpers: THROWAWAY keys only.

use k256::ecdsa::SigningKey;
use perps_node::actions::action_digest;
use perps_node::checkpoint::Aggregator;
use perps_node::dev::{dev_genesis, DevKeys};
use perps_node::engine::stub::StubEngine;
use perps_node::engine::Engine;
use perps_node::genesis::Genesis;
use perps_node::sequencer::Sequencer;
use perps_node::sig::{key_address, sign, throwaway_key};
use perps_node::types::{DepositEvent, SignedAction};
use perps_node::util::{keccak, to_hex, Address};
use perps_node::validator::{VResult, Validator};
use perps_node::watcher::{L2View, SharedView};
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

pub fn tmpdir(name: &str) -> PathBuf {
    let base = std::env::var("PERPS_TEST_TMP").map(PathBuf::from).unwrap_or_else(|_| std::env::temp_dir());
    let d = base.join(format!("perps-node-test-{name}-{}-{}", std::process::id(), perps_node::node::now_ms()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    d
}

pub fn view() -> SharedView {
    Arc::new(Mutex::new(L2View::default()))
}

pub fn now() -> u64 {
    perps_node::node::now_ms()
}

pub struct Net {
    pub g: Arc<Genesis>,
    pub keys: DevKeys,
    pub seq: Sequencer,
    pub vals: Vec<Validator>,
    pub seq_view: SharedView,
    pub val_views: Vec<SharedView>,
    pub dir: PathBuf,
}

pub fn net(name: &str, ci: u64) -> Net {
    let (g, keys) = dev_genesis(&format!("test-{name}"), 7, 5, ci, 0);
    let dir = tmpdir(name);
    let seq_view = view();
    let seq = Sequencer::open(g.clone(), keys.sequencer.clone(), Box::new(StubEngine::new(g.clone())), dir.clone(), seq_view.clone()).unwrap();
    let mut vals = Vec::new();
    let mut val_views = Vec::new();
    for k in &keys.validators {
        let v = view();
        val_views.push(v.clone());
        vals.push(Validator::new(g.clone(), k.clone(), Box::new(StubEngine::new(g.clone())), v, Some(dir.join(format!("val-{}", to_hex(&key_address(k)))))).unwrap());
    }
    Net { g, keys, seq, vals, seq_view, val_views, dir }
}

impl Net {
    /// Produces one block and lets every validator process it; returns the results.
    pub fn step(&mut self) -> Vec<VResult> {
        let info = self.seq.produce(now()).unwrap();
        let Some(info) = info else { return vec![] };
        let b = info.block.as_ref().clone();
        let mut out = Vec::new();
        for v in self.vals.iter_mut() {
            let r = v.process(&b, now());
            if let VResult::Applied { checkpoint: Some((body, sig)) } = &r {
                self.seq.add_signature(body.height, *sig).unwrap();
            }
            out.push(r);
        }
        out
    }

    pub fn deposit_everywhere(&self, d: DepositEvent) {
        self.seq_view.lock().unwrap().deposits.insert(d.index, d.clone());
        for v in &self.val_views {
            v.lock().unwrap().deposits.insert(d.index, d.clone());
        }
    }
}

pub fn user(i: u64) -> SigningKey {
    throwaway_key("test-user", i)
}

pub fn deposit(index: u64, account: Address, amount: u128) -> DepositEvent {
    DepositEvent {
        index,
        account,
        amount,
        l2_block: 100 + index,
        l2_block_hash: keccak(&index.to_be_bytes()),
        l2_tx_hash: keccak(&[index as u8, 1]),
        log_index: 0,
    }
}

/// Builds and signs an action from a wire-integer JSON message.
pub fn signed(g: &Genesis, key: &SigningKey, primary: &str, msg: serde_json::Value) -> SignedAction {
    let v = g.types.registry.val_from_json(primary, &msg).unwrap();
    let d = action_digest(g, primary, &v).unwrap();
    SignedAction { type_index: g.types.index_of(primary).unwrap(), message: v, signature: sign(key, &d) }
}

pub fn request(g: &Genesis, a: &SignedAction) -> serde_json::Value {
    let name = g.types.name(a.type_index).unwrap();
    serde_json::json!({"type": name, "message": g.types.registry.val_to_json(name, &a.message), "signature": to_hex(&a.signature), "wire": true})
}

pub fn place_order(g: &Genesis, key: &SigningKey, account: Address, nonce: u64, market: u32, price: u64, size: u64, reduce_only: bool) -> SignedAction {
    signed(
        g,
        key,
        "PlaceOrder",
        serde_json::json!({"account": to_hex(&account), "nonce": nonce.to_string(), "marketId": market, "isBuy": true, "orderType": 0,
            "timeInForce": 0, "price": price.to_string(), "size": size.to_string(), "reduceOnly": reduce_only, "triggerPrice": "0",
            "clientOrderId": to_hex(&[0u8; 32])}),
    )
}

pub fn withdraw(g: &Genesis, key: &SigningKey, account: Address, nonce: u64, amount: u128, recipient: Address) -> SignedAction {
    signed(g, key, "Withdraw", serde_json::json!({"account": to_hex(&account), "nonce": nonce.to_string(), "amount": amount.to_string(), "recipient": to_hex(&recipient)}))
}

pub fn fresh_engine(g: &Arc<Genesis>) -> Box<dyn Engine> {
    Box::new(StubEngine::new(g.clone()))
}

pub fn agg_final(a: &Aggregator) -> Option<u64> {
    a.latest_final
}

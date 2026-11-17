//! Adapter: the node's `Engine` trait over agent 388's `postern-perps-engine` (SPEC 0.1.1).
//!
//! Inputs cross the boundary as BYTES: the node encodes its inputs (SPEC 3.2) and the engine decodes them with
//! its own strict decoder (`codec::decode_inputs`, which also re-encodes and compares). A difference between the
//! node's and the engine's canonical form is therefore a refused block, never a silent reinterpretation.
//!
//! Engine 0.1 has no snapshot/restore: `snapshot()` is empty and the sequencer then recovers by replaying the
//! log from genesis (correct, slower; stated).
//!
//! UNAUDITED PROTOTYPE.

use super::stub::nonce_rule;
use super::{AuthorityError, BlockOutcome, Engine, InputOutcome, StateParts, WithdrawalRecord};
use crate::actions::verify_signed_action;
use crate::genesis::Genesis;
use crate::types::{encode_inputs, Input};
use crate::util::{scaled_to_decimal_signed, to_hex, Address, H256};
use postern_perps_engine as pe;
use std::sync::Arc;

#[derive(Clone)]
pub struct RealEngine {
    pub g: Arc<Genesis>,
    pub e: pe::Engine,
    log_hash0: H256,
}

impl RealEngine {
    pub fn new(g: Arc<Genesis>, checked: pe::genesis::Checked) -> Result<RealEngine, String> {
        if checked.verifying_contract != g.vault {
            return Err("engine genesis vault differs from the node genesis vault".into());
        }
        if checked.oracle_signers != g.oracle_committee {
            return Err("engine oracle signers differ from the node genesis oracle committee".into());
        }
        if checked.g.withdrawal_fee as u128 != g.withdrawal_fee {
            return Err("engine withdrawal fee differs from the node genesis".into());
        }
        for m in &checked.g.markets {
            if g.market(m.market_id).map(|x| x.symbol.as_str()) != Some(m.symbol.as_str()) {
                return Err(format!("market {} ({}) differs between engine and node genesis", m.market_id, m.symbol));
            }
        }
        let e = pe::Engine::new(checked);
        let log_hash0 = e.log_hash;
        Ok(RealEngine { g, e, log_hash0 })
    }

    fn agent_allowed(&self, primary: &str) -> bool {
        self.g.types.agent_allowed.iter().any(|a| a == primary)
    }
}

impl Engine for RealEngine {
    fn apply_block(&mut self, height: u64, time: u64, inputs: &[Input]) -> Result<BlockOutcome, String> {
        let bytes = encode_inputs(&self.g.types, inputs)?;
        let theirs = pe::codec::decode_inputs(&bytes).map_err(|e| format!("engine decoder refused the node's encoding: {}", e.0))?;
        let out = self.e.apply_block(&pe::Block { height, time, inputs: theirs }).map_err(|e| format!("engine block error {e:?}"))?;
        let mut bo = BlockOutcome::default();
        for (inp, r) in inputs.iter().zip(&out.outcomes) {
            let ah = match inp {
                Input::Action(a) => verify_signed_action(&self.g, a).ok().map(|v| v.action_hash),
                _ => None,
            };
            bo.outcomes.push(match r {
                Ok(()) => InputOutcome { ok: true, error: None, action_hash: ah, result: serde_json::json!({}) },
                Err(e) => InputOutcome { ok: false, error: Some(e.name().to_string()), action_hash: ah, result: serde_json::Value::Null },
            });
        }
        for ev in &out.events {
            if let pe::Event::Withdrawal(w) = ev {
                bo.withdrawals.push(w.id);
            }
            bo.events.push(serde_json::Value::String(format!("{ev:?}")));
        }
        Ok(bo)
    }
    fn state_root(&self) -> H256 {
        self.e.state_root()
    }
    fn height(&self) -> u64 {
        self.e.height
    }
    fn time(&self) -> u64 {
        self.e.time
    }
    fn snapshot(&self) -> Vec<u8> {
        Vec::new()
    }
    fn restore(&mut self, _bytes: &[u8]) -> Result<(), String> {
        Err("engine 0.1 has no snapshot format: replay from genesis".into())
    }
    fn next_deposit_index(&self) -> u64 {
        self.e.next_deposit
    }
    fn next_forced_index(&self) -> u64 {
        self.e.next_forced
    }
    fn last_oracle_round(&self) -> u64 {
        self.e.last_oracle_round.unwrap_or(0)
    }
    fn authority(&self, account: &Address, signer: &Address, primary: &str, time: u64) -> Result<(), AuthorityError> {
        if account == signer {
            return Ok(());
        }
        let ai = self.e.by_addr.get(account).ok_or(AuthorityError::UnknownSigner)?;
        match self.e.accounts[*ai as usize].agents.get(signer) {
            Some(a) if a.expires_at > time => {
                if self.agent_allowed(primary) {
                    Ok(())
                } else {
                    Err(AuthorityError::AgentNotAllowed)
                }
            }
            _ => Err(AuthorityError::UnknownSigner),
        }
    }
    fn highest_nonce(&self, signer: &Address) -> Option<u64> {
        self.e.nonces.get(signer).and_then(|s| s.last().copied())
    }
    fn genesis_log_hash(&self) -> H256 {
        self.log_hash0
    }
    fn withdrawal_count(&self) -> u64 {
        self.e.withdrawals.len() as u64
    }
    fn withdrawal(&self, id: u64) -> Option<WithdrawalRecord> {
        let w = self.e.withdrawals.get(id as usize)?;
        Some(WithdrawalRecord { id: w.id, account: w.account, recipient: w.recipient, amount: w.amount, fee: self.g.withdrawal_fee as u64, height: w.height })
    }
    fn state_parts(&self) -> StateParts {
        let total_escape: u128 = (0..self.e.accounts.len() as u32).map(|i| self.e.escape_value(i) as u128).sum();
        StateParts {
            accounts_root: pe::merkle::root(&self.e.account_leaves(), pe::engine::ACCOUNT_DEPTH),
            markets_root: self.e.markets_root(),
            withdrawals_root: pe::merkle::root(&self.e.withdrawal_leaves(), pe::engine::WITHDRAWAL_DEPTH),
            next_order_id: self.e.next_order_id,
            total_escape,
            dust: self.e.dust as u128,
            aux_hash: self.e.aux_hash(),
            withdrawals_total: self.e.withdrawals.iter().map(|w| w.amount).sum(),
        }
    }
    fn withdrawal_proof(&self, id: u64, count: u64) -> Option<Vec<H256>> {
        let leaves = self.e.withdrawal_leaves();
        if id >= count || count as usize > leaves.len() {
            return None;
        }
        Some(pe::merkle::proof(&leaves[..count as usize], pe::engine::WITHDRAWAL_DEPTH, id as usize))
    }
    fn nonce_ok(&self, signer: &Address, nonce: u64, time: u64) -> Result<(), &'static str> {
        nonce_rule(self.e.nonces.get(signer), nonce, time)
    }
    fn account_json(&self, a: &Address) -> serde_json::Value {
        let (bal, eq, n_pos, n_ord) = match self.e.by_addr.get(a) {
            Some(ai) => {
                let acc = &self.e.accounts[*ai as usize];
                (acc.balance, self.e.equity(*ai), acc.positions.len(), acc.orders.len())
            }
            None => (0, 0, 0, 0),
        };
        serde_json::json!({
            "address": to_hex(a), "height": self.e.height, "time": self.e.time,
            "collateral": scaled_to_decimal_signed(bal, 6),
            "equity": scaled_to_decimal_signed(eq / pe::num::N_PER_USDC, 6),
            "positionsCount": n_pos, "openOrders": n_ord, "engine": "postern-perps-engine 0.1",
        })
    }
    fn orders_json(&self, a: &Address) -> serde_json::Value {
        let Some(ai) = self.e.by_addr.get(a) else { return serde_json::json!({"orders": []}) };
        let v: Vec<_> = self.e.accounts[*ai as usize]
            .orders
            .iter()
            .filter_map(|id| self.e.orders.get(id))
            .map(|o| serde_json::json!({"orderId": o.id.to_string(), "marketId": o.market, "side": if o.is_buy {"buy"} else {"sell"},
                "priceRaw": o.price.to_string(), "remainingRaw": o.remaining.to_string(), "reduceOnly": o.reduce_only,
                "status": if o.trigger.is_some() {"untriggered"} else {"open"}}))
            .collect();
        serde_json::json!({"orders": v})
    }
    fn agents_json(&self, a: &Address) -> serde_json::Value {
        let Some(ai) = self.e.by_addr.get(a) else { return serde_json::json!({"agents": []}) };
        let v: Vec<_> = self.e.accounts[*ai as usize]
            .agents
            .iter()
            .map(|(ag, x)| serde_json::json!({"agent": to_hex(ag), "name": x.name, "expiresAt": x.expires_at, "active": x.expires_at > self.e.time}))
            .collect();
        serde_json::json!({"agents": v})
    }
    fn name(&self) -> &'static str {
        "postern-perps-engine"
    }
    fn clone_box(&self) -> Box<dyn Engine> {
        Box::new(self.clone())
    }
}

/// Dev: an engine genesis over the node's dev genesis (testkit markets with the node fixture's ids and symbols,
/// the node's oracle committee and vault). THROWAWAY keys only.
pub fn dev_engine_genesis(g: &Genesis) -> Result<pe::genesis::Checked, String> {
    let markets = g.markets.iter().map(|m| pe::testkit::market(m.market_id, &m.symbol)).collect();
    let eg = pe::genesis::Genesis {
        spec_version: "0.1".into(),
        chain_id: 700770,
        verifying_contract: to_hex(&g.vault),
        fee_recipient: to_hex(&g.fee_recipient),
        oracle_signers: g.oracle_committee.iter().map(|a| to_hex(a)).collect(),
        oracle_quorum: g.oracle_quorum as u8,
        oracle_max_future_ms: 1000,
        oracle_max_age_ms: 6000,
        withdrawal_fee: g.withdrawal_fee as u64,
        min_fee_sweep: 1_000_000,
        markets,
    };
    eg.check()
}

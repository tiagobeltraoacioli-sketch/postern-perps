//! Genesis parameters: everything set once that every sequencer and validator must agree on. Their canonical
//! encoding is hashed into `logHash(0)`, so a validator started with different markets, typed data, committee or
//! vault cannot agree with any block (the first block's `prevLogHash` differs).
//!
//! UNAUDITED PROTOTYPE.

use crate::eip712::{Domain, Registry, Ty};
use crate::types::ActionTypes;
use crate::util::{keccak, keccak_parts, parse_address, to_hex, Address, H256};
use std::collections::BTreeSet;

/// Nonce window of proposal (A): `blockTime - NONCE_PAST_MS <= nonce <= blockTime + NONCE_FUTURE_MS` (nonce read as
/// milliseconds). It bounds how long the sequencer can hold a signed action and still execute it.
pub const NONCE_PAST_MS: u64 = 120_000;
pub const NONCE_FUTURE_MS: u64 = 30_000;

#[derive(Clone, Debug)]
pub struct Market {
    pub symbol: String,
    pub market_id: u32,
    pub price_decimals: u32,
    pub size_decimals: u32,
    pub json: serde_json::Value,
}

#[derive(Clone, Debug)]
pub struct Genesis {
    pub engine_domain: Domain,
    pub vault_domain: Domain,
    pub types: ActionTypes,
    pub typed_data_json: serde_json::Value,
    pub typed_data_hash: H256,
    pub markets: Vec<Market>,
    pub markets_hash: H256,
    /// markets.json "defaults" (fees, funding, oracle) for the API's Market objects
    pub markets_defaults: serde_json::Map<String, serde_json::Value>,
    pub collateral_decimals: u32,
    pub committee: Vec<Address>,
    pub threshold: usize,
    pub oracle_committee: Vec<Address>,
    pub oracle_quorum: usize,
    pub sequencer: Address,
    pub vault: Address,
    pub fee_recipient: Address,
    pub withdrawal_fee: u128,
    pub genesis_time: u64,
    pub checkpoint_interval: u64,
    /// `logHash(0)`: by default keccak256 of `encode()`; an engine with its own genesis encoding (388's, SPEC 3.1)
    /// sets it with `with_log_hash0` so the node's chain starts where the engine's does.
    pub log_hash0: H256,
}

/// SPEC 3.2 action type ids 1..8 are the positions of this list (+1).
pub const SPEC_ACTIONS: [&str; 8] = ["PlaceOrder", "CancelOrder", "CancelAll", "UpdateLeverage", "UpdateMargin", "Withdraw", "ApproveAgent", "RevokeAgent"];

impl Genesis {
    /// Builds and validates the genesis. `typed_data_bytes` and `markets_bytes` are the exact files (their keccak
    /// hashes enter the genesis hash, so a whitespace change is a different genesis: deliberate).
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        typed_data_bytes: &[u8],
        markets_bytes: &[u8],
        committee: Vec<Address>,
        threshold: usize,
        oracle_committee: Vec<Address>,
        oracle_quorum: usize,
        sequencer: Address,
        vault: Address,
        fee_recipient: Address,
        genesis_time: u64,
        checkpoint_interval: u64,
    ) -> Result<Genesis, String> {
        let td: serde_json::Value = serde_json::from_slice(typed_data_bytes).map_err(|e| format!("typed data: {e}"))?;
        let registry = Registry::from_json(td.get("types").ok_or("typed data without types")?)?;
        let actions: Vec<String> = serde_json::from_value(td.get("actions").cloned().ok_or("typed data without actions")?)
            .map_err(|e| format!("actions: {e}"))?;
        let agent_allowed: Vec<String> = serde_json::from_value(td.get("agentAllowed").cloned().ok_or("typed data without agentAllowed")?)
            .map_err(|e| format!("agentAllowed: {e}"))?;
        if actions.iter().map(|s| s.as_str()).collect::<Vec<_>>() != SPEC_ACTIONS {
            return Err("action list must be SPEC 3.2's, in its order (type ids 1..8)".into());
        }
        let mut seen = BTreeSet::new();
        for a in &actions {
            if !seen.insert(a) {
                return Err(format!("duplicate action {a}"));
            }
            // every action carries `account: address` and `nonce: uint64` (authority and replay rules need them)
            let ms = registry.types.get(a).ok_or_else(|| format!("action {a} has no type"))?;
            let acc = ms.iter().find(|m| m.name == "account").ok_or_else(|| format!("{a} has no account"))?;
            let non = ms.iter().find(|m| m.name == "nonce").ok_or_else(|| format!("{a} has no nonce"))?;
            if acc.ty != Ty::Address || non.ty != Ty::Uint(64) {
                return Err(format!("{a}: account must be address and nonce uint64"));
            }
        }
        for a in &agent_allowed {
            if !actions.contains(a) {
                return Err(format!("agentAllowed names unknown action {a}"));
            }
        }
        // Moving funds out and managing agents is never agent-allowed, whatever the file says.
        for forbidden in ["Withdraw", "ApproveAgent", "RevokeAgent"] {
            if agent_allowed.iter().any(|a| a == forbidden) {
                return Err(format!("{forbidden} must never be agent-allowed"));
            }
        }
        let dom = td.get("domain").ok_or("typed data without domain")?;
        // SPEC 2.1: {PosternPerps, 1, 700770, verifyingContract = the vault}; the file may name the vault or leave
        // it to the genesis, never a different one
        let engine_domain = Domain {
            name: dom.get("name").and_then(|v| v.as_str()).ok_or("domain.name")?.to_string(),
            version: dom.get("version").and_then(|v| v.as_str()).ok_or("domain.version")?.to_string(),
            chain_id: dom.get("chainId").and_then(|v| v.as_u64()).ok_or("domain.chainId")?,
            verifying_contract: Some(vault),
        };
        if engine_domain.name != "PosternPerps" || engine_domain.version != "1" || engine_domain.chain_id != 700_770 {
            return Err("engine domain must be PosternPerps / 1 / 700770 + the vault (SPEC 2.1)".into());
        }
        if let Some(vc) = dom.get("verifyingContract").and_then(|v| v.as_str()) {
            if parse_address(vc)? != vault {
                return Err("typed-data verifyingContract differs from the genesis vault".into());
            }
        }

        let mj: serde_json::Value = serde_json::from_slice(markets_bytes).map_err(|e| format!("markets: {e}"))?;
        let mut markets = Vec::new();
        let mut ids = BTreeSet::new();
        let mut syms = BTreeSet::new();
        for m in mj.get("markets").and_then(|v| v.as_array()).ok_or("markets array")? {
            let symbol = m.get("symbol").and_then(|v| v.as_str()).ok_or("market symbol")?.to_string();
            let market_id = m.get("marketId").and_then(|v| v.as_u64()).ok_or("marketId")?;
            let pd = m.get("priceDecimals").and_then(|v| v.as_u64()).ok_or("priceDecimals")?;
            let sd = m.get("sizeDecimals").and_then(|v| v.as_u64()).ok_or("sizeDecimals")?;
            if market_id == 0 || market_id > u32::MAX as u64 || pd > 12 || sd > 12 {
                return Err(format!("market {symbol}: id or decimals out of range (marketId 0 means every market)"));
            }
            if !ids.insert(market_id) || !syms.insert(symbol.clone()) {
                return Err(format!("duplicate market {symbol}"));
            }
            markets.push(Market { symbol, market_id: market_id as u32, price_decimals: pd as u32, size_decimals: sd as u32, json: m.clone() });
        }
        let collateral_decimals = mj.pointer("/collateral/decimals").and_then(|v| v.as_u64()).unwrap_or(6) as u32;
        if collateral_decimals != 6 {
            return Err("collateral (bUSDC) has 6 decimals".into());
        }
        let withdrawal_fee = match mj.pointer("/defaults/withdrawalFee").and_then(|v| v.as_str()) {
            Some(s) => crate::util::decimal_to_scaled(s, 6, u64::MAX as u128)?,
            None => 0,
        };

        check_committee(&committee, threshold, "committee")?;
        check_committee(&oracle_committee, oracle_quorum, "oracle committee")?;
        if sequencer == [0u8; 20] || vault == [0u8; 20] || fee_recipient == [0u8; 20] {
            return Err("sequencer, vault and fee recipient must be set".into());
        }
        if checkpoint_interval == 0 {
            return Err("checkpoint interval must be at least 1".into());
        }
        let vault_domain = Domain { name: "PosternPerpsVault".into(), version: "1".into(), chain_id: 700_770, verifying_contract: Some(vault) };
        let mut g = Genesis {
            engine_domain,
            vault_domain,
            types: ActionTypes { registry, actions, agent_allowed },
            typed_data_json: td,
            typed_data_hash: keccak(typed_data_bytes),
            markets,
            markets_hash: keccak(markets_bytes),
            markets_defaults: mj.get("defaults").and_then(|v| v.as_object()).cloned().unwrap_or_default(),
            collateral_decimals,
            committee,
            threshold,
            oracle_committee,
            oracle_quorum,
            sequencer,
            vault,
            fee_recipient,
            withdrawal_fee,
            genesis_time,
            checkpoint_interval,
            log_hash0: [0u8; 32],
        };
        g.log_hash0 = keccak(&g.encode());
        Ok(g)
    }

    pub fn with_log_hash0(mut self, h: H256) -> Genesis {
        self.log_hash0 = h;
        self
    }

    /// The canonical encoding of the genesis; `logHash(0) = keccak256(encoding)`.
    pub fn encode(&self) -> Vec<u8> {
        let mut b = Vec::new();
        b.extend_from_slice(b"PosternPerps genesis v0\x00");
        b.extend_from_slice(&self.engine_domain.separator());
        b.extend_from_slice(&self.vault_domain.separator());
        b.extend_from_slice(&self.typed_data_hash);
        b.extend_from_slice(&self.markets_hash);
        b.extend_from_slice(&(self.committee.len() as u32).to_be_bytes());
        for a in &self.committee {
            b.extend_from_slice(a);
        }
        b.extend_from_slice(&(self.threshold as u32).to_be_bytes());
        b.extend_from_slice(&(self.oracle_committee.len() as u32).to_be_bytes());
        for a in &self.oracle_committee {
            b.extend_from_slice(a);
        }
        b.extend_from_slice(&(self.oracle_quorum as u32).to_be_bytes());
        b.extend_from_slice(&self.sequencer);
        b.extend_from_slice(&self.vault);
        b.extend_from_slice(&self.fee_recipient);
        b.extend_from_slice(&self.withdrawal_fee.to_be_bytes());
        b.extend_from_slice(&self.genesis_time.to_be_bytes());
        b.extend_from_slice(&self.checkpoint_interval.to_be_bytes());
        b
    }

    pub fn genesis_log_hash(&self) -> H256 {
        self.log_hash0
    }

    pub fn committee_set_hash(&self) -> H256 {
        let mut parts: Vec<&[u8]> = vec![b"PosternPerps committee v0"];
        let t = (self.threshold as u32).to_be_bytes();
        parts.push(&t);
        for a in &self.committee {
            parts.push(a);
        }
        keccak_parts(&parts)
    }

    pub fn market(&self, id: u32) -> Option<&Market> {
        self.markets.iter().find(|m| m.market_id == id)
    }

    pub fn market_by_symbol(&self, s: &str) -> Option<&Market> {
        self.markets.iter().find(|m| m.symbol == s)
    }

    pub fn committee_json(&self) -> serde_json::Value {
        serde_json::json!({
            "threshold": self.threshold,
            "validators": self.committee.iter().map(|a| to_hex(a)).collect::<Vec<_>>(),
            "setHash": to_hex(&self.committee_set_hash()),
        })
    }
}

fn check_committee(c: &[Address], k: usize, what: &str) -> Result<(), String> {
    if c.is_empty() || c.len() > 64 {
        return Err(format!("{what}: size 1..64"));
    }
    if k == 0 || k > c.len() || 2 * k <= c.len() {
        return Err(format!("{what}: threshold {k} of {} must be a strict majority and at most n", c.len()));
    }
    // strictly ascending: one canonical order, no duplicates (the vault requires ascending signers)
    for w in c.windows(2) {
        if w[0] >= w[1] {
            return Err(format!("{what}: addresses must be strictly ascending"));
        }
    }
    if c.iter().any(|a| *a == [0u8; 20]) {
        return Err(format!("{what}: zero address"));
    }
    Ok(())
}

pub fn parse_address_list(v: &[String]) -> Result<Vec<Address>, String> {
    v.iter().map(|s| parse_address(s)).collect()
}

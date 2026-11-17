//! Throwaway-key helpers for tests, the fuzzer, the benchmark and the demo log. NEVER a real key: every key here
//! is derived from a small public integer.
use crate::eip712::{address_of, digest, oracle_struct_hash, sign_digest, struct_hash};
use crate::engine::Engine;
use crate::genesis::{Checked, FeeTier, Genesis, MarketParams, OracleParams, Tier, OWNER_FEE_RECIPIENT};
use crate::hash::{hex0x, keccak, Addr};
use crate::types::*;
use k256::ecdsa::SigningKey;

pub const DEV_VAULT: &str = "0x00000000000000000000000000000000000ba417";

pub fn key(n: u32) -> SigningKey {
    let h = keccak(format!("postern-perps throwaway test key {n}").as_bytes());
    SigningKey::from_slice(&h).expect("valid scalar")
}

pub fn addr(k: &SigningKey) -> Addr {
    address_of(k.verifying_key())
}

pub fn oracle_keys(n: u32) -> Vec<SigningKey> {
    (0..n).map(|i| key(1_000_000 + i)).collect()
}

pub fn market(id: u32, symbol: &str) -> MarketParams {
    MarketParams {
        market_id: id,
        symbol: symbol.into(),
        tick: 10_000_000,          // 0.1 USD
        step: 100_000,             // 0.001 base
        min_size: 100_000,
        max_size: 10_000_000_000,  // 100 base
        oi_cap: 100_000_000_000,   // 1000 base
        max_leverage: 50,
        default_leverage: 10,
        tiers: vec![
            Tier { floor: 0, imf_ppm: 20_000, mmf_ppm: 10_000 },
            Tier { floor: 1_000_000_000_000, imf_ppm: 40_000, mmf_ppm: 20_000 },
        ],
        taker_fee_ppm: 450,
        maker_fee_ppm: 150,
        liquidation_fee_ppm: 5_000,
        mark_band_bps: 50,
        market_slippage_bps: 500,
        impact_notional: 10_000_000_000,
        funding_interval_ms: 3_600_000,
        interest: 12_500,
        funding_clamp: 500_000,
        max_funding_rate: 500_000,
        oracle: OracleParams { min_sources: 3, agree_bps: 100, jump_bps: 500, max_staleness_ms: 10_000, confirm_rounds: 1, exit_haircut_bps: 500, exit_haircut_per_min_bps: 100, exit_max_haircut_bps: 5000, max_halt_ms: 3_600_000 },
    }
}

pub fn genesis(n_oracles: u32, markets: Vec<MarketParams>) -> Checked {
    genesis_raw(n_oracles, markets).check().expect("test genesis valid")
}

pub fn genesis_raw(n_oracles: u32, markets: Vec<MarketParams>) -> Genesis {
    Genesis {
        spec_version: "0.1".into(),
        chain_id: 700770,
        verifying_contract: DEV_VAULT.into(),
        fee_recipient: OWNER_FEE_RECIPIENT.into(),
        oracle_signers: oracle_keys(n_oracles).iter().map(|k| hex0x(&addr(k))).collect(),
        oracle_quorum: (n_oracles / 2 + 1) as u8 | 1,
        oracle_max_future_ms: 1000,
        oracle_max_age_ms: 6000,
        withdrawal_fee: 1_000_000,
        min_fee_sweep: 1_000_000,
        fee_tiers: vec![FeeTier { min_volume_14d: 0, taker_fee_ppm: 450, maker_fee_ppm: 150 }],
        launch_insurance_min: 0,
        launch_backstop_min: 0,
        backstop_oi_ppm: 250_000,
        markets,
    }
}

pub fn sign_action(e: &Engine, k: &SigningKey, a: Action) -> Input {
    let d = digest(&e.domain_sep, &struct_hash(&a));
    Input::Action(SignedAction { action: a, signature: sign_digest(k, &d) })
}

/// One oracle round where every signer reports the same prices for every market (block time `t`).
pub fn oracle_round(e: &Engine, keys: &[SigningKey], t: u64, prices: &[(u32, u64)]) -> Input {
    let round = t / crate::oracle::ROUND_MS;
    let time_ms = round * crate::oracle::ROUND_MS;
    let ps: Vec<OraclePrice> = prices.iter().map(|(m, p)| OraclePrice { market_id: *m, price: *p, sources: 5 }).collect();
    let q = e.g.g.oracle_quorum as usize;
    let mut obs = Vec::new();
    for (i, k) in keys.iter().enumerate().take(q.max(keys.len())) {
        let d = digest(&e.domain_sep, &oracle_struct_hash(round, time_ms, &ps));
        obs.push(SignedObservation { signer: i as u8, time_ms, prices: ps.clone(), signature: sign_digest(k, &d) });
    }
    Input::Oracle(OracleUpdate { round, observations: obs })
}

pub fn place(account: Addr, nonce: u64, market_id: u32, is_buy: bool, order_type: u8, tif: u8, price: u64, size: u64, reduce_only: bool) -> Action {
    Action::PlaceOrder(PlaceOrder {
        account,
        nonce,
        market_id,
        is_buy,
        order_type,
        time_in_force: tif,
        price,
        size,
        reduce_only,
        trigger_price: 0,
        client_order_id: [0u8; 32],
    })
}

//! PosternDEX Perpetuals price oracle: deterministic rules. UNAUDITED PROTOTYPE.
//!
//! Normative text: `node/oracle/README.md`. Section numbers in comments (C1, H2, F4, ...) refer to it.
//!
//! Everything in this crate is integer arithmetic on explicit inputs: no floats, no clock, no randomness, no I/O.
//! The same inputs give the same outputs and the same canonical state bytes on every machine, so the engine can
//! call [`committee::apply_update`] while replaying the input log and validators agree on the state root.
//!
//! Modules:
//! - [`decimal`]: exact decimal-text parsing at 18 decimals and the rounding to a market's price scale.
//! - [`types`]: the observation, the signed observation and the block input `OracleUpdate`.
//! - [`encoding`]: canonical bytes of `OracleUpdate` (strict decoder: no trailing bytes, bounded counts).
//! - [`eip712`]: the `OracleObservation` typed-data digest in the `PosternPerps` domain.
//! - [`sig`]: 65-byte secp256k1 signatures (low-s, v 27/28) and address recovery.
//! - [`committee`]: the engine's acceptance rule (C1 to C5) and the per-market halt machine (H1 to H3).
//! - [`feeder_rule`]: one validator's aggregation of venue readings into an observation (F2 to F6).
//! - [`selection`]: the exact reach of the sequencer's choice of observations (R395-9) and the omission check.

#![forbid(unsafe_code)]

pub mod committee;
pub mod decimal;
pub mod eip712;
pub mod encoding;
pub mod feeder_rule;
pub mod selection;
pub mod sig;
pub mod types;

/// Length of an oracle round in milliseconds (README section 2).
pub const ROUND_MS: u64 = 3000;
/// The engine's largest price, `MAX_PRICE = 10^15` units of 1e-8 USD = 10,000,000 USD (`docs/SPEC.md` section 1).
/// A committee value above it is not a price (rule C3).
pub const MAX_PRICE: u64 = 1_000_000_000_000_000;
/// Maximum number of markets in one observation.
pub const MAX_PRICES: usize = 256;

/// keccak256 (the Ethereum variant, not NIST SHA3-256).
pub fn keccak256(data: &[u8]) -> [u8; 32] {
    use sha3::{Digest, Keccak256};
    let mut h = Keccak256::new();
    h.update(data);
    h.finalize().into()
}

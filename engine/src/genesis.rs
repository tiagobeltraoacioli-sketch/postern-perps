//! Genesis parameters (SPEC section 10). Integers are decimal strings in engine units (section 1); unknown
//! fields are refused; every consistency rule is checked before an engine is created.
use crate::hash::{keccak, parse_addr, Addr, H256};
use crate::num::{MAX_PRICE, MAX_SIZE, PPM};
use serde::{Deserialize, Serialize};

/// Integers as decimal strings (no floats anywhere).
pub mod strnum {
    use serde::{de::Error, Deserialize, Deserializer, Serializer};
    use std::str::FromStr;
    pub fn serialize<S: Serializer, T: ToString>(v: &T, s: S) -> Result<S::Ok, S::Error> {
        s.serialize_str(&v.to_string())
    }
    pub fn deserialize<'de, D: Deserializer<'de>, T: FromStr>(d: D) -> Result<T, D::Error> {
        let s = String::deserialize(d)?;
        if s.is_empty() || !(s.bytes().all(|c| c.is_ascii_digit()) || (s.starts_with('-') && s.len() > 1 && s[1..].bytes().all(|c| c.is_ascii_digit()))) {
            return Err(D::Error::custom("integer must be a decimal string"));
        }
        if (s.len() > 1 && s.starts_with('0')) || s.starts_with("-0") {
            return Err(D::Error::custom("non-canonical integer (leading zero)"));
        }
        T::from_str(&s).map_err(|_| D::Error::custom("integer out of range"))
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct Tier {
    /// Lower bound of the tier, in collateral units of notional (USDC 1e-6).
    #[serde(with = "strnum")]
    pub floor: u64,
    #[serde(with = "strnum")]
    pub imf_ppm: u32,
    #[serde(with = "strnum")]
    pub mmf_ppm: u32,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct OracleParams {
    #[serde(with = "strnum")]
    pub min_sources: u8,
    #[serde(with = "strnum")]
    pub agree_bps: u32,
    #[serde(with = "strnum")]
    pub jump_bps: u32,
    #[serde(with = "strnum")]
    pub max_staleness_ms: u64,
    #[serde(with = "strnum")]
    pub confirm_rounds: u32,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct MarketParams {
    #[serde(with = "strnum")]
    pub market_id: u32,
    pub symbol: String,
    #[serde(with = "strnum")]
    pub tick: u64,
    #[serde(with = "strnum")]
    pub step: u64,
    #[serde(with = "strnum")]
    pub min_size: u64,
    #[serde(with = "strnum")]
    pub max_size: u64,
    /// Open-interest cap in size units (sum of long sizes).
    #[serde(with = "strnum")]
    pub oi_cap: u64,
    #[serde(with = "strnum")]
    pub max_leverage: u32,
    #[serde(with = "strnum")]
    pub default_leverage: u32,
    pub tiers: Vec<Tier>,
    #[serde(with = "strnum")]
    pub taker_fee_ppm: u32,
    #[serde(with = "strnum")]
    pub maker_fee_ppm: u32,
    #[serde(with = "strnum")]
    pub liquidation_fee_ppm: u32,
    #[serde(with = "strnum")]
    pub mark_band_bps: u32,
    #[serde(with = "strnum")]
    pub market_slippage_bps: u32,
    /// Impact notional for the premium sample, in collateral units.
    #[serde(with = "strnum")]
    pub impact_notional: u64,
    #[serde(with = "strnum")]
    pub funding_interval_ms: u64,
    #[serde(with = "strnum")]
    pub interest: i64,
    #[serde(with = "strnum")]
    pub interest_clamp: i64,
    #[serde(with = "strnum")]
    pub max_funding_rate: i64,
    pub oracle: OracleParams,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct Genesis {
    pub spec_version: String,
    #[serde(with = "strnum")]
    pub chain_id: u64,
    pub verifying_contract: String,
    pub fee_recipient: String,
    pub oracle_signers: Vec<String>,
    #[serde(with = "strnum")]
    pub oracle_quorum: u8,
    #[serde(with = "strnum")]
    pub oracle_max_future_ms: u64,
    #[serde(with = "strnum")]
    pub oracle_max_age_ms: u64,
    #[serde(with = "strnum")]
    pub withdrawal_fee: u64,
    #[serde(with = "strnum")]
    pub min_fee_sweep: u64,
    pub markets: Vec<MarketParams>,
}

/// The owner's fee address (BRIEF-PERPS "Fees and revenue"); the vault fixes the same constant.
pub const OWNER_FEE_RECIPIENT: &str = "0x775a993F09Bd80aeCF8F8c2ADe0C84158E3a0544";

#[derive(Clone, Debug)]
pub struct Checked {
    pub g: Genesis,
    pub verifying_contract: Addr,
    pub fee_recipient: Addr,
    pub oracle_signers: Vec<Addr>,
    pub hash: H256,
}

impl Genesis {
    pub fn from_json(s: &str) -> Result<Checked, String> {
        let g: Genesis = serde_json::from_str(s).map_err(|e| format!("genesis: {e}"))?;
        g.check()
    }

    /// Canonical encoding: the engine's serialization (fields in declaration order, no whitespace, integers as
    /// decimal strings). `logHash(0)` commits to it (SPEC 3.1).
    pub fn encoding(&self) -> Vec<u8> {
        serde_json::to_vec(self).expect("serialize")
    }

    pub fn check(self) -> Result<Checked, String> {
        if self.spec_version != "0.1" {
            return Err("specVersion must be 0.1".into());
        }
        if self.chain_id != 700770 {
            return Err("chainId must be 700770 (SPEC 2.1)".into());
        }
        let vc = parse_addr(&self.verifying_contract)?;
        if vc == [0u8; 20] {
            return Err("verifyingContract must not be zero".into());
        }
        let fr = parse_addr(&self.fee_recipient)?;
        if fr != parse_addr(OWNER_FEE_RECIPIENT).unwrap() {
            return Err("feeRecipient must be the owner's fee address (SPEC S-9.3)".into());
        }
        let mut signers = Vec::new();
        for s in &self.oracle_signers {
            let a = parse_addr(s)?;
            if a == [0u8; 20] || signers.contains(&a) {
                return Err("oracle signers must be distinct and non-zero".into());
            }
            signers.push(a);
        }
        let n = signers.len();
        if n == 0 || n > 255 {
            return Err("oracle committee size".into());
        }
        let q = self.oracle_quorum as usize;
        // Median safety needs q = 2f+1 with f liars tolerated; q must be odd, > n/2, <= n.
        if q == 0 || q > n || q * 2 <= n || q % 2 == 0 {
            return Err("oracle quorum must be odd, a strict majority and <= n".into());
        }
        if self.markets.is_empty() || self.markets.len() > 255 {
            return Err("markets count".into());
        }
        let mut prev = 0u32;
        for m in &self.markets {
            if m.market_id <= prev || m.market_id > 255 {
                return Err(format!("market ids must be ascending in 1..=255 ({})", m.market_id));
            }
            prev = m.market_id;
            if m.tick == 0 || m.tick > MAX_PRICE || m.step == 0 || m.step > MAX_SIZE {
                return Err(format!("{}: tick/step", m.symbol));
            }
            if m.min_size == 0 || m.min_size % m.step != 0 || m.max_size % m.step != 0 || m.max_size < m.min_size || m.max_size > MAX_SIZE {
                return Err(format!("{}: min/max size", m.symbol));
            }
            if m.max_leverage == 0 || m.default_leverage == 0 || m.default_leverage > m.max_leverage {
                return Err(format!("{}: leverage", m.symbol));
            }
            if m.tiers.is_empty() {
                return Err(format!("{}: tiers", m.symbol));
            }
            let mut pf: Option<u64> = None;
            for t in &m.tiers {
                if let Some(p) = pf {
                    if t.floor <= p {
                        return Err(format!("{}: tier floors must ascend", m.symbol));
                    }
                } else if t.floor != 0 {
                    return Err(format!("{}: first tier floor must be 0", m.symbol));
                }
                pf = Some(t.floor);
                if t.mmf_ppm == 0 || t.imf_ppm <= t.mmf_ppm || t.imf_ppm as i128 > PPM {
                    return Err(format!("{}: need 0 < mmf < imf <= 1e6", m.symbol));
                }
                // S-5.3: maintenance must exceed the mark band (ppm vs bps*100).
                if (t.mmf_ppm as u64) <= (m.mark_band_bps as u64) * 100 {
                    return Err(format!("{}: mmf must exceed markBandBps (S-5.3)", m.symbol));
                }
            }
            // max leverage consistent with the first tier's imf
            if (m.max_leverage as i128) * (m.tiers[0].imf_ppm as i128) > PPM {
                return Err(format!("{}: maxLeverage above 1/imf of tier 0", m.symbol));
            }
            if m.taker_fee_ppm as i128 > PPM / 100 || m.maker_fee_ppm as i128 > PPM / 100 || m.liquidation_fee_ppm as i128 > PPM / 10 {
                return Err(format!("{}: fee bounds", m.symbol));
            }
            if m.mark_band_bps == 0 || m.mark_band_bps > 1000 || m.market_slippage_bps == 0 || m.market_slippage_bps > 5000 {
                return Err(format!("{}: band/slippage bounds", m.symbol));
            }
            if m.funding_interval_ms == 0 || m.max_funding_rate <= 0 || m.max_funding_rate > 10_000_000 || m.interest.abs() > m.max_funding_rate || m.interest_clamp < 0 {
                return Err(format!("{}: funding bounds (S-1.3)", m.symbol));
            }
            if m.oracle.min_sources == 0 || m.oracle.agree_bps == 0 || m.oracle.jump_bps == 0 || m.oracle.confirm_rounds == 0 || m.oracle.max_staleness_ms == 0 {
                return Err(format!("{}: oracle params", m.symbol));
            }
        }
        let hash = keccak(&self.encoding());
        Ok(Checked { verifying_contract: vc, fee_recipient: fr, oracle_signers: signers, hash, g: self })
    }
}

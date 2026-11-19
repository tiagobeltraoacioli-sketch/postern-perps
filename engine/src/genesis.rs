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
    /// Exit band of a halted market (394's README 6a, X1): base half-width, growth per minute, cap (bps).
    #[serde(with = "strnum")]
    pub exit_haircut_bps: u32,
    #[serde(with = "strnum")]
    pub exit_haircut_per_min_bps: u32,
    #[serde(with = "strnum")]
    pub exit_max_haircut_bps: u32,
    /// X3: a halt this old settles every position of the market at the settle price.
    #[serde(with = "strnum")]
    pub max_halt_ms: u64,
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
    pub funding_clamp: i64,
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
    /// Volume fee tiers (SPEC S-9.4): tier 0 has minVolume14d 0 and equals every market's fees.
    pub fee_tiers: Vec<FeeTier>,
    /// Launch gate (SPEC S-4.11): no order is accepted until INSURANCE and BACKSTOP balances reach these.
    #[serde(with = "strnum")]
    pub launch_insurance_min: u64,
    #[serde(with = "strnum")]
    pub launch_backstop_min: u64,
    /// The backstop takes over at most this fraction (ppm) of a market's oiCap (SPEC 6.3 step 3).
    #[serde(with = "strnum")]
    pub backstop_oi_ppm: u32,
    pub markets: Vec<MarketParams>,
    /// The collateral token contract on the L2 (bUSDC), SPEC S-9.2.1 (R395-22): never a withdrawal recipient.
    /// Optional in 0.2 and LAST, omitted from the canonical encoding when absent, so a genesis written before 0.2
    /// keeps its `logHash(0)` and every root; a genesis bound to a real vault MUST set it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub collateral_token: Option<String>,
    /// SPEC 6.5 (0.2.5, R436-5): the part of the backstop's genesis seed (`launchBackstopMin`) it may lose, ppm.
    /// Its equity floor is `ceil(launchBackstopMin * (10^6 - budget) / 10^6)`. Optional and LAST, omitted from the
    /// canonical encoding when absent (old genesis hashes stand); absent = 10^6 (floor 0: the backstop is then
    /// stopped by its maintenance requirement alone).
    #[serde(default, skip_serializing_if = "Option::is_none", with = "optnum")]
    pub backstop_loss_budget_ppm: Option<u32>,
}

/// An optional integer as a decimal string (same strictness as `strnum`).
pub mod optnum {
    use serde::{Deserialize, Deserializer, Serializer};
    pub fn serialize<S: Serializer>(v: &Option<u32>, s: S) -> Result<S::Ok, S::Error> {
        match v {
            Some(x) => super::strnum::serialize(x, s),
            None => s.serialize_none(),
        }
    }
    pub fn deserialize<'de, D: Deserializer<'de>>(d: D) -> Result<Option<u32>, D::Error> {
        #[derive(Deserialize)]
        struct W(#[serde(with = "super::strnum")] u32);
        Ok(Some(W::deserialize(d)?.0))
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct FeeTier {
    /// Sum of the account's maker + taker notional over the 14 complete UTC days before today, USDC units.
    #[serde(with = "strnum")]
    pub min_volume_14d: u64,
    #[serde(with = "strnum")]
    pub taker_fee_ppm: u32,
    #[serde(with = "strnum")]
    pub maker_fee_ppm: u32,
}

/// The owner's fee address (BRIEF-PERPS "Fees and revenue"); the vault fixes the same constant.
pub const OWNER_FEE_RECIPIENT: &str = "0x775a993F09Bd80aeCF8F8c2ADe0C84158E3a0544";

#[derive(Clone, Debug)]
pub struct Checked {
    pub g: Genesis,
    pub verifying_contract: Addr,
    pub fee_recipient: Addr,
    pub oracle_signers: Vec<Addr>,
    pub collateral_token: Option<Addr>,
    /// SPEC 6.5: the backstop's equity floor, USDC units (rounded UP: toward stopping earlier).
    pub backstop_floor: u64,
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
        let token = match &self.collateral_token {
            None => None,
            Some(t) => {
                let a = parse_addr(t)?;
                if a[..18] == [0u8; 18] || a == vc || a == fr {
                    return Err("collateralToken must be a contract address distinct from the vault and the fee recipient".into());
                }
                Some(a)
            }
        };
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
        if self.fee_tiers.is_empty() || self.fee_tiers[0].min_volume_14d != 0 || self.fee_tiers.len() > 16 {
            return Err("feeTiers: 1 to 16 tiers, the first with minVolume14d 0".into());
        }
        for w in self.fee_tiers.windows(2) {
            if w[1].min_volume_14d <= w[0].min_volume_14d || w[1].taker_fee_ppm > w[0].taker_fee_ppm || w[1].maker_fee_ppm > w[0].maker_fee_ppm {
                return Err("feeTiers: volumes ascending, fees non-increasing".into());
            }
        }
        for t in &self.fee_tiers {
            if t.maker_fee_ppm > t.taker_fee_ppm || t.taker_fee_ppm as i128 > PPM / 100 {
                return Err("feeTiers: need 0 <= maker <= taker <= 1%".into());
            }
        }
        if self.backstop_oi_ppm as i128 > PPM {
            return Err("backstopOiPpm <= 1e6".into());
        }
        let budget = self.backstop_loss_budget_ppm.unwrap_or(1_000_000) as u128;
        if budget > 1_000_000 {
            return Err("backstopLossBudgetPpm <= 1e6".into());
        }
        let backstop_floor = ((self.launch_backstop_min as u128) * (1_000_000 - budget)).div_ceil(1_000_000) as u64;
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
            if m.taker_fee_ppm != self.fee_tiers[0].taker_fee_ppm || m.maker_fee_ppm != self.fee_tiers[0].maker_fee_ppm {
                return Err(format!("{}: market fees must equal fee tier 0 (S-9.4)", m.symbol));
            }
            if m.oi_cap == 0 || m.oi_cap > MAX_SIZE {
                return Err(format!("{}: oiCap in 1..=MAX_SIZE (bounds every position, SPEC section 1)", m.symbol));
            }
            if m.taker_fee_ppm as i128 > PPM / 100 || m.maker_fee_ppm as i128 > PPM / 100 || m.liquidation_fee_ppm as i128 > PPM / 10 {
                return Err(format!("{}: fee bounds", m.symbol));
            }
            if m.mark_band_bps == 0 || m.mark_band_bps > 1000 || m.market_slippage_bps == 0 || m.market_slippage_bps > 5000 {
                return Err(format!("{}: band/slippage bounds", m.symbol));
            }
            if m.impact_notional == 0 {
                // R436-8: the premium sample divides by the size an impactNotional order buys
                return Err(format!("{}: impactNotional must be at least 1", m.symbol));
            }
            if m.funding_interval_ms == 0 || m.max_funding_rate <= 0 || m.max_funding_rate > 10_000_000 || m.interest.abs() > m.max_funding_rate || m.funding_clamp < 0 {
                return Err(format!("{}: funding bounds (S-1.3)", m.symbol));
            }
            if m.oracle.min_sources == 0 || m.oracle.agree_bps == 0 || m.oracle.jump_bps == 0 || m.oracle.confirm_rounds == 0 || m.oracle.max_staleness_ms == 0 {
                return Err(format!("{}: oracle params", m.symbol));
            }
            let o = &m.oracle;
            if o.exit_haircut_bps < o.jump_bps || o.exit_max_haircut_bps < o.exit_haircut_bps || o.exit_max_haircut_bps >= 10_000 || o.max_halt_ms < o.max_staleness_ms {
                return Err(format!("{}: exit band (394 README 6a: exitHaircutBps >= jumpBps, max >= base, max < 10000, maxHaltMs >= maxStalenessMs)", m.symbol));
            }
        }
        let hash = keccak(&self.encoding());
        Ok(Checked { verifying_contract: vc, fee_recipient: fr, oracle_signers: signers, collateral_token: token, backstop_floor, hash, g: self })
    }
}

/// S-1.2: exact decimal string -> integer units with `decimals` fractional digits; refuses anything not exactly
/// representable (never rounds), signs, exponents, empty parts.
pub fn dec_units(s: &str, decimals: u32) -> Result<u64, String> {
    let (i, f) = match s.split_once('.') {
        Some((i, f)) => (i, f),
        None => (s, ""),
    };
    if i.is_empty() || !i.bytes().all(|c| c.is_ascii_digit()) || !f.bytes().all(|c| c.is_ascii_digit()) || (s.contains('.') && f.is_empty()) {
        return Err(format!("not a plain decimal: {s:?}"));
    }
    if f.len() as u32 > decimals {
        let (keep, rest) = f.split_at(decimals as usize);
        if rest.bytes().any(|c| c != b'0') {
            return Err(format!("{s:?} has more than {decimals} fractional digits (S-1.2: refused, not rounded)"));
        }
        return dec_units(&format!("{i}.{keep}"), decimals);
    }
    let mut digits = String::from(i);
    digits.push_str(f);
    for _ in 0..(decimals - f.len() as u32) {
        digits.push('0');
    }
    digits.trim_start_matches('0').parse::<u64>().or_else(|e| if digits.bytes().all(|c| c == b'0') { Ok(0) } else { Err(e) }).map_err(|_| format!("{s:?} out of range"))
}

fn jint(v: &serde_json::Value, k: &str) -> Result<i64, String> {
    match &v[k] {
        serde_json::Value::Number(n) => n.as_i64().ok_or(format!("{k}: integer expected")),
        serde_json::Value::String(s) => s.parse().map_err(|_| format!("{k}: integer expected")),
        _ => Err(format!("{k}: missing")),
    }
}

/// R436-8: a JSON integer that does not fit the field's type is REFUSED (S-0.4), never narrowed.
fn jnum<T: TryFrom<i64>>(v: &serde_json::Value, k: &str) -> Result<T, String> {
    T::try_from(jint(v, k)?).map_err(|_| format!("{k}: out of range"))
}

/// An OPTIONAL integer: the default only when the key is absent; a present value that does not fit is refused.
fn jopt<T: TryFrom<i64>>(v: &serde_json::Value, k: &str, default: T) -> Result<T, String> {
    if v.get(k).is_none_or(|x| x.is_null()) {
        Ok(default)
    } else {
        jnum(v, k)
    }
}

fn jstr<'a>(v: &'a serde_json::Value, k: &str) -> Result<&'a str, String> {
    v[k].as_str().ok_or(format!("{k}: string expected"))
}

/// Converter from 392's `compliance/markets.json` + `risk.json` (human units) to an engine genesis (engine units).
/// Owner and deployment inputs are explicit arguments; a `TBD-owner` value in the files is never guessed.
pub fn from_compliance(
    markets: &serde_json::Value,
    risk: &serde_json::Value,
    verifying_contract: &str,
    oracle_signers: Vec<String>,
    oracle_quorum: u8,
    backstop_seed_usdc: &str,
    backstop_oi_ppm: u32,
) -> Result<Genesis, String> {
    use crate::num::{PRICE_SCALE, SIZE_SCALE};
    const PD: u32 = 8; // PRICE_SCALE = 10^8
    const SD: u32 = 8; // SIZE_SCALE = 10^8
    const UD: u32 = 6; // USDC 10^6
    debug_assert_eq!(10u64.pow(PD), PRICE_SCALE);
    debug_assert_eq!(10u64.pow(SD), SIZE_SCALE);
    let g = &risk["global"];
    let mut ms = Vec::new();
    for m in markets["markets"].as_array().ok_or("markets[] missing")? {
        let tiers = m["tiers"].as_array().ok_or("tiers missing")?.iter().map(|t| {
            Ok(Tier { floor: dec_units(jstr(t, "floor")?, UD)?, imf_ppm: jnum::<u32>(t, "imfPpm")?, mmf_ppm: jnum::<u32>(t, "mmfPpm")? })
        }).collect::<Result<Vec<_>, String>>()?;
        let max_lev = jnum::<u32>(m, "maxLeverage")?;
        let o = &m["oracle"];
        let tick = dec_units(jstr(m, "tick")?, PD)?;
        let step = dec_units(jstr(m, "step")?, SD)?;
        // display decimals must match tick/step precision (SPEC section 1)
        for (k, unit, d) in [("priceDecimals", tick, PD), ("sizeDecimals", step, SD)] {
            let want = jnum::<u32>(m, k)?;
            if unit % 10u64.pow(d - want.min(d)) != 0 {
                return Err(format!("{}: {k} {want} finer than the tick/step", jstr(m, "symbol")?));
            }
        }
        ms.push(MarketParams {
            market_id: jnum::<u32>(m, "marketId")?,
            symbol: jstr(m, "symbol")?.to_string(),
            tick,
            step,
            min_size: dec_units(jstr(m, "minSize")?, SD)?,
            max_size: dec_units(jstr(m, "maxSize")?, SD)?,
            oi_cap: dec_units(jstr(m, "oiCap")?, SD)?,
            max_leverage: max_lev,
            // Decided (technical): default leverage = min(10, maxLeverage) unless the file names one.
            default_leverage: jopt::<u32>(m, "defaultLeverage", max_lev.min(10))?,
            tiers,
            taker_fee_ppm: jnum::<u32>(m, "takerFeePpm")?,
            maker_fee_ppm: jnum::<u32>(m, "makerFeePpm")?,
            liquidation_fee_ppm: jnum::<u32>(m, "liquidationFeePpm")?,
            mark_band_bps: jnum::<u32>(m, "markBandBps")?,
            market_slippage_bps: jnum::<u32>(m, "marketSlippageBps")?,
            impact_notional: dec_units(jstr(m, "impactNotional")?, UD)?,
            funding_interval_ms: jnum::<u64>(m, "fundingIntervalMs")?,
            interest: jint(m, "interest")?,
            funding_clamp: jint(m, "fundingClamp")?,
            max_funding_rate: jint(m, "maxFundingRate")?,
            oracle: OracleParams {
                min_sources: jnum::<u8>(o, "minSources")?,
                agree_bps: jnum::<u32>(o, "agreeBps")?,
                jump_bps: jnum::<u32>(o, "jumpBps")?,
                max_staleness_ms: jnum::<u64>(o, "maxStalenessMs")?,
                confirm_rounds: jnum::<u32>(o, "confirmRounds")?,
                // 394's README section 8 defaults when the file does not name them
                exit_haircut_bps: jopt::<u32>(o, "exitHaircutBps", jnum::<u32>(o, "jumpBps")?)?,
                exit_haircut_per_min_bps: jopt::<u32>(o, "exitHaircutPerMinBps", 100)?,
                exit_max_haircut_bps: jopt::<u32>(o, "exitMaxHaircutBps", 5000)?,
                max_halt_ms: jopt::<u64>(o, "maxHaltMs", 3_600_000)?,
            },
        });
    }
    let fee_tiers = risk["feeTiers"]["tiers"].as_array().ok_or("feeTiers.tiers missing")?.iter().map(|t| {
        Ok(FeeTier { min_volume_14d: dec_units(jstr(t, "minVolume14d")?, UD)?, taker_fee_ppm: jnum::<u32>(t, "takerFeePpm")?, maker_fee_ppm: jnum::<u32>(t, "makerFeePpm")? })
    }).collect::<Result<Vec<_>, String>>()?;
    let ins = jstr(&risk["insuranceFund"], "seedRequired")?;
    Genesis {
        spec_version: "0.1".into(),
        chain_id: jnum::<u64>(g, "feeRecipientChainId")?,
        verifying_contract: verifying_contract.into(),
        fee_recipient: jstr(g, "feeRecipient")?.into(),
        oracle_signers,
        oracle_quorum,
        oracle_max_future_ms: jnum::<u64>(g, "maxFutureMs")?,
        oracle_max_age_ms: jnum::<u64>(g, "maxAgeMs")?,
        withdrawal_fee: dec_units(jstr(g, "withdrawalFee")?, UD)?,
        min_fee_sweep: dec_units(jstr(g, "minFeeSweep")?, UD)?,
        fee_tiers,
        launch_insurance_min: dec_units(ins, UD)?,
        launch_backstop_min: dec_units(backstop_seed_usdc, UD)?,
        backstop_oi_ppm,
        markets: ms,
        // a deployment input (the bUSDC address of the vault's chain): the caller sets it; never guessed here
        collateral_token: None,
        // an owner input (SPEC 6.5): the caller sets it; absent = the maintenance stop alone
        backstop_loss_budget_ppm: None,
    }
    .check()
    .map(|c| c.g)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn exact_decimals() {
        assert_eq!(dec_units("0.5", 8), Ok(50_000_000));
        assert_eq!(dec_units("67250", 8), Ok(6_725_000_000_000));
        assert_eq!(dec_units("0.00000001", 8), Ok(1));
        assert_eq!(dec_units("1.230", 2), Ok(123));
        assert_eq!(dec_units("0", 6), Ok(0));
        assert!(dec_units("0.000000001", 8).is_err(), "more digits than the scale: refused");
        assert!(dec_units("-1", 6).is_err());
        assert!(dec_units("1e3", 6).is_err());
        assert!(dec_units(".5", 6).is_err());
        assert!(dec_units("5.", 6).is_err());
        assert!(dec_units("TBD-owner", 6).is_err());
    }
}

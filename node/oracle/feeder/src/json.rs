//! JSON forms (README section 4; conventions of `docs/API.md`: prices as decimal strings of the scaled integer,
//! times and rounds as numbers, bytes as 0x-hex). Parsing is strict: unknown or missing fields, wrong types and
//! non-canonical hex are refused.

use perps_oracle::committee::{
    CommitteeParams, MarketOutcome, MarketParams, OracleError, UpdateReport,
};
use perps_oracle::types::{OraclePrice, OracleUpdate, SignedObservation};
use serde_json::{json, Map, Value};

use crate::pipeline::{fmt18, RoundReport, SourceReport};

pub type R<T> = Result<T, String>;

fn obj<'a>(v: &'a Value, what: &str, allowed: &[&str]) -> R<&'a Map<String, Value>> {
    let o = v.as_object().ok_or(format!("{what}: not an object"))?;
    for k in o.keys() {
        if !allowed.contains(&k.as_str()) {
            return Err(format!("{what}: unknown field {k}"));
        }
    }
    Ok(o)
}

fn u64f(o: &Map<String, Value>, k: &str) -> R<u64> {
    o.get(k)
        .and_then(Value::as_u64)
        .ok_or(format!("field {k}: not an unsigned integer"))
}

fn u64s(o: &Map<String, Value>, k: &str) -> R<u64> {
    let s = o
        .get(k)
        .and_then(Value::as_str)
        .ok_or(format!("field {k}: not a decimal string"))?;
    if s.is_empty()
        || s.len() > 20
        || !s.bytes().all(|b| b.is_ascii_digit())
        || (s.len() > 1 && s.starts_with('0'))
    {
        return Err(format!("field {k}: not a canonical decimal string"));
    }
    s.parse().map_err(|_| format!("field {k}: out of range"))
}

pub fn hex_bytes<const N: usize>(s: &str, what: &str) -> R<[u8; N]> {
    let h = s.strip_prefix("0x").ok_or(format!("{what}: missing 0x"))?;
    if h.len() != 2 * N || h.bytes().any(|b| b.is_ascii_uppercase()) {
        return Err(format!("{what}: expected {N} bytes of lowercase hex"));
    }
    let v = hex::decode(h).map_err(|_| format!("{what}: bad hex"))?;
    Ok(v.try_into().unwrap())
}

pub fn price_to_json(p: &OraclePrice) -> Value {
    json!({"marketId": p.market_id, "price": p.price.to_string(), "sources": p.sources})
}

pub fn price_from_json(v: &Value) -> R<OraclePrice> {
    let o = obj(v, "price", &["marketId", "price", "sources"])?;
    let market_id =
        u32::try_from(u64f(o, "marketId")?).map_err(|_| "marketId out of range".to_string())?;
    let sources =
        u8::try_from(u64f(o, "sources")?).map_err(|_| "sources out of range".to_string())?;
    Ok(OraclePrice {
        market_id,
        price: u64s(o, "price")?,
        sources,
    })
}

pub fn signed_to_json(o: &SignedObservation, round: u64) -> Value {
    json!({
        "round": round,
        "signer": o.signer,
        "timeMs": o.time_ms,
        "prices": o.prices.iter().map(price_to_json).collect::<Vec<_>>(),
        "signature": format!("0x{}", hex::encode(o.signature)),
    })
}

fn signed_body(o: &Map<String, Value>) -> R<SignedObservation> {
    let signer = u8::try_from(u64f(o, "signer")?).map_err(|_| "signer out of range".to_string())?;
    let prices = o
        .get("prices")
        .and_then(Value::as_array)
        .ok_or("prices: not an array")?
        .iter()
        .map(price_from_json)
        .collect::<R<Vec<_>>>()?;
    let sig = o
        .get("signature")
        .and_then(Value::as_str)
        .ok_or("signature: not a string")?;
    Ok(SignedObservation {
        signer,
        time_ms: u64f(o, "timeMs")?,
        prices,
        signature: hex_bytes::<65>(sig, "signature")?,
    })
}

/// A feeder's submission: `{"round", "signer", "timeMs", "prices", "signature"}`.
pub fn signed_from_json(v: &Value) -> R<(u64, SignedObservation)> {
    let o = obj(
        v,
        "observation",
        &["round", "signer", "timeMs", "prices", "signature"],
    )?;
    Ok((u64f(o, "round")?, signed_body(o)?))
}

pub fn update_to_json(u: &OracleUpdate) -> Value {
    json!({
        "round": u.round,
        "observations": u.observations.iter().map(|o| {
            let mut v = signed_to_json(o, u.round);
            v.as_object_mut().unwrap().remove("round");
            v
        }).collect::<Vec<_>>(),
    })
}

pub fn update_from_json(v: &Value) -> R<OracleUpdate> {
    let o = obj(v, "update", &["round", "observations"])?;
    let observations = o
        .get("observations")
        .and_then(Value::as_array)
        .ok_or("observations: not an array")?
        .iter()
        .map(|e| {
            obj(
                e,
                "observation",
                &["signer", "timeMs", "prices", "signature"],
            )
            .and_then(signed_body)
        })
        .collect::<R<Vec<_>>>()?;
    Ok(OracleUpdate {
        round: u64f(o, "round")?,
        observations,
    })
}

pub fn committee_from_json(v: &Value) -> R<CommitteeParams> {
    let o = obj(
        v,
        "committee",
        &[
            "_note",
            "verifyingContract",
            "signers",
            "quorum",
            "maxFutureMs",
            "maxAgeMs",
            "markets",
        ],
    )?;
    let vault = o
        .get("verifyingContract")
        .and_then(Value::as_str)
        .ok_or("verifyingContract: not a string")?;
    let verifying_contract = hex_bytes::<20>(&vault.to_lowercase(), "verifyingContract")?;
    let signers = o
        .get("signers")
        .and_then(Value::as_array)
        .ok_or("signers: not an array")?
        .iter()
        .map(|s| {
            s.as_str()
                .ok_or("signer: not a string".to_string())
                .and_then(|s| hex_bytes::<20>(&s.to_lowercase(), "signer"))
        })
        .collect::<R<Vec<_>>>()?;
    let markets = o
        .get("markets")
        .and_then(Value::as_array)
        .ok_or("markets: not an array")?
        .iter()
        .map(|m| {
            let o = obj(
                m,
                "market",
                &[
                    "marketId",
                    "symbol",
                    "minSources",
                    "agreeBps",
                    "jumpBps",
                    "maxStalenessMs",
                    "confirmRounds",
                    "exitHaircutBps",
                    "exitHaircutPerMinBps",
                    "exitMaxHaircutBps",
                    "maxHaltMs",
                ],
            )?;
            let small = |k: &str, max: u64| -> R<u64> {
                let v = u64f(o, k)?;
                if v > max {
                    return Err(format!("{k} out of range"));
                }
                Ok(v)
            };
            Ok(MarketParams {
                market_id: small("marketId", u64::from(u32::MAX))? as u32,
                min_sources: small("minSources", 255)? as u8,
                agree_bps: small("agreeBps", 10_000)? as u32,
                jump_bps: small("jumpBps", 10_000)? as u32,
                max_staleness_ms: u64f(o, "maxStalenessMs")?,
                confirm_rounds: small("confirmRounds", 255)? as u8,
                exit_haircut_bps: small("exitHaircutBps", 10_000)? as u32,
                exit_haircut_per_min_bps: small("exitHaircutPerMinBps", 10_000)? as u32,
                exit_max_haircut_bps: small("exitMaxHaircutBps", 10_000)? as u32,
                max_halt_ms: u64f(o, "maxHaltMs")?,
            })
        })
        .collect::<R<Vec<_>>>()?;
    let quorum = u8::try_from(u64f(o, "quorum")?).map_err(|_| "quorum out of range".to_string())?;
    let p = CommitteeParams {
        verifying_contract,
        signers,
        quorum,
        max_future_ms: u64f(o, "maxFutureMs")?,
        max_age_ms: u64f(o, "maxAgeMs")?,
        markets,
    };
    p.validate()
        .map_err(|e| format!("committee parameters refused: {e:?}"))?;
    Ok(p)
}

pub fn outcome_to_json(o: &MarketOutcome) -> Value {
    match *o {
        MarketOutcome::NoQuorum { have } => json!({"outcome": "noQuorum", "have": have}),
        MarketOutcome::Disagree { median, agree } => {
            json!({"outcome": "disagree", "median": median.to_string(), "agree": agree})
        }
        MarketOutcome::Moved { price } => json!({"outcome": "moved", "price": price.to_string()}),
        MarketOutcome::HaltedJump { price } => {
            json!({"outcome": "haltedJump", "price": price.to_string()})
        }
        MarketOutcome::Confirming { price, count } => {
            json!({"outcome": "confirming", "price": price.to_string(), "count": count})
        }
        MarketOutcome::Resumed { price, confirmed } => {
            json!({"outcome": "resumed", "price": price.to_string(), "confirmed": confirmed})
        }
    }
}

pub fn report_to_json(r: &UpdateReport) -> Value {
    json!({"round": r.round, "markets": r.outcomes.iter().map(|(m, o)| {
        let mut v = outcome_to_json(o);
        v.as_object_mut().unwrap().insert("marketId".into(), json!(m));
        v
    }).collect::<Vec<_>>()})
}

pub fn error_to_json(e: &OracleError) -> Value {
    json!({"error": e.code(), "detail": format!("{e:?}")})
}

fn source_json(s: &SourceReport) -> Value {
    json!({"name": s.name, "price": s.price18.map(|p| fmt18(p, 8)), "time": s.time, "used": s.used, "reason": s.reason})
}

/// The feeder's per-round report, shaped like `OracleStatus` of `docs/API.md` per market.
pub fn round_report_to_json(r: &RoundReport) -> Value {
    json!({
        "round": r.round,
        "timeMs": r.time_ms,
        "usdtUsd": r.usdt_rate18.map(|p| fmt18(p, 8)),
        "usdtSources": r.usdt_sources.iter().map(source_json).collect::<Vec<_>>(),
        "markets": r.markets.iter().map(|m| json!({
            "marketId": m.market_id,
            "symbol": m.symbol,
            "price": m.result.as_ref().ok().map(|p| p.price.to_string()),
            "sources": m.result.as_ref().ok().map(|p| p.sources),
            "abstain": m.result.as_ref().err(),
            "venues": m.sources.iter().map(source_json).collect::<Vec<_>>(),
        })).collect::<Vec<_>>(),
    })
}

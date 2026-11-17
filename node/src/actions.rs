//! User actions: parsing of `POST /v1/actions` requests, EIP-712 digest, strict signature recovery, action hash.
//! The same `verify_signed_action` runs in the engine (deterministic, during replay) and in admission (a
//! pre-filter only), proposal (A) of the 03:50 UTC cross-area line.
//!
//! UNAUDITED PROTOTYPE.

use crate::eip712::{digest, Val};
use crate::genesis::Genesis;
use crate::sig::{recover, SigError};
use crate::types::SignedAction;
use crate::util::{decimal_to_scaled, keccak_parts, parse_hex_fixed, Address, H256};

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct VerifiedAction {
    pub primary: String,
    pub account: Address,
    pub signer: Address,
    pub nonce: u64,
    pub digest: H256,
    /// `keccak256(digest || signer)`: a main key and its agent may sign identical messages.
    pub action_hash: H256,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ActionError {
    Malformed(String),
    BadSignature(SigError),
}

impl std::fmt::Display for ActionError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ActionError::Malformed(s) => write!(f, "malformed action: {s}"),
            ActionError::BadSignature(e) => write!(f, "bad signature: {e}"),
        }
    }
}

pub fn field<'a>(g: &Genesis, primary: &str, msg: &'a Val, name: &str) -> Option<&'a Val> {
    let idx = g.types.registry.member_index(primary, name)?;
    match msg {
        Val::Struct(f) => f.get(idx),
        _ => None,
    }
}

pub fn field_u(g: &Genesis, primary: &str, msg: &Val, name: &str) -> Option<u128> {
    match field(g, primary, msg, name)? {
        Val::Uint(x) => Some(*x),
        _ => None,
    }
}

pub fn field_i(g: &Genesis, primary: &str, msg: &Val, name: &str) -> Option<i128> {
    match field(g, primary, msg, name)? {
        Val::Int(x) => Some(*x),
        _ => None,
    }
}

pub fn field_addr(g: &Genesis, primary: &str, msg: &Val, name: &str) -> Option<Address> {
    match field(g, primary, msg, name)? {
        Val::Address(a) => Some(*a),
        _ => None,
    }
}

pub fn field_bool(g: &Genesis, primary: &str, msg: &Val, name: &str) -> Option<bool> {
    match field(g, primary, msg, name)? {
        Val::Bool(b) => Some(*b),
        _ => None,
    }
}

pub fn action_digest(g: &Genesis, primary: &str, msg: &Val) -> Result<H256, String> {
    let sh = g.types.registry.hash_struct(primary, msg)?;
    Ok(digest(&g.engine_domain.separator(), &sh))
}

/// Signature and shape check. Authority (signer is the account or an approved agent) and the nonce rule need
/// state and are checked by the engine.
pub fn verify_signed_action(g: &Genesis, a: &SignedAction) -> Result<VerifiedAction, ActionError> {
    let primary = g.types.name(a.type_index).ok_or_else(|| ActionError::Malformed("unknown action type".into()))?.to_string();
    let account = field_addr(g, &primary, &a.message, "account").ok_or_else(|| ActionError::Malformed("account".into()))?;
    let nonce = field_u(g, &primary, &a.message, "nonce").ok_or_else(|| ActionError::Malformed("nonce".into()))? as u64;
    let d = action_digest(g, &primary, &a.message).map_err(ActionError::Malformed)?;
    let signer = recover(&d, &a.signature).map_err(ActionError::BadSignature)?;
    let action_hash = keccak_parts(&[&d, &signer]);
    Ok(VerifiedAction { primary, account, signer, nonce, digest: d, action_hash })
}

/// Fields given in human units in a request without `"wire":true`, and the scale each one uses.
/// PROVISIONAL (fixture typed data); follows `docs/SPEC.md` when it fixes the names.
fn human_scale(g: &Genesis, primary: &str, name: &str, msg: &serde_json::Value) -> Result<Option<u32>, String> {
    let market_dec = |price: bool| -> Result<Option<u32>, String> {
        let mid = msg.get("marketId").and_then(|v| v.as_u64().or_else(|| v.as_str().and_then(|s| s.parse().ok())));
        let mid = mid.ok_or("marketId required to scale a price or size")?;
        let m = g.market(mid as u32).ok_or("unknown marketId")?;
        Ok(Some(if price { m.price_decimals } else { m.size_decimals }))
    };
    Ok(match (primary, name) {
        ("PlaceOrder", "price") | ("PlaceOrder", "triggerPrice") | ("ModifyOrder", "price") | ("ModifyOrder", "triggerPrice") => market_dec(true)?,
        ("PlaceOrder", "size") | ("ModifyOrder", "size") => market_dec(false)?,
        ("Withdraw", "amount") | ("UpdateMargin", "amount") => Some(g.collateral_decimals),
        _ => None,
    })
}

/// Parses an `ActionRequest` (`{type, message, signature, wire?}`) into a signed action.
/// Without `"wire":true`, decimal fields are converted EXACTLY to the scaled integers (never rounded; a value with
/// too many decimals is refused), so the request means precisely what the client signed.
pub fn parse_request(g: &Genesis, req: &serde_json::Value) -> Result<SignedAction, String> {
    let obj = req.as_object().ok_or("request must be an object")?;
    for k in obj.keys() {
        if !matches!(k.as_str(), "type" | "message" | "signature" | "wire") {
            return Err(format!("unknown request member {k:?}"));
        }
    }
    let primary = obj.get("type").and_then(|v| v.as_str()).ok_or("type")?;
    let type_index = g.types.index_of(primary).ok_or_else(|| format!("unknown action type {primary:?}"))?;
    let wire = match obj.get("wire") {
        None => false,
        Some(serde_json::Value::Bool(b)) => *b,
        Some(_) => return Err("wire must be a boolean".into()),
    };
    let msg_in = obj.get("message").ok_or("message")?;
    let msg_obj = msg_in.as_object().ok_or("message must be an object")?;
    let mut msg = serde_json::Map::new();
    for (k, v) in msg_obj {
        let scale = if wire { None } else { human_scale(g, primary, k, msg_in)? };
        let nv = match (scale, v) {
            (Some(dec), serde_json::Value::String(s)) => {
                let neg = s.starts_with('-');
                let body = if neg { &s[1..] } else { s.as_str() };
                let x = decimal_to_scaled(body, dec, u64::MAX as u128).map_err(|e| format!("{k}: {e}"))?;
                serde_json::Value::String(if neg { format!("-{x}") } else { x.to_string() })
            }
            (Some(_), _) => return Err(format!("{k}: human-unit values are decimal strings")),
            (None, v) => v.clone(),
        };
        msg.insert(k.clone(), nv);
    }
    let message = g.types.registry.val_from_json(primary, &serde_json::Value::Object(msg))?;
    let sig_s = obj.get("signature").and_then(|v| v.as_str()).ok_or("signature")?;
    let signature = parse_hex_fixed::<65>(sig_s)?;
    Ok(SignedAction { type_index, message, signature })
}

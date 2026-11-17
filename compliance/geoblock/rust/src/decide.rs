//! The access decision (port of ../src/decide.mjs). It reads the request as JSON so that every malformed
//! shape is judged exactly as the JS reference judges it; `Request` builders for the node are below.
//! Conformance: vectors/decision-vectors.json (tests/vectors.rs).

use crate::addr::normalize_evm_address;
use crate::list::{is_subdivision, DeniedPolicy, Effect, Hosting, List, RESERVED_CODES};
use crate::sanctions::Sanctions;
use serde_json::{json, Value};

pub const ACTION_TYPES: [&str; 8] = [
    "PlaceOrder", "CancelOrder", "CancelAll", "UpdateLeverage", "UpdateMargin", "Withdraw", "ApproveAgent", "RevokeAgent",
];
pub const NO_VALUE_ACTIONS: [&str; 3] = ["CancelOrder", "CancelAll", "RevokeAgent"];
pub const ADDRESS_ROLES: [&str; 5] = ["signer", "account", "recipient", "agent", "depositor"];

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Decision {
    pub allow: bool,
    pub code: &'static str,
    pub http_status: u16,
    pub exit_only: bool,
    pub restricted: Option<bool>,
    pub sanctioned: bool,
    pub policy: Option<&'static str>,
    pub country: Option<String>,
    pub subdivision: Option<String>,
    pub tier: Option<String>,
    pub reasons: Vec<&'static str>,
    pub list_version: Option<String>,
    pub list_sha256: Option<String>,
    pub sanctions_version: Option<String>,
    pub detail: Option<String>,
}

impl Decision {
    pub fn to_json(&self) -> Value {
        json!({"allow": self.allow, "code": self.code, "httpStatus": self.http_status, "exitOnly": self.exit_only,
            "restricted": self.restricted, "sanctioned": self.sanctioned, "policy": self.policy, "country": self.country,
            "subdivision": self.subdivision, "tier": self.tier, "reasons": self.reasons, "listVersion": self.list_version,
            "listSha256": self.list_sha256, "sanctionsVersion": self.sanctions_version, "detail": self.detail})
    }
}

pub struct GeoEval {
    pub denied: bool,
    pub reasons: Vec<&'static str>,
    pub country: Option<String>,
    pub subdivision: Option<String>,
    pub tier: Option<String>,
}

fn ascii_code(v: Option<&Value>) -> Option<String> {
    match v {
        Some(Value::String(s)) if !s.is_empty() && s.bytes().all(|b| (0x21..=0x7e).contains(&b)) => Some(s.to_ascii_uppercase()),
        _ => None,
    }
}

fn is_cc(s: &str) -> bool {
    s.len() == 2 && s.bytes().all(|b| b.is_ascii_uppercase())
}

fn att_valid_residence(att: Option<&Value>) -> Option<&str> {
    let a = att?;
    if a.get("status").and_then(Value::as_str) == Some("valid") {
        a.get("residenceCountry").and_then(Value::as_str)
    } else {
        None
    }
}

fn attested_residence_allowed(list: &List, att: Option<&Value>) -> bool {
    match att_valid_residence(att) {
        Some(c) => list.countries.get(c).map(|e| e.effect == Effect::Allow).unwrap_or(false)
            && !list.restricted_subdivision_countries.contains(c),
        None => false,
    }
}

fn push(v: &mut Vec<&'static str>, r: &'static str) {
    if !v.contains(&r) {
        v.push(r);
    }
}

pub fn evaluate_geo(list: &List, req: &Value) -> GeoEval {
    let live = req.get("mode").and_then(Value::as_str) == Some("live");
    let geo = req.get("geo").filter(|g| g.is_object());
    let mut reasons = vec![];
    let mut country = ascii_code(geo.and_then(|g| g.get("country")));
    let mut subdivision = ascii_code(geo.and_then(|g| g.get("subdivision")));
    let trusted = geo.and_then(|g| g.get("trusted")).and_then(Value::as_bool) == Some(true);
    if live && !trusted {
        push(&mut reasons, "UNTRUSTED_GEO_SOURCE");
        country = None;
        subdivision = None;
    }
    let mut tier = None;
    if country.as_deref() == Some("T1") {
        push(&mut reasons, "ANONYMIZER_TOR");
    }
    match country.as_deref() {
        Some(c) if is_cc(c) && !RESERVED_CODES.contains(&c) => {
            let entry = list.countries.get(c);
            tier = Some(entry.map(|e| e.tier.clone()).unwrap_or_else(|| "unlisted".to_string()));
            if list.country_effect(c) != Effect::Allow {
                push(&mut reasons, "JURISDICTION");
            }
            match subdivision.as_deref() {
                Some(s) => {
                    if !is_subdivision(s) || &s[..2] != c {
                        push(&mut reasons, "GEO_INCONSISTENT");
                    } else if list.subdivisions.contains_key(s) {
                        push(&mut reasons, "JURISDICTION_SUBDIVISION");
                    }
                }
                None => {
                    if list.restricted_subdivision_countries.contains(c) {
                        push(&mut reasons, "SUBDIVISION_UNKNOWN");
                    }
                }
            }
        }
        _ => push(&mut reasons, "UNKNOWN_COUNTRY"),
    }
    let att = req.get("attestation");
    if let Some(rc) = att_valid_residence(att) {
        if list.country_effect(rc) != Effect::Allow {
            push(&mut reasons, "RESIDENCE_DECLARED_RESTRICTED");
        }
    }
    let sig = req.get("signals").filter(|s| s.is_object());
    let deferred = att.and_then(Value::as_str) == Some("deferred");
    for (k, code) in [("tor", "ANONYMIZER_TOR"), ("vpnProxy", "ANONYMIZER_VPN"), ("hosting", "ANONYMIZER_HOSTING")] {
        match sig.and_then(|s| s.get(k)).and_then(Value::as_bool) {
            Some(true) => {
                if k == "hosting"
                    && list.hosting == Hosting::DenyUiAllowApiWithAttestedAccount
                    && req.get("surface").and_then(Value::as_str) == Some("api")
                    && (deferred || attested_residence_allowed(list, att))
                {
                    continue;
                }
                push(&mut reasons, code);
            }
            Some(false) => {}
            None => {
                if live {
                    push(&mut reasons, "SIGNALS_UNAVAILABLE");
                }
            }
        }
    }
    GeoEval { denied: !reasons.is_empty(), reasons, country, subdivision, tier }
}

fn is_exit_action(list: &List, ty: &str, reduce_only: bool) -> bool {
    if list.denied_policy != DeniedPolicy::ExitOnly {
        return false;
    }
    if ty == "PlaceOrder" {
        return reduce_only && list.exit_actions.iter().any(|a| a == "PlaceOrder(reduceOnly=true)");
    }
    list.exit_actions.iter().any(|a| a == ty)
}

/// decide(list, req, sanctions). `req` is the JSON request of ../src/decide.mjs; `sanctions` replaces req.sanctions.
pub fn decide(list: Option<&List>, req: &Value, sanctions: Option<&Sanctions>) -> Decision {
    let mut d = Decision {
        allow: false, code: "MALFORMED", http_status: 400, exit_only: false, restricted: None, sanctioned: false,
        policy: None, country: None, subdivision: None, tier: None, reasons: vec![],
        list_version: list.map(|l| l.version.clone()), list_sha256: list.and_then(|l| l.sha256.clone()),
        sanctions_version: sanctions.map(|s| s.version.clone()), detail: None,
    };
    let refuse = |mut d: Decision, code: &'static str, http: u16, detail: &str| {
        d.allow = false;
        d.code = code;
        d.http_status = http;
        d.detail = Some(detail.to_string());
        d
    };
    let mode = req.get("mode").and_then(Value::as_str);
    if mode != Some("live") && mode != Some("dev") {
        return refuse(d, "MALFORMED", 400, "mode");
    }
    let live = mode == Some("live");
    let kind = req.get("kind").and_then(Value::as_str).unwrap_or("");
    if !["action", "read", "deposit"].contains(&kind) {
        return refuse(d, "MALFORMED", 400, "kind");
    }
    let surface = req.get("surface").and_then(Value::as_str);
    if surface != Some("api") && surface != Some("ui") {
        return refuse(d, "MALFORMED", 400, "surface");
    }
    let mut ty = "";
    let mut reduce_only = false;
    if kind == "action" {
        let a = req.get("action").filter(|a| a.is_object());
        let t = a.and_then(|a| a.get("type")).and_then(Value::as_str);
        match t.and_then(|t| ACTION_TYPES.iter().find(|x| **x == t)) {
            Some(t) => ty = t,
            None => return refuse(d, "MALFORMED", 400, "action.type"),
        }
        if ty == "PlaceOrder" {
            match a.and_then(|a| a.get("reduceOnly")).and_then(Value::as_bool) {
                Some(b) => reduce_only = b,
                None => return refuse(d, "MALFORMED", 400, "action.reduceOnly"),
            }
        }
    }
    let addrs = match req.get("addresses") {
        None | Some(Value::Null) => vec![],
        Some(Value::Array(a)) => a.clone(),
        Some(_) => return refuse(d, "MALFORMED", 400, "addresses"),
    };
    let mut normalized: Vec<(String, String)> = vec![];
    for a in &addrs {
        let role = a.get("role").and_then(Value::as_str);
        let Some(role) = role.filter(|r| ADDRESS_ROLES.contains(r)) else {
            return refuse(d, "MALFORMED", 400, "address.role");
        };
        let Some(n) = a.get("address").and_then(Value::as_str).and_then(normalize_evm_address) else {
            return refuse(d, "MALFORMED", 400, &format!("address.{role}"));
        };
        normalized.push((role.to_string(), n));
    }
    if (kind == "action" || kind == "deposit") && normalized.is_empty() {
        return refuse(d, "MALFORMED", 400, "no address to screen");
    }
    let now = req.get("nowMs").and_then(Value::as_f64);
    let usable = sanctions.is_some_and(|s| !live || now.is_some_and(|n| s.fresh(n)));
    if let Some(s) = sanctions {
        let hits: Vec<&str> = normalized.iter().filter(|(_, a)| s.has(a)).map(|(r, _)| r.as_str()).collect();
        if !hits.is_empty() {
            let detail = hits.join(",");
            let mut d = refuse(d, "SANCTIONED", 451, &detail);
            d.sanctioned = true;
            d.reasons = vec!["SANCTIONED"];
            return d;
        }
    }
    let ok = |mut d: Decision, code: &'static str| {
        d.allow = true;
        d.code = code;
        d.http_status = 200;
        d
    };
    if kind == "read" {
        return ok(d, "OK");
    }
    if !usable && live && (kind == "deposit" || !NO_VALUE_ACTIONS.contains(&ty)) {
        let mut d = refuse(d, "SANCTIONS_UNAVAILABLE", 503, if sanctions.is_none() { "no list" } else { "stale" });
        d.reasons = vec!["SANCTIONS_UNAVAILABLE"];
        return d;
    }
    if kind == "deposit" {
        return ok(d, "OK");
    }
    let Some(list) = list else {
        if NO_VALUE_ACTIONS.contains(&ty) {
            let mut d = ok(d, "OK_LIST_UNAVAILABLE");
            d.reasons = vec!["LIST_UNAVAILABLE"];
            return d;
        }
        let mut d = refuse(d, "LIST_UNAVAILABLE", 503, "");
        d.detail = None;
        d.reasons = vec!["LIST_UNAVAILABLE"];
        return d;
    };
    let g = evaluate_geo(list, req);
    d.country = g.country;
    d.subdivision = g.subdivision;
    d.tier = g.tier;
    d.restricted = Some(g.denied);
    d.reasons = g.reasons.clone();
    if g.denied {
        d.policy = Some(list.denied_policy.as_str());
        if is_exit_action(list, ty, reduce_only) {
            let mut d = ok(d, "OK_EXIT_ONLY");
            d.exit_only = true;
            return d;
        }
        let first = g.reasons[0];
        return refuse(d, "GEO_RESTRICTED", 451, first);
    }
    d.policy = Some("open");
    let require_att = req.get("requireAttestation").and_then(Value::as_bool).unwrap_or(live);
    let exit_like = if ty == "PlaceOrder" { reduce_only } else { ["CancelOrder", "CancelAll", "Withdraw", "RevokeAgent"].contains(&ty) };
    let att = req.get("attestation");
    let deferred = att.and_then(Value::as_str) == Some("deferred");
    let valid = att.and_then(|a| a.get("status")).and_then(Value::as_str) == Some("valid");
    if require_att && !exit_like && !deferred && !valid {
        let st = att.and_then(|a| a.get("status")).and_then(Value::as_str).unwrap_or("missing").to_string();
        let mut d = refuse(d, "ATTESTATION_REQUIRED", 403, &st);
        d.reasons = vec!["ATTESTATION_REQUIRED"];
        return d;
    }
    ok(d, "OK")
}

/// GET /v1/geo body (docs/API.md section 6 shape).
pub fn geo_status(list: Option<&List>, req: &Value) -> Value {
    let Some(list) = list else {
        return json!({"country": null, "restricted": true, "policy": "actions_refused", "listVersion": null,
            "reasonCode": "LIST_UNAVAILABLE", "message": "Access rules are unavailable; only cancellations are accepted.", "sanctioned": false});
    };
    let g = evaluate_geo(list, req);
    let policy = if g.denied { list.denied_policy.as_str() } else { "open" };
    let message = if !g.denied {
        Value::Null
    } else if policy == "exit_only" {
        json!("PosternDEX Perpetuals is not available in your jurisdiction. You can still cancel orders, reduce positions and withdraw.")
    } else {
        json!("PosternDEX Perpetuals is not available in your jurisdiction.")
    };
    json!({"country": g.country, "subdivision": g.subdivision, "restricted": g.denied, "policy": policy,
        "listVersion": list.version, "reasonCode": g.reasons.first(), "reasons": g.reasons, "message": message, "sanctioned": false})
}

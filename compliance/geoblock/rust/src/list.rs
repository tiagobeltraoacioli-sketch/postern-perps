//! Strict loader of compliance/geoblock.json. Port of ../src/list.mjs: same refusals, same error substrings
//! (vectors/list-vectors.json). A list this code does not fully understand is refused.

use serde_json::{Map, Value};
use std::collections::{BTreeMap, BTreeSet};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ListError(pub String);
impl std::fmt::Display for ListError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "geoblock list refused: {}", self.0)
    }
}
impl std::error::Error for ListError {}

fn err<T>(m: impl Into<String>) -> Result<T, ListError> {
    Err(ListError(m.into()))
}

pub const SCHEMA: &str = "postern-perps/geoblock";
pub const SUPPORTED_MAJOR: u64 = 0;
pub const TIERS: [(&str, Effect); 3] = [
    ("allowed", Effect::Allow),
    ("blocked", Effect::Deny),
    ("restricted-review", Effect::Deny),
];
pub const RESERVED_CODES: [&str; 8] = ["XX", "T1", "ZZ", "A1", "A2", "O1", "EU", "AP"];
pub const EXIT_ACTIONS_KNOWN: [&str; 5] = [
    "CancelOrder",
    "CancelAll",
    "PlaceOrder(reduceOnly=true)",
    "Withdraw",
    "RevokeAgent",
];
const DECISION_TOP_LEVEL: [&str; 10] = [
    "schema", "version", "rule", "tiers", "default_tier", "unknown_country_effect", "countries", "subdivisions",
    "anonymizers", "denied_policy",
];
pub const INFORMATIONAL_TOP_LEVEL: [&str; 8] = [
    "generated_utc", "status", "reason_categories", "sources", "notes", "$comment", "research", "changelog",
];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Effect {
    Allow,
    Deny,
}

pub fn tier_effect(t: &str) -> Option<Effect> {
    TIERS.iter().find(|(n, _)| *n == t).map(|(_, e)| *e)
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Entry {
    pub tier: String,
    pub effect: Effect,
    pub reasons: Vec<String>,
    pub name: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Hosting {
    Deny,
    DenyUiAllowApiWithAttestedAccount,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DeniedPolicy {
    ExitOnly,
    ActionsRefused,
}
impl DeniedPolicy {
    pub fn as_str(&self) -> &'static str {
        match self {
            DeniedPolicy::ExitOnly => "exit_only",
            DeniedPolicy::ActionsRefused => "actions_refused",
        }
    }
}

#[derive(Debug, Clone)]
pub struct List {
    pub version: String,
    pub sha256: Option<String>,
    pub default_tier: String,
    pub countries: BTreeMap<String, Entry>,
    pub subdivisions: BTreeMap<String, Entry>,
    pub restricted_subdivision_countries: BTreeSet<String>,
    pub hosting: Hosting,
    pub denied_policy: DeniedPolicy,
    pub exit_actions: Vec<String>,
}

fn is_cc(k: &str) -> bool {
    k.len() == 2 && k.bytes().all(|b| b.is_ascii_uppercase())
}
pub(crate) fn is_subdivision(k: &str) -> bool {
    let b = k.as_bytes();
    b.len() >= 4
        && b.len() <= 6
        && b[0].is_ascii_uppercase()
        && b[1].is_ascii_uppercase()
        && b[2] == b'-'
        && b[3..].iter().all(|c| c.is_ascii_uppercase() || c.is_ascii_digit())
}

fn check_entries(
    obj: Option<&Value>,
    what: &str,
    is_key: fn(&str) -> bool,
    allow_allowing: &dyn Fn(&str) -> bool,
) -> Result<BTreeMap<String, Entry>, ListError> {
    let Some(Value::Object(map)) = obj else { return err(format!("{what} must be an object")) };
    let mut out = BTreeMap::new();
    for (k, e) in map {
        if !is_key(k) {
            return err(format!("{what} key {} has the wrong shape", Value::String(k.clone())));
        }
        let Value::Object(e) = e else { return err(format!("{what}.{k} must be an object")) };
        let tier = e.get("tier").and_then(Value::as_str);
        let Some(effect) = tier.and_then(tier_effect) else {
            return err(format!("{what}.{k}.tier {} unknown", e.get("tier").cloned().unwrap_or(Value::Null)));
        };
        if effect == Effect::Allow && !allow_allowing(k) {
            return err(format!("{what}.{k} may not be allowing"));
        }
        let reasons = match e.get("reasons") {
            None => vec![],
            Some(Value::Array(a)) if a.iter().all(Value::is_string) => {
                a.iter().map(|x| x.as_str().unwrap().to_string()).collect()
            }
            Some(_) => return err(format!("{what}.{k}.reasons must be an array of strings")),
        };
        let name = match e.get("name") {
            Some(Value::String(s)) => s.clone(),
            _ => String::new(),
        };
        out.insert(k.clone(), Entry { tier: tier.unwrap().to_string(), effect, reasons, name });
    }
    Ok(out)
}

fn parse_version(v: &str) -> Option<u64> {
    let parts: Vec<&str> = v.split('.').collect();
    if parts.len() < 2 || parts.len() > 3 || parts.iter().any(|p| p.is_empty() || !p.bytes().all(|b| b.is_ascii_digit())) {
        return None;
    }
    parts[0].parse().ok()
}

/// load_list(json, sha256) -> List, or the refusal.
pub fn load_list(json: &Value, sha256: Option<String>) -> Result<List, ListError> {
    let Value::Object(j) = json else { return err("not an object") };
    for k in j.keys() {
        if !DECISION_TOP_LEVEL.contains(&k.as_str()) && !INFORMATIONAL_TOP_LEVEL.contains(&k.as_str()) {
            return err(format!(
                "unknown top-level key {} (a decision field this code does not read)",
                Value::String(k.clone())
            ));
        }
    }
    if j.get("schema").and_then(Value::as_str) != Some(SCHEMA) {
        return err(format!("schema must be {SCHEMA}"));
    }
    let version = j.get("version").and_then(Value::as_str).unwrap_or("");
    let Some(major) = parse_version(version) else { return err("version must be MAJOR.MINOR[.PATCH]") };
    if major != SUPPORTED_MAJOR {
        return err(format!("major version {major} not supported (expects {SUPPORTED_MAJOR})"));
    }
    match j.get("rule").and_then(Value::as_str) {
        Some(r) if r.to_ascii_lowercase().contains("deny by default") => {}
        _ => return err("rule text must state \"Deny by default\""),
    }
    let Some(Value::Object(tiers)) = j.get("tiers") else { return err("tiers missing") };
    let mut names: Vec<&String> = tiers.keys().collect();
    names.sort();
    let want: Vec<&str> = TIERS.iter().map(|(n, _)| *n).collect();
    if names.iter().map(|s| s.as_str()).collect::<Vec<_>>() != want {
        return err("tiers must be exactly allowed, blocked, restricted-review");
    }
    for (t, eff) in TIERS {
        let want = if eff == Effect::Allow { "allow" } else { "deny" };
        match tiers.get(t) {
            Some(Value::Object(o)) if o.get("effect").and_then(Value::as_str) == Some(want) => {}
            _ => return err(format!("tier {t} must have effect {want}")),
        }
    }
    let default_tier = j.get("default_tier").and_then(Value::as_str).unwrap_or("");
    if tier_effect(default_tier) != Some(Effect::Deny) {
        return err("default_tier must be a denying tier");
    }
    if j.get("unknown_country_effect").and_then(Value::as_str) != Some("deny") {
        return err("unknown_country_effect must be deny");
    }
    let countries = check_entries(j.get("countries"), "countries", is_cc, &|k| !RESERVED_CODES.contains(&k))?;
    let empty = Value::Object(Map::new());
    let subdivisions = check_entries(Some(j.get("subdivisions").unwrap_or(&empty)), "subdivisions", is_subdivision, &|_| false)?;
    let Some(Value::Object(anon)) = j.get("anonymizers") else { return err("anonymizers missing") };
    for k in anon.keys() {
        if !["tor", "vpn_proxy", "hosting"].contains(&k.as_str()) {
            return err(format!("anonymizer {k} unknown"));
        }
    }
    if anon.get("tor").and_then(Value::as_str) != Some("deny") {
        return err("anonymizers.tor must be one of deny");
    }
    if anon.get("vpn_proxy").and_then(Value::as_str) != Some("deny") {
        return err("anonymizers.vpn_proxy must be one of deny");
    }
    let hosting = match anon.get("hosting").and_then(Value::as_str) {
        Some("deny") => Hosting::Deny,
        Some("deny-ui-allow-api-with-attested-account") => Hosting::DenyUiAllowApiWithAttestedAccount,
        _ => return err("anonymizers.hosting must be one of deny | deny-ui-allow-api-with-attested-account"),
    };
    let Some(Value::Object(dp)) = j.get("denied_policy") else {
        return err("denied_policy.policy must be exit_only or actions_refused");
    };
    let denied_policy = match dp.get("policy").and_then(Value::as_str) {
        Some("exit_only") => DeniedPolicy::ExitOnly,
        Some("actions_refused") => DeniedPolicy::ActionsRefused,
        _ => return err("denied_policy.policy must be exit_only or actions_refused"),
    };
    let mut exit_actions = vec![];
    if denied_policy == DeniedPolicy::ExitOnly {
        let Some(Value::Array(a)) = dp.get("exit_only_actions") else {
            return err("denied_policy.exit_only_actions must be an array");
        };
        for x in a {
            match x.as_str() {
                Some(s) if EXIT_ACTIONS_KNOWN.contains(&s) => exit_actions.push(s.to_string()),
                _ => return err(format!("exit-only action {x} is not an exit action")),
            }
        }
    }
    let restricted_subdivision_countries = subdivisions.keys().map(|s| s[..2].to_string()).collect();
    Ok(List {
        version: version.to_string(),
        sha256,
        default_tier: default_tier.to_string(),
        countries,
        subdivisions,
        restricted_subdivision_countries,
        hosting,
        denied_policy,
        exit_actions,
    })
}

impl List {
    /// Effect of a country code under this list (listed entry, else the default tier).
    pub fn country_effect(&self, cc: &str) -> Effect {
        match self.countries.get(cc) {
            Some(e) => e.effect,
            None => tier_effect(&self.default_tier).unwrap_or(Effect::Deny),
        }
    }
}

//! Geoblocking at the node's API (an operator access-point control; the L2 forced-action path is not, and cannot
//! be, geoblocked). The list is agent 392/354's `compliance/geoblock.json`; the shared decision library for edge,
//! node and frontend is agent 397's `compliance/geoblock/`. This module applies the same rule to the node:
//! deny by default; only a country listed with tier `allowed` (and no blocked subdivision) is open; a denied
//! request may still use the exit-only actions; a sanctioned address is refused for everything.
//! Everything legal about the list is for counsel.
//!
//! Where the country comes from: in `live` mode ONLY the configured header set by a trusted reverse proxy, and only
//! when the TCP peer is one of the configured proxy addresses; otherwise the country is unknown and the request is
//! denied (fail closed). In `dev`/`mock` mode the `X-Mock-Country` header or `?country=` may set it.
//!
//! UNAUDITED PROTOTYPE.

use crate::util::{parse_address, Address};
use std::collections::{BTreeMap, BTreeSet};
use std::net::IpAddr;
use std::sync::Mutex;

pub const ASSERTION_HEADER: &str = "x-postern-geo";
pub const ASSERTION_MAC_HEADER: &str = "x-postern-geo-mac";
/// WebSocket `op: action` frames carry their own per-action assertion here (397, R395-36).
pub const EDGE_GEO_FIELD: &str = "edgeGeo";

/// The edge's HMAC assertion (agent 397's `compliance/geoblock`, verified with its Rust port): one shared secret,
/// one replay cache for REST and WebSocket together, so an assertion admits at most one action anywhere.
pub struct EdgeVerifier {
    secret: Vec<u8>,
    replay: Mutex<postern_geoblock::assertion::ReplayCache>,
}

impl EdgeVerifier {
    pub fn new(secret: Vec<u8>) -> Result<EdgeVerifier, String> {
        if secret.len() < postern_geoblock::assertion::MIN_SECRET_BYTES {
            return Err("edge secret shorter than 32 bytes".into());
        }
        Ok(EdgeVerifier { secret, replay: Mutex::new(postern_geoblock::assertion::ReplayCache::new(200_000)) })
    }

    /// Did this request come through the edge? True for a valid, fresh, never-seen assertion (which it spends),
    /// whatever it says about the country: used to decide whether the edge's client-address header is believed.
    pub fn authenticate(&self, header: Option<&str>, mac: Option<&str>, now_ms: u64) -> bool {
        let mut r = self.replay.lock().unwrap();
        postern_geoblock::assertion::verify(header, mac, &self.secret, now_ms as i64, Some(&mut r)).is_ok()
    }

    /// (country, subdivision) of a valid, fresh, never-seen assertion whose anonymizer signals do not deny.
    pub fn verify(&self, header: Option<&str>, mac: Option<&str>, now_ms: u64) -> Result<(Option<String>, Option<String>), String> {
        let mut r = self.replay.lock().unwrap();
        let v = postern_geoblock::assertion::verify(header, mac, &self.secret, now_ms as i64, Some(&mut r)).map_err(|e| e.to_string())?;
        // geoblock.json anonymizers: tor and vpn_proxy deny; hosting is allowed on the API only with an attested
        // account, which this node does not check yet: deny (fail closed, stated)
        if v.tor == Some(true) || v.vpn_proxy == Some(true) || v.hosting == Some(true) {
            return Ok((None, None));
        }
        Ok((v.country, v.subdivision))
    }
}

/// Everything a list's `denied_policy.exit_only_actions` may name: actions that reduce exposure, take the user's
/// own funds out, or REMOVE authority. Nothing that opens exposure or grants authority can be listed.
pub const EXIT_ACTIONS_BOUND: [&str; 4] = ["CancelOrder", "CancelAll", "Withdraw", "RevokeAgent"];
pub const EXIT_REDUCE_ONLY_ORDER: &str = "PlaceOrder(reduceOnly=true)";

#[derive(Clone)]
pub struct GeoPolicy {
    pub version: String,
    pub allowed: BTreeSet<String>,
    pub blocked_subdivisions: BTreeSet<String>,
    pub exit_only: bool,
    /// Action types a denied user may still send (`denied_policy.exit_only_actions` of the list; R436-7), always
    /// inside `EXIT_ACTIONS_BOUND`.
    pub exit_actions: BTreeSet<String>,
    /// `PlaceOrder(reduceOnly=true)` is among the list's exit actions.
    pub exit_reduce_only_orders: bool,
    pub sanctioned: BTreeSet<Address>,
    pub trusted_proxies: BTreeSet<IpAddr>,
    pub country_header: String,
    pub subdivision_header: Option<String>,
    pub dev: bool,
    /// dev only, explicit flag: the country of a request that names none (conformance runs); never in live mode
    pub dev_default_country: Option<String>,
    /// when set, the country of an action comes ONLY from a valid edge assertion (REST header or WebSocket
    /// per-action `edgeGeo`); nothing else is trusted
    pub edge: Option<std::sync::Arc<EdgeVerifier>>,
    pub tiers: BTreeMap<String, String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum GeoDecision {
    Allow,
    ExitOnly { country: Option<String> },
    Sanctioned,
}

impl GeoPolicy {
    pub fn from_json(j: &serde_json::Value, dev: bool) -> Result<GeoPolicy, String> {
        let version = j.get("version").and_then(|v| v.as_str()).ok_or("geoblock.version")?.to_string();
        if j.get("unknown_country_effect").and_then(|v| v.as_str()) != Some("deny") {
            return Err("geoblock.json must deny unknown countries".into());
        }
        let tiers_def = j.get("tiers").and_then(|v| v.as_object()).ok_or("geoblock.tiers")?;
        let effect = |t: &str| tiers_def.get(t).and_then(|v| v.get("effect")).and_then(|v| v.as_str()).unwrap_or("deny").to_string();
        let mut allowed = BTreeSet::new();
        let mut tiers = BTreeMap::new();
        for (cc, e) in j.get("countries").and_then(|v| v.as_object()).ok_or("geoblock.countries")? {
            if cc.len() != 2 || !cc.bytes().all(|c| c.is_ascii_uppercase()) {
                return Err(format!("bad country code {cc:?}"));
            }
            let tier = e.get("tier").and_then(|v| v.as_str()).ok_or("country tier")?;
            if !tiers_def.contains_key(tier) {
                return Err(format!("{cc}: unknown tier {tier}"));
            }
            if effect(tier) == "allow" {
                allowed.insert(cc.clone());
            }
            tiers.insert(cc.clone(), tier.to_string());
        }
        let mut blocked_subdivisions = BTreeSet::new();
        if let Some(s) = j.get("subdivisions").and_then(|v| v.as_object()) {
            for (code, e) in s {
                let tier = e.get("tier").and_then(|v| v.as_str()).unwrap_or("blocked");
                if effect(tier) != "allow" {
                    blocked_subdivisions.insert(code.clone());
                }
            }
        }
        let exit_only = j.pointer("/denied_policy/policy").and_then(|v| v.as_str()) == Some("exit_only");
        // R436-7: the exit actions are the list's. A list may CLOSE more than v0.1 did; it can never open an
        // action outside the fixed bound (an action that adds exposure or grants authority): such a file is
        // refused, so an edit of the data cannot widen what a denied user may do.
        let (mut exit_actions, mut exit_reduce_only_orders) = (BTreeSet::new(), false);
        match j.pointer("/denied_policy/exit_only_actions") {
            None => {
                // a list without the field: the v0.1 rule
                exit_actions = ["CancelOrder", "CancelAll", "Withdraw"].iter().map(|s| s.to_string()).collect();
                exit_reduce_only_orders = true;
            }
            Some(v) => {
                for e in v.as_array().ok_or("denied_policy.exit_only_actions must be a list")? {
                    let name = e.as_str().ok_or("denied_policy.exit_only_actions: entries are strings")?;
                    if name == EXIT_REDUCE_ONLY_ORDER {
                        exit_reduce_only_orders = true;
                    } else if EXIT_ACTIONS_BOUND.contains(&name) {
                        exit_actions.insert(name.to_string());
                    } else {
                        return Err(format!("denied_policy.exit_only_actions: {name:?} can never be an exit action (allowed: {EXIT_ACTIONS_BOUND:?} and {EXIT_REDUCE_ONLY_ORDER:?})"));
                    }
                }
            }
        }
        Ok(GeoPolicy {
            version,
            allowed,
            blocked_subdivisions,
            exit_only,
            exit_actions,
            exit_reduce_only_orders,
            sanctioned: BTreeSet::new(),
            trusted_proxies: BTreeSet::new(),
            country_header: "x-country".into(),
            subdivision_header: None,
            dev,
            dev_default_country: None,
            edge: None,
            tiers,
        })
    }

    pub fn add_sanctioned(&mut self, list: &[String]) -> Result<(), String> {
        for a in list {
            self.sanctioned.insert(parse_address(a)?);
        }
        Ok(())
    }

    /// Determines the request's country (None = unknown) from the trusted header or, in dev, the mock header.
    pub fn country(&self, peer: Option<IpAddr>, headers: &axum::http::HeaderMap, query_country: Option<&str>) -> (Option<String>, Option<String>) {
        let norm = |s: &str| -> Option<String> {
            let s = s.trim().to_ascii_uppercase();
            if s.len() == 2 && s.bytes().all(|c| c.is_ascii_uppercase()) && s != "XX" && s != "T1" {
                Some(s)
            } else {
                None
            }
        };
        if self.dev {
            if let Some(c) = headers.get("x-mock-country").and_then(|v| v.to_str().ok()).or(query_country) {
                return (norm(c), None);
            }
            if let Some(c) = &self.dev_default_country {
                return (norm(c), None);
            }
        }
        let trusted = peer.is_some_and(|p| self.trusted_proxies.contains(&p));
        if !trusted {
            return (None, None);
        }
        let c = headers.get(self.country_header.as_str()).and_then(|v| v.to_str().ok()).and_then(norm);
        let sd = self
            .subdivision_header
            .as_ref()
            .and_then(|h| headers.get(h.as_str()))
            .and_then(|v| v.to_str().ok())
            .map(|s| s.trim().to_ascii_uppercase());
        (c, sd)
    }

    pub fn decide(&self, country: Option<&str>, subdivision: Option<&str>, account: &Address, signer: &Address) -> GeoDecision {
        if self.sanctioned.contains(account) || self.sanctioned.contains(signer) {
            return GeoDecision::Sanctioned;
        }
        let open = match country {
            Some(c) => self.allowed.contains(c) && !subdivision.is_some_and(|s| self.blocked_subdivisions.contains(s)),
            None => false,
        };
        if open {
            GeoDecision::Allow
        } else {
            GeoDecision::ExitOnly { country: country.map(|s| s.to_string()) }
        }
    }

    /// The v0.1 exit-only set (CancelOrder, CancelAll, reduce-only PlaceOrder, Withdraw): what a list without
    /// `exit_only_actions` means. The policy's own set is `is_exit`.
    pub fn is_exit_action(primary: &str, reduce_only: Option<bool>) -> bool {
        match primary {
            "CancelOrder" | "CancelAll" | "Withdraw" => true,
            "PlaceOrder" => reduce_only == Some(true),
            _ => false,
        }
    }

    /// Is this action an exit action of THIS list (R436-7)?
    pub fn is_exit(&self, primary: &str, reduce_only: Option<bool>) -> bool {
        if primary == "PlaceOrder" {
            return self.exit_reduce_only_orders && reduce_only == Some(true);
        }
        self.exit_actions.contains(primary)
    }

    /// Whether an action may be admitted under this decision.
    pub fn admits(&self, d: &GeoDecision, primary: &str, reduce_only: Option<bool>) -> bool {
        match d {
            GeoDecision::Allow => true,
            GeoDecision::Sanctioned => false,
            GeoDecision::ExitOnly { .. } => self.exit_only && self.is_exit(primary, reduce_only),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn policy() -> GeoPolicy {
        let j = serde_json::json!({
            "version": "t", "unknown_country_effect": "deny",
            "tiers": {"blocked": {"effect": "deny"}, "restricted-review": {"effect": "deny"}, "allowed": {"effect": "allow"}},
            "countries": {"PA": {"tier": "allowed"}, "BR": {"tier": "blocked"}, "UA": {"tier": "allowed"}, "DE": {"tier": "restricted-review"}},
            "subdivisions": {"UA-43": {"tier": "blocked"}},
            "denied_policy": {"policy": "exit_only"}
        });
        GeoPolicy::from_json(&j, false).unwrap()
    }

    #[test]
    fn deny_by_default_exit_only_and_sanctions() {
        let mut p = policy();
        let a = [1u8; 20];
        assert_eq!(p.decide(Some("PA"), None, &a, &a), GeoDecision::Allow);
        for c in [Some("BR"), Some("DE"), Some("FR"), None] {
            let d = p.decide(c, None, &a, &a);
            assert!(matches!(d, GeoDecision::ExitOnly { .. }), "{c:?}");
            assert!(!p.admits(&d, "PlaceOrder", Some(false)));
            assert!(!p.admits(&d, "ApproveAgent", None));
            assert!(!p.admits(&d, "UpdateLeverage", None));
            assert!(p.admits(&d, "PlaceOrder", Some(true)));
            assert!(p.admits(&d, "CancelAll", None));
            assert!(p.admits(&d, "Withdraw", None));
        }
        assert!(matches!(p.decide(Some("UA"), Some("UA-43"), &a, &a), GeoDecision::ExitOnly { .. }));
        assert_eq!(p.decide(Some("UA"), Some("UA-30"), &a, &a), GeoDecision::Allow);
        p.add_sanctioned(&["0x0101010101010101010101010101010101010101".into()]).unwrap();
        let d = p.decide(Some("PA"), None, &a, &[2u8; 20]);
        assert_eq!(d, GeoDecision::Sanctioned);
        assert!(!p.admits(&d, "Withdraw", None));
        // the signer (an agent) being sanctioned is enough
        assert_eq!(p.decide(Some("PA"), None, &[2u8; 20], &a), GeoDecision::Sanctioned);
    }

    /// R436-7 (Low): the exit actions are the LIST's (`denied_policy.exit_only_actions`), so a denied user can
    /// revoke a leaked agent key once the list says so (geoblock.json 0.2.2); the node bounds what a list may open.
    #[test]
    fn r436_7_exit_actions_come_from_the_list_inside_a_fixed_bound() {
        let base = |actions: serde_json::Value| {
            let mut j = serde_json::json!({
                "version": "t", "unknown_country_effect": "deny",
                "tiers": {"blocked": {"effect": "deny"}, "allowed": {"effect": "allow"}},
                "countries": {"PA": {"tier": "allowed"}, "BR": {"tier": "blocked"}},
                "denied_policy": {"policy": "exit_only"}
            });
            if !actions.is_null() {
                j["denied_policy"]["exit_only_actions"] = actions;
            }
            GeoPolicy::from_json(&j, false)
        };
        let a = [1u8; 20];
        // the 0.2.2 list: RevokeAgent leaves authority and moves no value: admitted for a denied country
        let p = base(serde_json::json!(["CancelOrder", "CancelAll", "PlaceOrder(reduceOnly=true)", "Withdraw", "RevokeAgent"])).unwrap();
        let d = p.decide(Some("BR"), None, &a, &a);
        assert!(p.admits(&d, "RevokeAgent", None));
        assert!(!p.admits(&d, "ApproveAgent", None), "granting authority stays refused");
        assert!(p.admits(&d, "PlaceOrder", Some(true)) && !p.admits(&d, "PlaceOrder", Some(false)) && !p.admits(&d, "PlaceOrder", None));
        assert!(p.admits(&d, "Withdraw", None) && p.admits(&d, "CancelOrder", None) && p.admits(&d, "CancelAll", None));
        // a list without it (0.2.1) and a file without the field (the v0.1 rule) refuse it
        let p = base(serde_json::json!(["CancelOrder", "CancelAll", "PlaceOrder(reduceOnly=true)", "Withdraw"])).unwrap();
        assert!(!p.admits(&p.decide(Some("BR"), None, &a, &a), "RevokeAgent", None));
        let p = base(serde_json::Value::Null).unwrap();
        let d = p.decide(Some("BR"), None, &a, &a);
        assert!(!p.admits(&d, "RevokeAgent", None) && p.admits(&d, "Withdraw", None) && p.admits(&d, "PlaceOrder", Some(true)));
        // a shorter list is honoured (a list may close more)
        let p = base(serde_json::json!(["Withdraw"])).unwrap();
        let d = p.decide(Some("BR"), None, &a, &a);
        assert!(p.admits(&d, "Withdraw", None) && !p.admits(&d, "CancelAll", None) && !p.admits(&d, "PlaceOrder", Some(true)));
        // a list can never OPEN an action that adds exposure or authority: the file is refused (fail closed)
        for bad in ["ApproveAgent", "PlaceOrder", "UpdateLeverage", "UpdateMargin", "PlaceOrder(reduceOnly=false)", "Deposit", ""] {
            assert!(base(serde_json::json!(["Withdraw", bad])).is_err(), "{bad:?} must not be accepted as an exit action");
        }
        assert!(base(serde_json::json!("Withdraw")).is_err(), "not a list");
        assert!(base(serde_json::json!([1])).is_err());
    }

    #[test]
    fn country_only_from_trusted_proxy_in_live_mode() {
        let mut p = policy();
        let mut h = axum::http::HeaderMap::new();
        h.insert("x-country", "PA".parse().unwrap());
        h.insert("x-mock-country", "PA".parse().unwrap());
        let peer: IpAddr = "127.0.0.1".parse().unwrap();
        assert_eq!(p.country(Some(peer), &h, Some("PA")).0, None, "untrusted peer: header ignored, mock ignored in live");
        p.trusted_proxies.insert(peer);
        assert_eq!(p.country(Some(peer), &h, None).0.as_deref(), Some("PA"));
        h.insert("x-country", "XX".parse().unwrap());
        assert_eq!(p.country(Some(peer), &h, None).0, None, "XX is unknown");
    }

    #[test]
    fn real_geoblock_file_loads_and_has_no_allowed_entry_yet() {
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../compliance/geoblock.json");
        if let Ok(b) = std::fs::read(&path) {
            let j: serde_json::Value = serde_json::from_slice(&b).unwrap();
            let p = GeoPolicy::from_json(&j, false).unwrap();
            assert!(p.exit_only);
            // R436-7: whatever version of the list is checked out, its exit actions keep the four of v0.1
            let a = [1u8; 20];
            let d = p.decide(Some("BR"), None, &a, &a);
            assert!(p.admits(&d, "CancelOrder", None) && p.admits(&d, "CancelAll", None) && p.admits(&d, "Withdraw", None) && p.admits(&d, "PlaceOrder", Some(true)));
            assert!(!p.admits(&d, "ApproveAgent", None) && !p.admits(&d, "PlaceOrder", Some(false)));
            for c in ["US", "BR", "GB", "CA", "KP", "IR"] {
                assert!(!p.allowed.contains(c), "{c} must not be open");
            }
        }
    }
}

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

#[derive(Clone, Debug)]
pub struct GeoPolicy {
    pub version: String,
    pub allowed: BTreeSet<String>,
    pub blocked_subdivisions: BTreeSet<String>,
    pub exit_only: bool,
    pub sanctioned: BTreeSet<Address>,
    pub trusted_proxies: BTreeSet<IpAddr>,
    pub country_header: String,
    pub subdivision_header: Option<String>,
    pub dev: bool,
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
        Ok(GeoPolicy {
            version,
            allowed,
            blocked_subdivisions,
            exit_only,
            sanctioned: BTreeSet::new(),
            trusted_proxies: BTreeSet::new(),
            country_header: "x-country".into(),
            subdivision_header: None,
            dev,
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

    /// Exit-only actions of `denied_policy` (CancelOrder, CancelAll, reduce-only PlaceOrder, Withdraw).
    pub fn is_exit_action(primary: &str, reduce_only: Option<bool>) -> bool {
        match primary {
            "CancelOrder" | "CancelAll" | "Withdraw" => true,
            "PlaceOrder" => reduce_only == Some(true),
            _ => false,
        }
    }

    /// Whether an action may be admitted under this decision.
    pub fn admits(&self, d: &GeoDecision, primary: &str, reduce_only: Option<bool>) -> bool {
        match d {
            GeoDecision::Allow => true,
            GeoDecision::Sanctioned => false,
            GeoDecision::ExitOnly { .. } => self.exit_only && Self::is_exit_action(primary, reduce_only),
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
            for c in ["US", "BR", "GB", "CA", "KP", "IR"] {
                assert!(!p.allowed.contains(c), "{c} must not be open");
            }
        }
    }
}

//! The checkpoint relayer: submits final checkpoints to the PerpsVault on the L2 and tracks what the vault holds.
//!
//! UNAUDITED PROTOTYPE.

use std::collections::BTreeMap;

/// What the relayer last learned from the vault, served by `/v1/status` (`vaultMode`) and `/v1/node`.
#[derive(Clone, Debug)]
pub struct RelayState {
    /// `normal`, `paused`, `escape`, or `unknown` (no relayer, or the vault was never read)
    pub vault_mode: &'static str,
    /// withdrawal id -> L2 transaction hash that paid it (not tracked yet: empty)
    pub paid: BTreeMap<u64, String>,
    pub enabled: bool,
    pub last_error: Option<String>,
}

impl Default for RelayState {
    fn default() -> Self {
        RelayState { vault_mode: "unknown", paid: BTreeMap::new(), enabled: false, last_error: None }
    }
}

impl RelayState {
    pub fn to_json(&self) -> serde_json::Value {
        serde_json::json!({"enabled": self.enabled, "vaultMode": self.vault_mode, "lastError": self.last_error})
    }
}

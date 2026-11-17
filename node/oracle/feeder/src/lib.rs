//! PosternDEX Perpetuals oracle feeder (UNAUDITED PROTOTYPE): venue adapters, the per-round observation
//! pipeline (README section 7), the observation signer, JSON forms, and the record / replay tooling.
//!
//! The network fetch lives behind the `net` feature; everything else is offline and deterministic on recorded
//! responses, so a fixture set replays to the same observations on every machine.

#![forbid(unsafe_code)]

pub mod json;
pub mod keyfile;
pub mod pipeline;
pub mod venues;

#[cfg(feature = "net")]
pub mod net;

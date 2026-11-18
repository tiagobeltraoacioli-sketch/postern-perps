//! PosternDEX Perpetuals geoblocking and sanctions decision for the node's admission (agent 389).
//! UNAUDITED PROTOTYPE. Not legal advice. Rust port of `compliance/geoblock/src/*.mjs` (the reference),
//! held to the same conformance vectors (`compliance/geoblock/vectors/`, run by tests/vectors.rs).
//!
//! Use at admission: verify the edge's assertion (`assertion::verify`), build the decision request JSON from it
//! plus kind/action/addresses (add the RECOVERED signer with role "signer"), the attestation you hold
//! (`attestation::verify` -> `request_field()`), `nowMs`, `mode`, then `decide::decide(list, &req, sanctions)`.

pub mod addr;
pub mod assertion;
pub mod attestation;
pub mod decide;
pub mod list;
pub mod sanctions;

pub use decide::{decide, geo_status, Decision};
pub use list::{load_list, List, ListError};
pub use sanctions::{load_sanctions, Sanctions};

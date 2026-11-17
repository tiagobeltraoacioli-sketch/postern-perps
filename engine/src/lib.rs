//! PosternDEX Perpetuals engine. UNAUDITED PROTOTYPE.
//!
//! A deterministic exchange state machine: the same genesis and the same ordered input log give the same state
//! root on every machine. Normative rules: `docs/SPEC.md` (rule ids `S-…`, invariants `I-…` are cited in code).
#![forbid(unsafe_code)]

pub mod num;
pub mod hash;
pub mod eip712;
pub mod types;
pub mod codec;
pub mod genesis;
pub mod book;
pub mod oracle;
pub mod merkle;
pub mod engine;
pub mod invariants;
pub mod testkit;

pub use engine::{BlockError, BlockOutcome, Engine, Event};
pub use types::*;

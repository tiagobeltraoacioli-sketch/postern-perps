//! Block inputs, block header, block, and their canonical byte encodings.
//!
//! PROVISIONAL until `docs/SPEC.md` (agent 388) fixes the envelope: the encodings below are the node's proposal
//! (cross-area line of 389, 2026-11-17 03:50 UTC); the oracle payload is agent 394's format verbatim
//! (`node/oracle/README.md` section 4).
//!
//! ```text
//! inputsEncoded = count:u32be  Input{count}
//! Input         = kind:u8  len:u32be  payload[len]
//!   kind 0 oracle   payload = OracleUpdate (394)
//!   kind 1 deposit  payload = index:u64 account:20 amount:u128 l2Block:u64 l2BlockHash:32 l2TxHash:32 logIndex:u32
//!   kind 2 forced   payload = index:u64 account:20 kind:u8 deadline:u64 l2Block:u64 l2BlockHash:32 l2TxHash:32 logIndex:u32
//!   kind 3 action   payload = typeIndex:u8 message(canonical typed encoding) signature:65
//! ```
//! Kinds are non-decreasing inside a block (oracle, deposits, forced, actions); every length, count, bool byte and
//! ordering rule is part of the canonical form, and decoding refuses trailing bytes, so one block has exactly one
//! encoding and `logHash` commits to it.
//!
//! UNAUDITED PROTOTYPE.

use crate::eip712::{Reader, Registry, Val};
use crate::util::{keccak, keccak_parts, to_hex, Address, H256};

pub const MAX_INPUTS_PER_BLOCK: usize = 20_000;
pub const MAX_INPUT_BYTES: usize = 16 * 1024;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DepositEvent {
    pub index: u64,
    pub account: Address,
    pub amount: u128,
    pub l2_block: u64,
    pub l2_block_hash: H256,
    pub l2_tx_hash: H256,
    pub log_index: u32,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ForcedKind {
    CancelAll = 0,
    Close = 1,
    Withdraw = 2,
}

impl ForcedKind {
    pub fn from_u8(v: u8) -> Option<ForcedKind> {
        match v {
            0 => Some(ForcedKind::CancelAll),
            1 => Some(ForcedKind::Close),
            2 => Some(ForcedKind::Withdraw),
            _ => None,
        }
    }
    pub fn name(&self) -> &'static str {
        match self {
            ForcedKind::CancelAll => "cancelAll",
            ForcedKind::Close => "close",
            ForcedKind::Withdraw => "withdraw",
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ForcedEvent {
    pub index: u64,
    pub account: Address,
    pub kind: ForcedKind,
    pub deadline: u64,
    pub l2_block: u64,
    pub l2_block_hash: H256,
    pub l2_tx_hash: H256,
    pub log_index: u32,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct OraclePrice {
    pub market_id: u32,
    pub price: u64,
    pub sources: u8,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SignedObservation {
    pub signer: u8,
    pub time_ms: u64,
    pub prices: Vec<OraclePrice>,
    pub signature: [u8; 65],
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct OracleUpdate {
    pub round: u64,
    pub observations: Vec<SignedObservation>,
}

/// A user action: the EIP-712 primary type (index into the genesis action list), the typed message, the signature.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SignedAction {
    pub type_index: u8,
    pub message: Val,
    pub signature: [u8; 65],
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Input {
    Oracle(OracleUpdate),
    Deposit(DepositEvent),
    Forced(ForcedEvent),
    Action(SignedAction),
}

impl Input {
    pub fn kind(&self) -> u8 {
        match self {
            Input::Oracle(_) => 0,
            Input::Deposit(_) => 1,
            Input::Forced(_) => 2,
            Input::Action(_) => 3,
        }
    }
}

/// Typed data of the genesis: the EIP-712 registry and the ordered action list (index = `typeIndex`).
#[derive(Clone, Debug)]
pub struct ActionTypes {
    pub registry: Registry,
    pub actions: Vec<String>,
    pub agent_allowed: Vec<String>,
}

impl ActionTypes {
    pub fn index_of(&self, primary: &str) -> Option<u8> {
        self.actions.iter().position(|a| a == primary).map(|i| i as u8)
    }
    pub fn name(&self, idx: u8) -> Option<&str> {
        self.actions.get(idx as usize).map(|s| s.as_str())
    }
}

pub fn encode_inputs(types: &ActionTypes, inputs: &[Input]) -> Result<Vec<u8>, String> {
    if inputs.len() > MAX_INPUTS_PER_BLOCK {
        return Err("too many inputs".into());
    }
    let mut out = Vec::new();
    out.extend_from_slice(&(inputs.len() as u32).to_be_bytes());
    let mut last_kind = 0u8;
    for inp in inputs {
        if inp.kind() < last_kind {
            return Err("input kinds must be non-decreasing (oracle, deposit, forced, action)".into());
        }
        last_kind = inp.kind();
        let payload = encode_payload(types, inp)?;
        if payload.len() > MAX_INPUT_BYTES {
            return Err("input too large".into());
        }
        out.push(inp.kind());
        out.extend_from_slice(&(payload.len() as u32).to_be_bytes());
        out.extend_from_slice(&payload);
    }
    Ok(out)
}

pub fn encode_payload(types: &ActionTypes, inp: &Input) -> Result<Vec<u8>, String> {
    let mut p = Vec::new();
    match inp {
        Input::Oracle(u) => encode_oracle(u, &mut p)?,
        Input::Deposit(d) => {
            p.extend_from_slice(&d.index.to_be_bytes());
            p.extend_from_slice(&d.account);
            p.extend_from_slice(&d.amount.to_be_bytes());
            p.extend_from_slice(&d.l2_block.to_be_bytes());
            p.extend_from_slice(&d.l2_block_hash);
            p.extend_from_slice(&d.l2_tx_hash);
            p.extend_from_slice(&d.log_index.to_be_bytes());
        }
        Input::Forced(f) => {
            p.extend_from_slice(&f.index.to_be_bytes());
            p.extend_from_slice(&f.account);
            p.push(f.kind as u8);
            p.extend_from_slice(&f.deadline.to_be_bytes());
            p.extend_from_slice(&f.l2_block.to_be_bytes());
            p.extend_from_slice(&f.l2_block_hash);
            p.extend_from_slice(&f.l2_tx_hash);
            p.extend_from_slice(&f.log_index.to_be_bytes());
        }
        Input::Action(a) => {
            let name = types.name(a.type_index).ok_or("unknown action type index")?;
            p.push(a.type_index);
            types.registry.encode_canonical(name, &a.message, &mut p)?;
            p.extend_from_slice(&a.signature);
        }
    }
    Ok(p)
}

fn encode_oracle(u: &OracleUpdate, p: &mut Vec<u8>) -> Result<(), String> {
    if u.observations.len() > 255 {
        return Err("too many observations".into());
    }
    p.extend_from_slice(&u.round.to_be_bytes());
    p.push(u.observations.len() as u8);
    for o in &u.observations {
        if o.prices.len() > 256 {
            return Err("too many prices".into());
        }
        p.push(o.signer);
        p.extend_from_slice(&o.time_ms.to_be_bytes());
        p.extend_from_slice(&(o.prices.len() as u16).to_be_bytes());
        for pr in &o.prices {
            p.extend_from_slice(&pr.market_id.to_be_bytes());
            p.extend_from_slice(&pr.price.to_be_bytes());
            p.push(pr.sources);
        }
        p.extend_from_slice(&o.signature);
    }
    Ok(())
}

pub fn decode_inputs(types: &ActionTypes, buf: &[u8]) -> Result<Vec<Input>, String> {
    let mut r = Reader::new(buf);
    let n = r.u32()? as usize;
    if n > MAX_INPUTS_PER_BLOCK {
        return Err("too many inputs".into());
    }
    let mut out = Vec::with_capacity(n);
    let mut last_kind = 0u8;
    for _ in 0..n {
        let kind = r.u8()?;
        if kind < last_kind {
            return Err("input kinds must be non-decreasing".into());
        }
        last_kind = kind;
        let len = r.u32()? as usize;
        if len > MAX_INPUT_BYTES {
            return Err("input too large".into());
        }
        let payload = r.take(len)?;
        let mut pr = Reader::new(payload);
        let inp = match kind {
            0 => Input::Oracle(decode_oracle(&mut pr)?),
            1 => Input::Deposit(DepositEvent {
                index: pr.u64()?,
                account: pr.addr()?,
                amount: pr.u128()?,
                l2_block: pr.u64()?,
                l2_block_hash: pr.h256()?,
                l2_tx_hash: pr.h256()?,
                log_index: pr.u32()?,
            }),
            2 => {
                let index = pr.u64()?;
                let account = pr.addr()?;
                let kind = ForcedKind::from_u8(pr.u8()?).ok_or("unknown forced kind")?;
                Input::Forced(ForcedEvent {
                    index,
                    account,
                    kind,
                    deadline: pr.u64()?,
                    l2_block: pr.u64()?,
                    l2_block_hash: pr.h256()?,
                    l2_tx_hash: pr.h256()?,
                    log_index: pr.u32()?,
                })
            }
            3 => {
                let type_index = pr.u8()?;
                let name = types.name(type_index).ok_or("unknown action type index")?;
                let message = types.registry.decode_canonical(name, &mut pr)?;
                let signature: [u8; 65] = pr.take(65)?.try_into().unwrap();
                Input::Action(SignedAction { type_index, message, signature })
            }
            _ => return Err(format!("unknown input kind {kind}")),
        };
        pr.expect_done()?;
        out.push(inp);
    }
    r.expect_done()?;
    Ok(out)
}

fn decode_oracle(r: &mut Reader) -> Result<OracleUpdate, String> {
    let round = r.u64()?;
    let count = r.u8()? as usize;
    let mut observations = Vec::with_capacity(count);
    for _ in 0..count {
        let signer = r.u8()?;
        let time_ms = r.u64()?;
        let pc = r.u16()? as usize;
        if pc > 256 {
            return Err("too many prices".into());
        }
        let mut prices = Vec::with_capacity(pc);
        for _ in 0..pc {
            prices.push(OraclePrice { market_id: r.u32()?, price: r.u64()?, sources: r.u8()? });
        }
        let signature: [u8; 65] = r.take(65)?.try_into().unwrap();
        observations.push(SignedObservation { signer, time_ms, prices, signature });
    }
    Ok(OracleUpdate { round, observations })
}

/// Block header. `logHash` commits to the inputs; `stateRoot` is the engine's output after them.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BlockHeader {
    pub height: u64,
    pub time: u64,
    pub prev_log_hash: H256,
    pub log_hash: H256,
    pub state_root: H256,
}

pub fn log_hash(prev: &H256, height: u64, time: u64, inputs_encoded: &[u8]) -> H256 {
    keccak_parts(&[prev, &height.to_be_bytes(), &time.to_be_bytes(), &keccak(inputs_encoded)])
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Block {
    pub header: BlockHeader,
    pub inputs_encoded: Vec<u8>,
    pub sequencer_signature: [u8; 65],
}

impl Block {
    /// Serialisation used by the input log file and `GET /v1/blocks` (`raw`).
    pub fn to_bytes(&self) -> Vec<u8> {
        let h = &self.header;
        let mut b = Vec::with_capacity(8 + 8 + 96 + 4 + self.inputs_encoded.len() + 65);
        b.extend_from_slice(&h.height.to_be_bytes());
        b.extend_from_slice(&h.time.to_be_bytes());
        b.extend_from_slice(&h.prev_log_hash);
        b.extend_from_slice(&h.log_hash);
        b.extend_from_slice(&h.state_root);
        b.extend_from_slice(&(self.inputs_encoded.len() as u32).to_be_bytes());
        b.extend_from_slice(&self.inputs_encoded);
        b.extend_from_slice(&self.sequencer_signature);
        b
    }

    pub fn from_bytes(buf: &[u8]) -> Result<Block, String> {
        let mut r = Reader::new(buf);
        let height = r.u64()?;
        let time = r.u64()?;
        let prev_log_hash = r.h256()?;
        let lh = r.h256()?;
        let state_root = r.h256()?;
        let n = r.u32()? as usize;
        let inputs_encoded = r.take(n)?.to_vec();
        let sequencer_signature: [u8; 65] = r.take(65)?.try_into().unwrap();
        r.expect_done()?;
        Ok(Block { header: BlockHeader { height, time, prev_log_hash, log_hash: lh, state_root }, inputs_encoded, sequencer_signature })
    }

    /// The block's own consistency: `logHash` recomputed from its fields.
    pub fn check_log_hash(&self) -> Result<(), String> {
        let h = &self.header;
        if log_hash(&h.prev_log_hash, h.height, h.time, &self.inputs_encoded) != h.log_hash {
            return Err(format!("block {}: logHash does not match its inputs", h.height));
        }
        Ok(())
    }
}

pub fn deposit_json(d: &DepositEvent) -> serde_json::Value {
    serde_json::json!({
        "index": d.index.to_string(), "account": to_hex(&d.account), "amount": d.amount.to_string(),
        "l2Block": d.l2_block, "l2BlockHash": to_hex(&d.l2_block_hash), "l2TxHash": to_hex(&d.l2_tx_hash), "logIndex": d.log_index
    })
}

pub fn forced_json(f: &ForcedEvent) -> serde_json::Value {
    serde_json::json!({
        "index": f.index.to_string(), "account": to_hex(&f.account), "kind": f.kind.name(), "deadline": f.deadline,
        "l2Block": f.l2_block, "l2BlockHash": to_hex(&f.l2_block_hash), "l2TxHash": to_hex(&f.l2_tx_hash), "logIndex": f.log_index
    })
}

pub fn oracle_json(u: &OracleUpdate) -> serde_json::Value {
    serde_json::json!({
        "round": u.round,
        "observations": u.observations.iter().map(|o| serde_json::json!({
            "signer": o.signer, "timeMs": o.time_ms,
            "prices": o.prices.iter().map(|p| serde_json::json!({"marketId": p.market_id, "price": p.price.to_string(), "sources": p.sources})).collect::<Vec<_>>(),
            "signature": to_hex(&o.signature)
        })).collect::<Vec<_>>()
    })
}

pub fn input_json(types: &ActionTypes, inp: &Input) -> serde_json::Value {
    match inp {
        Input::Oracle(u) => serde_json::json!({"kind": "oracle", "payload": oracle_json(u)}),
        Input::Deposit(d) => serde_json::json!({"kind": "deposit", "payload": deposit_json(d)}),
        Input::Forced(f) => serde_json::json!({"kind": "forced", "payload": forced_json(f)}),
        Input::Action(a) => {
            let name = types.name(a.type_index).unwrap_or("?");
            serde_json::json!({"kind": "action", "type": name,
                "message": types.registry.val_to_json(name, &a.message), "signature": to_hex(&a.signature)})
        }
    }
}

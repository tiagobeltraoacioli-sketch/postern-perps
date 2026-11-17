//! `StubEngine`: the node's stand-in LEDGER until agent 388's `engine/` lands. NOT THE EXCHANGE: no matching, no
//! margin, no funding, no liquidation, no positions. It keeps exactly what the node's own paths need and applies
//! the node-level rules for real (signature, authority, nonce window, deposit and forced ordering, withdrawals,
//! oracle quorum), so the sequencer / validator / checkpoint / certificate machinery is tested on real rules.
//!
//! Determinism: `BTreeMap` only, integer arithmetic only, checked everywhere (an overflow is a refusal), no clock.
//! State root: `keccak256("PosternPerps stub state v0" || snapshot())`: a commitment, not a Merkle tree (388's
//! engine provides the per-account provable root).
//!
//! UNAUDITED PROTOTYPE.

use super::{merkle_proof, merkle_root, AuthorityError, BlockOutcome, Engine, InputOutcome, WithdrawalRecord};
use crate::actions::{field_addr, field_bool, field_u, verify_signed_action, ActionError};
use crate::eip712::{digest, Domain, Reader, Registry, Val};
use crate::genesis::{Genesis, NONCE_FUTURE_MS, NONCE_PAST_MS};
use crate::sig::recover;
use crate::types::{ForcedKind, Input, OracleUpdate};
use crate::util::{keccak_parts, scaled_to_decimal, to_hex, Address, H256};
use std::collections::BTreeMap;
use std::sync::{Arc, OnceLock};

#[derive(Clone, Debug, PartialEq, Eq)]
struct Order {
    account: Address,
    market_id: u32,
    is_buy: bool,
    price: u64,
    size: u64,
    reduce_only: bool,
    height: u64,
}

#[derive(Clone)]
pub struct StubEngine {
    g: Arc<Genesis>,
    height: u64,
    time: u64,
    collateral: BTreeMap<Address, u128>,
    nonces: BTreeMap<Address, u64>,
    /// (account, agent) -> (expiresAt, approvedAt)
    agents: BTreeMap<(Address, Address), (u64, u64)>,
    orders: BTreeMap<u64, Order>,
    next_order_id: u64,
    withdrawals: Vec<WithdrawalRecord>,
    next_deposit: u64,
    next_forced: u64,
    oracle_round: u64,
    prices: BTreeMap<u32, u64>,
    /// cached withdrawal leaves (derived, not state)
    leaves: Vec<H256>,
}

pub fn oracle_registry() -> &'static Registry {
    static R: OnceLock<Registry> = OnceLock::new();
    R.get_or_init(|| {
        Registry::from_json(&serde_json::json!({
            "OracleObservation": [{"name":"round","type":"uint64"},{"name":"timeMs","type":"uint64"},{"name":"prices","type":"OraclePrice[]"}],
            "OraclePrice": [{"name":"marketId","type":"uint32"},{"name":"price","type":"uint64"},{"name":"sources","type":"uint8"}]
        }))
        .expect("static oracle types")
    })
}

/// Digest an oracle signer signs (agent 394's `OracleObservation`, engine domain).
pub fn oracle_digest(domain: &Domain, round: u64, time_ms: u64, prices: &[crate::types::OraclePrice]) -> H256 {
    let v = Val::Struct(vec![
        Val::Uint(round as u128),
        Val::Uint(time_ms as u128),
        Val::Array(
            prices
                .iter()
                .map(|p| Val::Struct(vec![Val::Uint(p.market_id as u128), Val::Uint(p.price as u128), Val::Uint(p.sources as u128)]))
                .collect(),
        ),
    ]);
    let sh = oracle_registry().hash_struct("OracleObservation", &v).expect("static oracle types");
    digest(&domain.separator(), &sh)
}

/// `ROUND_MS` of agent 394.
pub const ORACLE_ROUND_MS: u64 = 3000;

fn ok(result: serde_json::Value, ah: Option<H256>) -> InputOutcome {
    InputOutcome { ok: true, error: None, action_hash: ah, result }
}
fn fail(code: &str, ah: Option<H256>) -> InputOutcome {
    InputOutcome { ok: false, error: Some(code.to_string()), action_hash: ah, result: serde_json::Value::Null }
}

impl StubEngine {
    pub fn new(g: Arc<Genesis>) -> StubEngine {
        let mut e = StubEngine {
            g,
            height: 0,
            time: 0,
            collateral: BTreeMap::new(),
            nonces: BTreeMap::new(),
            agents: BTreeMap::new(),
            orders: BTreeMap::new(),
            next_order_id: 1,
            withdrawals: Vec::new(),
            next_deposit: 0,
            next_forced: 0,
            oracle_round: 0,
            prices: BTreeMap::new(),
            leaves: Vec::new(),
        };
        e.time = e.g.genesis_time;
        e
    }

    fn credit(&mut self, a: &Address, v: u128) -> Result<(), &'static str> {
        let c = self.collateral.entry(*a).or_insert(0);
        *c = c.checked_add(v).ok_or("OVERFLOW")?;
        Ok(())
    }

    fn debit(&mut self, a: &Address, v: u128) -> Result<(), &'static str> {
        let c = self.collateral.get(a).copied().unwrap_or(0);
        let n = c.checked_sub(v).ok_or("INSUFFICIENT_COLLATERAL")?;
        if n == 0 {
            self.collateral.remove(a);
        } else {
            self.collateral.insert(*a, n);
        }
        Ok(())
    }

    fn withdraw(&mut self, account: Address, recipient: Address, amount: u128, out: &mut BlockOutcome) -> Result<u64, &'static str> {
        if amount == 0 {
            return Err("ZERO_AMOUNT");
        }
        if recipient == [0u8; 20] {
            return Err("BAD_RECIPIENT");
        }
        let fee = self.g.withdrawal_fee;
        let total = amount.checked_add(fee).ok_or("OVERFLOW")?;
        self.debit(&account, total)?;
        if fee > 0 {
            let fr = self.g.fee_recipient;
            self.credit(&fr, fee)?;
        }
        let id = self.withdrawals.len() as u64;
        let rec = WithdrawalRecord { id, account, recipient, amount, fee, height: self.height };
        self.leaves.push(rec.leaf());
        out.events.push(serde_json::json!({"type": "withdrawal", "withdrawal": rec.to_json()}));
        self.withdrawals.push(rec);
        out.withdrawals.push(id);
        Ok(id)
    }

    fn cancel_all(&mut self, account: &Address, market_id: u32) -> u64 {
        let ids: Vec<u64> = self
            .orders
            .iter()
            .filter(|(_, o)| &o.account == account && (market_id == 0 || o.market_id == market_id))
            .map(|(id, _)| *id)
            .collect();
        for id in &ids {
            self.orders.remove(id);
        }
        ids.len() as u64
    }

    fn apply_oracle(&mut self, u: &OracleUpdate, time: u64) -> InputOutcome {
        // Simplified acceptance (394's README section 5 is the real rule, implemented by 388's engine):
        // new round, quorum of distinct ascending signers, valid signatures, observation inside its round and
        // not after the block time, per-market median of the observations that price it (lower median).
        if u.round <= self.oracle_round {
            return fail("ORACLE_ROUND_NOT_NEW", None);
        }
        let n = self.g.oracle_committee.len();
        if u.observations.len() < self.g.oracle_quorum || u.observations.len() > n {
            return fail("ORACLE_MALFORMED", None);
        }
        let mut last: Option<u8> = None;
        let mut per_market: BTreeMap<u32, Vec<u64>> = BTreeMap::new();
        for o in &u.observations {
            if (o.signer as usize) >= n || last.is_some_and(|l| o.signer <= l) {
                return fail("ORACLE_MALFORMED", None);
            }
            last = Some(o.signer);
            if o.time_ms / ORACLE_ROUND_MS != u.round || o.time_ms > time {
                return fail("ORACLE_TIME", None);
            }
            let mut lm: Option<u32> = None;
            for p in &o.prices {
                if lm.is_some_and(|l| p.market_id <= l) || p.price == 0 || self.g.market(p.market_id).is_none() {
                    return fail("ORACLE_MALFORMED", None);
                }
                lm = Some(p.market_id);
            }
            let d = oracle_digest(&self.g.engine_domain, u.round, o.time_ms, &o.prices);
            match recover(&d, &o.signature) {
                Ok(a) if a == self.g.oracle_committee[o.signer as usize] => {}
                _ => return fail("ORACLE_BAD_SIGNATURE", None),
            }
            for p in &o.prices {
                per_market.entry(p.market_id).or_default().push(p.price);
            }
        }
        self.oracle_round = u.round;
        let mut applied = serde_json::Map::new();
        for (m, mut v) in per_market {
            if v.len() < self.g.oracle_quorum {
                continue;
            }
            v.sort_unstable();
            let med = v[(v.len() - 1) / 2];
            self.prices.insert(m, med);
            applied.insert(m.to_string(), serde_json::Value::String(med.to_string()));
        }
        ok(serde_json::json!({"round": u.round, "prices": applied}), None)
    }

    fn apply_action(&mut self, a: &crate::types::SignedAction, time: u64, out: &mut BlockOutcome) -> InputOutcome {
        let g = self.g.clone();
        let v = match verify_signed_action(&g, a) {
            Ok(v) => v,
            Err(ActionError::BadSignature(_)) => return fail("BAD_SIGNATURE", None),
            Err(ActionError::Malformed(_)) => return fail("MALFORMED", None),
        };
        let ah = Some(v.action_hash);
        if let Err(e) = self.authority(&v.account, &v.signer, &v.primary, time) {
            return fail(if e == AuthorityError::UnknownSigner { "UNKNOWN_SIGNER" } else { "AGENT_NOT_ALLOWED" }, ah);
        }
        let hi = self.nonces.get(&v.signer).copied();
        if hi.is_some_and(|h| v.nonce <= h) {
            return fail("BAD_NONCE", ah);
        }
        if v.nonce.saturating_add(NONCE_PAST_MS) < time || v.nonce > time.saturating_add(NONCE_FUTURE_MS) {
            return fail("NONCE_OUTSIDE_WINDOW", ah);
        }
        // The nonce is consumed by every authentic, authorised action, whatever the business outcome:
        // a refused action can never be replayed later when it would succeed.
        self.nonces.insert(v.signer, v.nonce);
        let p = v.primary.as_str();
        let msg = &a.message;
        let r = match p {
            "PlaceOrder" => (|| {
                let mid = field_u(&g, p, msg, "marketId").ok_or("MALFORMED")? as u32;
                if g.market(mid).is_none() {
                    return Err("UNKNOWN_MARKET");
                }
                let size = field_u(&g, p, msg, "size").ok_or("MALFORMED")? as u64;
                let price = field_u(&g, p, msg, "price").ok_or("MALFORMED")? as u64;
                if size == 0 {
                    return Err("ZERO_SIZE");
                }
                if price == 0 {
                    // node rule agreed with 391: a market order carries a worst price; 0 ("any price") is refused
                    return Err("PRICE_REQUIRED");
                }
                let id = self.next_order_id;
                self.next_order_id = self.next_order_id.checked_add(1).ok_or("OVERFLOW")?;
                self.orders.insert(
                    id,
                    Order {
                        account: v.account,
                        market_id: mid,
                        is_buy: field_bool(&g, p, msg, "isBuy").unwrap_or(false),
                        price,
                        size,
                        reduce_only: field_bool(&g, p, msg, "reduceOnly").unwrap_or(false),
                        height: self.height,
                    },
                );
                Ok(serde_json::json!({"orderId": id.to_string(), "orderStatus": "open", "filledSize": "0", "stub": "no matching in the node stub engine"}))
            })(),
            "CancelOrder" => (|| {
                let oid = field_u(&g, p, msg, "orderId").ok_or("MALFORMED")? as u64;
                match self.orders.get(&oid) {
                    Some(o) if o.account == v.account => {
                        self.orders.remove(&oid);
                        Ok(serde_json::json!({"orderId": oid.to_string(), "orderStatus": "canceled"}))
                    }
                    _ => Err("ORDER_NOT_FOUND"),
                }
            })(),
            "CancelAll" => (|| {
                let mid = field_u(&g, p, msg, "marketId").ok_or("MALFORMED")? as u32;
                Ok(serde_json::json!({"canceled": self.cancel_all(&v.account, mid)}))
            })(),
            "Withdraw" => (|| {
                let amount = field_u(&g, p, msg, "amount").ok_or("MALFORMED")?;
                let recipient = field_addr(&g, p, msg, "recipient").ok_or("MALFORMED")?;
                let id = self.withdraw(v.account, recipient, amount, out)?;
                Ok(serde_json::json!({"withdrawalId": id.to_string()}))
            })(),
            "ApproveAgent" => (|| {
                let agent = field_addr(&g, p, msg, "agent").ok_or("MALFORMED")?;
                let exp = field_u(&g, p, msg, "expiresAt").ok_or("MALFORMED")? as u64;
                if agent == v.account || agent == [0u8; 20] {
                    return Err("BAD_AGENT");
                }
                if exp <= time {
                    return Err("EXPIRED");
                }
                self.agents.insert((v.account, agent), (exp, time));
                Ok(serde_json::json!({"agent": to_hex(&agent), "expiresAt": exp}))
            })(),
            "RevokeAgent" => (|| {
                let agent = field_addr(&g, p, msg, "agent").ok_or("MALFORMED")?;
                self.agents.remove(&(v.account, agent)).ok_or("AGENT_NOT_FOUND")?;
                Ok(serde_json::json!({"agent": to_hex(&agent), "revoked": true}))
            })(),
            "UpdateLeverage" | "UpdateMargin" => Ok(serde_json::json!({"stub": "not modelled by the node stub engine"})),
            _ => Err("NOT_SUPPORTED"),
        };
        match r {
            Ok(res) => ok(res, ah),
            Err(code) => fail(code, ah),
        }
    }

    fn serialize(&self) -> Vec<u8> {
        let mut b = Vec::new();
        b.extend_from_slice(b"STUB0");
        b.extend_from_slice(&self.height.to_be_bytes());
        b.extend_from_slice(&self.time.to_be_bytes());
        b.extend_from_slice(&(self.collateral.len() as u64).to_be_bytes());
        for (a, v) in &self.collateral {
            b.extend_from_slice(a);
            b.extend_from_slice(&v.to_be_bytes());
        }
        b.extend_from_slice(&(self.nonces.len() as u64).to_be_bytes());
        for (a, v) in &self.nonces {
            b.extend_from_slice(a);
            b.extend_from_slice(&v.to_be_bytes());
        }
        b.extend_from_slice(&(self.agents.len() as u64).to_be_bytes());
        for ((a, ag), (e, t)) in &self.agents {
            b.extend_from_slice(a);
            b.extend_from_slice(ag);
            b.extend_from_slice(&e.to_be_bytes());
            b.extend_from_slice(&t.to_be_bytes());
        }
        b.extend_from_slice(&(self.orders.len() as u64).to_be_bytes());
        for (id, o) in &self.orders {
            b.extend_from_slice(&id.to_be_bytes());
            b.extend_from_slice(&o.account);
            b.extend_from_slice(&o.market_id.to_be_bytes());
            b.push(o.is_buy as u8);
            b.extend_from_slice(&o.price.to_be_bytes());
            b.extend_from_slice(&o.size.to_be_bytes());
            b.push(o.reduce_only as u8);
            b.extend_from_slice(&o.height.to_be_bytes());
        }
        b.extend_from_slice(&self.next_order_id.to_be_bytes());
        b.extend_from_slice(&(self.withdrawals.len() as u64).to_be_bytes());
        for w in &self.withdrawals {
            b.extend_from_slice(&w.id.to_be_bytes());
            b.extend_from_slice(&w.account);
            b.extend_from_slice(&w.recipient);
            b.extend_from_slice(&w.amount.to_be_bytes());
            b.extend_from_slice(&w.fee.to_be_bytes());
            b.extend_from_slice(&w.height.to_be_bytes());
        }
        b.extend_from_slice(&self.next_deposit.to_be_bytes());
        b.extend_from_slice(&self.next_forced.to_be_bytes());
        b.extend_from_slice(&self.oracle_round.to_be_bytes());
        b.extend_from_slice(&(self.prices.len() as u64).to_be_bytes());
        for (m, p) in &self.prices {
            b.extend_from_slice(&m.to_be_bytes());
            b.extend_from_slice(&p.to_be_bytes());
        }
        b
    }

    fn deserialize(&mut self, buf: &[u8]) -> Result<(), String> {
        let mut r = Reader::new(buf);
        if r.take(5)? != b"STUB0" {
            return Err("not a stub engine snapshot".into());
        }
        let mut e = StubEngine::new(self.g.clone());
        e.height = r.u64()?;
        e.time = r.u64()?;
        let bound = |n: u64| -> Result<u64, String> { if n > (buf.len() as u64) { Err("count larger than the snapshot".into()) } else { Ok(n) } };
        let mut last1: Option<Address> = None;
        for _ in 0..bound(r.u64()?)? {
            let a = r.addr()?;
            let v = r.u128()?;
            if last1.is_some_and(|l| a <= l) {
                return Err("balance keys not ascending".into());
            }
            last1 = Some(a);
            if v == 0 {
                return Err("zero balance entry".into());
            }
            e.collateral.insert(a, v);
        }
        let mut last2: Option<Address> = None;
        for _ in 0..bound(r.u64()?)? {
            let a = r.addr()?;
            if last2.is_some_and(|l| a <= l) {
                return Err("nonce keys not ascending".into());
            }
            last2 = Some(a);
            e.nonces.insert(a, r.u64()?);
        }
        let mut last3: Option<(Address, Address)> = None;
        for _ in 0..bound(r.u64()?)? {
            let k = (r.addr()?, r.addr()?);
            if last3.is_some_and(|l| k <= l) {
                return Err("agent keys not ascending".into());
            }
            last3 = Some(k);
            let ex = r.u64()?;
            let t = r.u64()?;
            e.agents.insert(k, (ex, t));
        }
        let mut last4: Option<u64> = None;
        for _ in 0..bound(r.u64()?)? {
            let id = r.u64()?;
            if last4.is_some_and(|l| id <= l) {
                return Err("order ids not ascending".into());
            }
            last4 = Some(id);
            let account = r.addr()?;
            let market_id = r.u32()?;
            let is_buy = bool_byte(r.u8()?)?;
            let price = r.u64()?;
            let size = r.u64()?;
            let reduce_only = bool_byte(r.u8()?)?;
            let height = r.u64()?;
            e.orders.insert(id, Order { account, market_id, is_buy, price, size, reduce_only, height });
        }
        e.next_order_id = r.u64()?;
        for i in 0..bound(r.u64()?)? {
            let w = WithdrawalRecord { id: r.u64()?, account: r.addr()?, recipient: r.addr()?, amount: r.u128()?, fee: r.u128()?, height: r.u64()? };
            if w.id != i {
                return Err("withdrawal ids not consecutive".into());
            }
            e.leaves.push(w.leaf());
            e.withdrawals.push(w);
        }
        e.next_deposit = r.u64()?;
        e.next_forced = r.u64()?;
        e.oracle_round = r.u64()?;
        let mut last5: Option<u32> = None;
        for _ in 0..bound(r.u64()?)? {
            let m = r.u32()?;
            if last5.is_some_and(|l| m <= l) {
                return Err("price keys not ascending".into());
            }
            last5 = Some(m);
            e.prices.insert(m, r.u64()?);
        }
        r.expect_done()?;
        if e.serialize() != buf {
            return Err("snapshot is not in canonical form".into());
        }
        *self = e;
        Ok(())
    }
}

fn bool_byte(b: u8) -> Result<bool, String> {
    match b {
        0 => Ok(false),
        1 => Ok(true),
        _ => Err("bool byte".into()),
    }
}

impl Engine for StubEngine {
    fn apply_block(&mut self, height: u64, time: u64, inputs: &[Input]) -> Result<BlockOutcome, String> {
        // ---- structural pre-pass: any failure refuses the whole block, state untouched ----
        if height != self.height + 1 {
            return Err(format!("height {height} does not follow {}", self.height));
        }
        if time <= self.time {
            return Err(format!("block time {time} not after {}", self.time));
        }
        let (mut nd, mut nf, mut oracles) = (self.next_deposit, self.next_forced, 0);
        for inp in inputs {
            match inp {
                Input::Oracle(_) => {
                    oracles += 1;
                    if oracles > 1 {
                        return Err("more than one oracle update in a block".into());
                    }
                }
                Input::Deposit(d) => {
                    if d.index != nd {
                        return Err(format!("deposit index {} where {nd} is next (vault order, no gap, no repeat)", d.index));
                    }
                    if d.amount == 0 || d.account == [0u8; 20] {
                        return Err("deposit with zero amount or zero account".into());
                    }
                    nd += 1;
                }
                Input::Forced(f) => {
                    if f.index != nf {
                        return Err(format!("forced index {} where {nf} is next", f.index));
                    }
                    nf += 1;
                }
                Input::Action(_) => {}
            }
        }
        // ---- apply ----
        self.height = height;
        self.time = time;
        let mut out = BlockOutcome::default();
        for inp in inputs {
            let o = match inp {
                Input::Oracle(u) => self.apply_oracle(u, time),
                Input::Deposit(d) => {
                    self.next_deposit += 1;
                    match self.credit(&d.account, d.amount) {
                        Ok(()) => {
                            out.events.push(serde_json::json!({"type": "deposit", "index": d.index.to_string(), "account": to_hex(&d.account), "amount": d.amount.to_string()}));
                            ok(serde_json::json!({"depositId": d.index.to_string(), "status": "credited"}), None)
                        }
                        Err(c) => fail(c, None),
                    }
                }
                Input::Forced(f) => {
                    self.next_forced += 1;
                    let r = match f.kind {
                        ForcedKind::CancelAll | ForcedKind::Close => {
                            let n = self.cancel_all(&f.account, 0);
                            Ok(serde_json::json!({"forcedId": f.index.to_string(), "canceled": n}))
                        }
                        ForcedKind::Withdraw => {
                            let bal = self.collateral.get(&f.account).copied().unwrap_or(0);
                            if bal > self.g.withdrawal_fee {
                                self.withdraw(f.account, f.account, bal - self.g.withdrawal_fee, &mut out)
                                    .map(|id| serde_json::json!({"forcedId": f.index.to_string(), "withdrawalId": id.to_string()}))
                            } else {
                                Ok(serde_json::json!({"forcedId": f.index.to_string(), "withdrawalId": null, "reason": "balance not above the withdrawal fee"}))
                            }
                        }
                    };
                    match r {
                        Ok(v) => ok(v, None),
                        Err(c) => fail(c, None),
                    }
                }
                Input::Action(a) => self.apply_action(a, time, &mut out),
            };
            out.outcomes.push(o);
        }
        Ok(out)
    }

    fn state_root(&self) -> H256 {
        keccak_parts(&[b"PosternPerps stub state v0", &self.serialize()])
    }
    fn height(&self) -> u64 {
        self.height
    }
    fn time(&self) -> u64 {
        self.time
    }
    fn snapshot(&self) -> Vec<u8> {
        self.serialize()
    }
    fn restore(&mut self, bytes: &[u8]) -> Result<(), String> {
        self.deserialize(bytes)
    }
    fn next_deposit_index(&self) -> u64 {
        self.next_deposit
    }
    fn next_forced_index(&self) -> u64 {
        self.next_forced
    }
    fn last_oracle_round(&self) -> u64 {
        self.oracle_round
    }
    fn authority(&self, account: &Address, signer: &Address, primary: &str, time: u64) -> Result<(), AuthorityError> {
        if account == signer {
            return Ok(());
        }
        match self.agents.get(&(*account, *signer)) {
            Some((exp, _)) if *exp > time => {
                if self.g.types.agent_allowed.iter().any(|a| a == primary) {
                    Ok(())
                } else {
                    Err(AuthorityError::AgentNotAllowed)
                }
            }
            _ => Err(AuthorityError::UnknownSigner),
        }
    }
    fn highest_nonce(&self, signer: &Address) -> Option<u64> {
        self.nonces.get(signer).copied()
    }
    fn withdrawal_count(&self) -> u64 {
        self.withdrawals.len() as u64
    }
    fn withdrawals_root(&self) -> H256 {
        merkle_root(&self.leaves)
    }
    fn withdrawal(&self, id: u64) -> Option<WithdrawalRecord> {
        self.withdrawals.get(id as usize).cloned()
    }
    fn withdrawals_root_at(&self, count: u64) -> Option<H256> {
        if count as usize > self.leaves.len() {
            return None;
        }
        Some(merkle_root(&self.leaves[..count as usize]))
    }
    fn withdrawal_proof(&self, id: u64, count: u64) -> Option<Vec<H256>> {
        if count as usize > self.leaves.len() {
            return None;
        }
        merkle_proof(&self.leaves[..count as usize], id as usize)
    }
    fn account_json(&self, a: &Address) -> serde_json::Value {
        let c = self.collateral.get(a).copied().unwrap_or(0);
        let cs = scaled_to_decimal(c, 6);
        serde_json::json!({
            "address": to_hex(a), "height": self.height, "time": self.time,
            "collateral": cs, "unrealizedPnl": "0.000000", "equity": cs,
            "initialMargin": "0.000000", "maintenanceMargin": "0.000000",
            "freeCollateral": cs, "withdrawable": cs, "marginRatio": "0.000000",
            "feeTier": {"tier": 0, "makerFeeBps": "1.5", "takerFeeBps": "4.5", "volume30d": "0.00"},
            "positions": [], "openOrders": self.orders.values().filter(|o| &o.account == a).count(),
            "engine": "node-stub (not the exchange engine)"
        })
    }
    fn orders_json(&self, a: &Address) -> serde_json::Value {
        let v: Vec<_> = self
            .orders
            .iter()
            .filter(|(_, o)| &o.account == a)
            .map(|(id, o)| {
                let m = self.g.market(o.market_id);
                let (pd, sd, sym) = m.map(|m| (m.price_decimals, m.size_decimals, m.symbol.clone())).unwrap_or((0, 0, "?".into()));
                serde_json::json!({"orderId": id.to_string(), "account": to_hex(&o.account), "symbol": sym,
                    "side": if o.is_buy {"buy"} else {"sell"}, "price": scaled_to_decimal(o.price as u128, pd),
                    "size": scaled_to_decimal(o.size as u128, sd), "filledSize": scaled_to_decimal(0, sd),
                    "reduceOnly": o.reduce_only, "status": "open", "height": o.height})
            })
            .collect();
        serde_json::json!({"orders": v})
    }
    fn agents_json(&self, a: &Address) -> serde_json::Value {
        let v: Vec<_> = self
            .agents
            .range((*a, [0u8; 20])..=(*a, [0xffu8; 20]))
            .map(|((_, ag), (e, t))| serde_json::json!({"agent": to_hex(ag), "approvedAt": t, "expiresAt": e, "active": *e > self.time}))
            .collect();
        serde_json::json!({"agents": v})
    }
    fn name(&self) -> &'static str {
        "node-stub"
    }
    fn clone_box(&self) -> Box<dyn Engine> {
        Box::new(self.clone())
    }
}

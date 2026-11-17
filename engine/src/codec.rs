//! Canonical byte encoding of inputs (SPEC 3.2). Decoding is strict: unknown tags, bad lengths, non-0/1 booleans,
//! invalid UTF-8 and trailing bytes are refused (S-0.4).
use crate::hash::{keccak, keccak_parts, Addr, H256};
use crate::types::*;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DecodeError(pub &'static str);

pub struct Reader<'a> {
    b: &'a [u8],
    pos: usize,
}

impl<'a> Reader<'a> {
    pub fn new(b: &'a [u8]) -> Self {
        Reader { b, pos: 0 }
    }
    pub fn take(&mut self, n: usize) -> Result<&'a [u8], DecodeError> {
        if self.b.len() - self.pos < n {
            return Err(DecodeError("short"));
        }
        let s = &self.b[self.pos..self.pos + n];
        self.pos += n;
        Ok(s)
    }
    pub fn u8(&mut self) -> Result<u8, DecodeError> {
        Ok(self.take(1)?[0])
    }
    pub fn bool(&mut self) -> Result<bool, DecodeError> {
        match self.u8()? {
            0 => Ok(false),
            1 => Ok(true),
            _ => Err(DecodeError("bool")),
        }
    }
    pub fn u16(&mut self) -> Result<u16, DecodeError> {
        Ok(u16::from_be_bytes(self.take(2)?.try_into().unwrap()))
    }
    pub fn u32(&mut self) -> Result<u32, DecodeError> {
        Ok(u32::from_be_bytes(self.take(4)?.try_into().unwrap()))
    }
    pub fn u64(&mut self) -> Result<u64, DecodeError> {
        Ok(u64::from_be_bytes(self.take(8)?.try_into().unwrap()))
    }
    pub fn addr(&mut self) -> Result<Addr, DecodeError> {
        Ok(self.take(20)?.try_into().unwrap())
    }
    pub fn b32(&mut self) -> Result<H256, DecodeError> {
        Ok(self.take(32)?.try_into().unwrap())
    }
    pub fn sig(&mut self) -> Result<[u8; 65], DecodeError> {
        Ok(self.take(65)?.try_into().unwrap())
    }
    pub fn done(&self) -> Result<(), DecodeError> {
        if self.pos == self.b.len() {
            Ok(())
        } else {
            Err(DecodeError("trailing bytes"))
        }
    }
}

fn enc_action(a: &Action, out: &mut Vec<u8>) {
    out.push(a.type_id());
    match a {
        Action::PlaceOrder(p) => {
            out.extend_from_slice(&p.account);
            out.extend_from_slice(&p.nonce.to_be_bytes());
            out.extend_from_slice(&p.market_id.to_be_bytes());
            out.push(p.is_buy as u8);
            out.push(p.order_type);
            out.push(p.time_in_force);
            out.extend_from_slice(&p.price.to_be_bytes());
            out.extend_from_slice(&p.size.to_be_bytes());
            out.push(p.reduce_only as u8);
            out.extend_from_slice(&p.trigger_price.to_be_bytes());
            out.extend_from_slice(&p.client_order_id);
        }
        Action::CancelOrder { account, nonce, market_id, order_id } => {
            out.extend_from_slice(account);
            out.extend_from_slice(&nonce.to_be_bytes());
            out.extend_from_slice(&market_id.to_be_bytes());
            out.extend_from_slice(&order_id.to_be_bytes());
        }
        Action::CancelAll { account, nonce, market_id } => {
            out.extend_from_slice(account);
            out.extend_from_slice(&nonce.to_be_bytes());
            out.extend_from_slice(&market_id.to_be_bytes());
        }
        Action::UpdateLeverage { account, nonce, market_id, leverage, isolated } => {
            out.extend_from_slice(account);
            out.extend_from_slice(&nonce.to_be_bytes());
            out.extend_from_slice(&market_id.to_be_bytes());
            out.extend_from_slice(&leverage.to_be_bytes());
            out.push(*isolated as u8);
        }
        Action::UpdateMargin { account, nonce, market_id, amount, add } => {
            out.extend_from_slice(account);
            out.extend_from_slice(&nonce.to_be_bytes());
            out.extend_from_slice(&market_id.to_be_bytes());
            out.extend_from_slice(&amount.to_be_bytes());
            out.push(*add as u8);
        }
        Action::Withdraw { account, nonce, amount, recipient } => {
            out.extend_from_slice(account);
            out.extend_from_slice(&nonce.to_be_bytes());
            out.extend_from_slice(&amount.to_be_bytes());
            out.extend_from_slice(recipient);
        }
        Action::ApproveAgent { account, nonce, agent, name, expires_at } => {
            out.extend_from_slice(account);
            out.extend_from_slice(&nonce.to_be_bytes());
            out.extend_from_slice(agent);
            out.extend_from_slice(&(name.len() as u16).to_be_bytes());
            out.extend_from_slice(name.as_bytes());
            out.extend_from_slice(&expires_at.to_be_bytes());
        }
        Action::RevokeAgent { account, nonce, agent } => {
            out.extend_from_slice(account);
            out.extend_from_slice(&nonce.to_be_bytes());
            out.extend_from_slice(agent);
        }
    }
}

fn dec_action(r: &mut Reader) -> Result<Action, DecodeError> {
    let t = r.u8()?;
    Ok(match t {
        1 => Action::PlaceOrder(PlaceOrder {
            account: r.addr()?,
            nonce: r.u64()?,
            market_id: r.u32()?,
            is_buy: r.bool()?,
            order_type: r.u8()?,
            time_in_force: r.u8()?,
            price: r.u64()?,
            size: r.u64()?,
            reduce_only: r.bool()?,
            trigger_price: r.u64()?,
            client_order_id: r.b32()?,
        }),
        2 => Action::CancelOrder { account: r.addr()?, nonce: r.u64()?, market_id: r.u32()?, order_id: r.u64()? },
        3 => Action::CancelAll { account: r.addr()?, nonce: r.u64()?, market_id: r.u32()? },
        4 => Action::UpdateLeverage {
            account: r.addr()?,
            nonce: r.u64()?,
            market_id: r.u32()?,
            leverage: r.u32()?,
            isolated: r.bool()?,
        },
        5 => Action::UpdateMargin {
            account: r.addr()?,
            nonce: r.u64()?,
            market_id: r.u32()?,
            amount: r.u64()?,
            add: r.bool()?,
        },
        6 => Action::Withdraw { account: r.addr()?, nonce: r.u64()?, amount: r.u64()?, recipient: r.addr()? },
        7 => {
            let account = r.addr()?;
            let nonce = r.u64()?;
            let agent = r.addr()?;
            let len = r.u16()? as usize;
            if len > 32 {
                return Err(DecodeError("agent name > 32 bytes"));
            }
            let name = std::str::from_utf8(r.take(len)?).map_err(|_| DecodeError("utf8"))?.to_string();
            let expires_at = r.u64()?;
            Action::ApproveAgent { account, nonce, agent, name, expires_at }
        }
        8 => Action::RevokeAgent { account: r.addr()?, nonce: r.u64()?, agent: r.addr()? },
        _ => return Err(DecodeError("action type")),
    })
}

pub fn encode_input(i: &Input) -> Vec<u8> {
    let mut p = Vec::new();
    match i {
        Input::Oracle(u) => {
            p.extend_from_slice(&u.round.to_be_bytes());
            p.push(u.observations.len() as u8);
            for o in &u.observations {
                p.push(o.signer);
                p.extend_from_slice(&o.time_ms.to_be_bytes());
                p.extend_from_slice(&(o.prices.len() as u16).to_be_bytes());
                for x in &o.prices {
                    p.extend_from_slice(&x.market_id.to_be_bytes());
                    p.extend_from_slice(&x.price.to_be_bytes());
                    p.push(x.sources);
                }
                p.extend_from_slice(&o.signature);
            }
        }
        Input::Deposit { index, account, amount } => {
            p.extend_from_slice(&index.to_be_bytes());
            p.extend_from_slice(account);
            p.extend_from_slice(&amount.to_be_bytes());
        }
        Input::Forced { index, account, kind, market_id, amount, deadline } => {
            p.extend_from_slice(&index.to_be_bytes());
            p.extend_from_slice(account);
            p.push(*kind);
            p.extend_from_slice(&market_id.to_be_bytes());
            p.extend_from_slice(&amount.to_be_bytes());
            p.extend_from_slice(&deadline.to_be_bytes());
        }
        Input::Action(sa) => {
            enc_action(&sa.action, &mut p);
            p.extend_from_slice(&sa.signature);
        }
    }
    let mut out = Vec::with_capacity(p.len() + 5);
    out.push(i.tag());
    out.extend_from_slice(&(p.len() as u32).to_be_bytes());
    out.extend_from_slice(&p);
    out
}

pub fn decode_payload(tag: u8, payload: &[u8]) -> Result<Input, DecodeError> {
    let mut r = Reader::new(payload);
    let i = match tag {
        0x01 => {
            let round = r.u64()?;
            let count = r.u8()? as usize;
            let mut observations = Vec::with_capacity(count);
            for _ in 0..count {
                let signer = r.u8()?;
                let time_ms = r.u64()?;
                let pc = r.u16()? as usize;
                let mut prices = Vec::with_capacity(pc);
                for _ in 0..pc {
                    prices.push(OraclePrice { market_id: r.u32()?, price: r.u64()?, sources: r.u8()? });
                }
                let signature = r.sig()?;
                observations.push(SignedObservation { signer, time_ms, prices, signature });
            }
            Input::Oracle(OracleUpdate { round, observations })
        }
        0x02 => Input::Deposit { index: r.u64()?, account: r.addr()?, amount: r.u64()? },
        0x03 => Input::Forced { index: r.u64()?, account: r.addr()?, kind: r.u8()?, market_id: r.u32()?, amount: r.u64()?, deadline: r.u64()? },
        0x04 => {
            let action = dec_action(&mut r)?;
            let signature = r.sig()?;
            Input::Action(SignedAction { action, signature })
        }
        _ => return Err(DecodeError("input tag")),
    };
    r.done()?;
    Ok(i)
}

pub fn encode_inputs(inputs: &[Input]) -> Vec<u8> {
    let mut out = Vec::new();
    out.extend_from_slice(&(inputs.len() as u32).to_be_bytes());
    for i in inputs {
        out.extend_from_slice(&encode_input(i));
    }
    out
}

pub fn decode_inputs(b: &[u8]) -> Result<Vec<Input>, DecodeError> {
    let mut r = Reader::new(b);
    let n = r.u32()? as usize;
    if n > crate::engine::MAX_INPUTS {
        return Err(DecodeError("too many inputs"));
    }
    let mut v = Vec::with_capacity(n);
    for _ in 0..n {
        let tag = r.u8()?;
        let len = r.u32()? as usize;
        let payload = r.take(len)?;
        v.push(decode_payload(tag, payload)?);
    }
    r.done()?;
    // Re-encoding must reproduce the bytes (canonical form).
    if encode_inputs(&v) != b {
        return Err(DecodeError("non-canonical"));
    }
    Ok(v)
}

/// SPEC 3.1: logHash(h) = keccak256(logHash(h-1) || be64(h) || be64(time) || keccak256(inputsEncoded)).
pub fn log_hash(prev: &H256, height: u64, time: u64, inputs_encoded: &[u8]) -> H256 {
    keccak_parts(&[prev, &height.to_be_bytes(), &time.to_be_bytes(), &keccak(inputs_encoded)])
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn roundtrip_and_strict() {
        let inputs = vec![
            Input::Deposit { index: 0, account: [1; 20], amount: 5 },
            Input::Action(SignedAction {
                action: Action::ApproveAgent {
                    account: [1; 20],
                    nonce: 9,
                    agent: [2; 20],
                    name: "bot".into(),
                    expires_at: 77,
                },
                signature: [3; 65],
            }),
        ];
        let b = encode_inputs(&inputs);
        assert_eq!(decode_inputs(&b).unwrap(), inputs);
        let mut t = b.clone();
        t.push(0);
        assert!(decode_inputs(&t).is_err(), "trailing byte refused");
        let mut bad = b.clone();
        bad[4] = 0x09;
        assert!(decode_inputs(&bad).is_err(), "unknown tag refused");
    }
    #[test]
    fn bool_strict() {
        let i = Input::Action(SignedAction {
            action: Action::UpdateLeverage { account: [1; 20], nonce: 1, market_id: 1, leverage: 5, isolated: false },
            signature: [0; 65],
        });
        let mut b = encode_inputs(&[i]);
        // position of the bool: 4 (count) + 1 tag + 4 len + 1 type + 20 + 8 + 4 + 4
        let pos = 4 + 1 + 4 + 1 + 20 + 8 + 4 + 4;
        b[pos] = 2;
        assert!(decode_inputs(&b).is_err());
    }
}

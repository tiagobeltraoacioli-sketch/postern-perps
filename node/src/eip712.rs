//! EIP-712 typed data: type registry, `encodeType`, `hashStruct`, domain separator, digest, plus the canonical
//! byte encoding of a typed value used inside the input log (`inputsEncoded`).
//!
//! Supported member types: `uint8..uint256` (values limited to u128), `int8..int128`, `bool`, `address`,
//! `bytes32`, `string`, `bytes`, struct types of the registry and dynamic arrays `T[]` of any of these.
//! Anything else is refused when the registry is built, so a typed-data file cannot smuggle in a type whose
//! hashing this code would get wrong.
//!
//! UNAUDITED PROTOTYPE.

use crate::util::{keccak, keccak_parts, parse_address, parse_hex_fixed, parse_hex_vec, word_address, word_i128, word_u128, Address, H256};
use std::collections::{BTreeMap, BTreeSet};

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Ty {
    Uint(u16),
    Int(u16),
    Bool,
    Address,
    Bytes32,
    String,
    Bytes,
    Struct(String),
    Array(Box<Ty>),
}

impl Ty {
    pub fn parse(s: &str, known: &BTreeSet<String>) -> Result<Ty, String> {
        if let Some(inner) = s.strip_suffix("[]") {
            return Ok(Ty::Array(Box::new(Ty::parse(inner, known)?)));
        }
        if s.contains('[') {
            return Err(format!("fixed-size arrays are not supported: {s}"));
        }
        let bits = |p: &str| -> Result<u16, String> {
            let n: u16 = p.parse().map_err(|_| format!("bad integer width in {s}"))?;
            if n == 0 || n > 256 || n % 8 != 0 {
                return Err(format!("bad integer width in {s}"));
            }
            Ok(n)
        };
        Ok(match s {
            "bool" => Ty::Bool,
            "address" => Ty::Address,
            "bytes32" => Ty::Bytes32,
            "string" => Ty::String,
            "bytes" => Ty::Bytes,
            _ if s.starts_with("uint") => Ty::Uint(bits(&s[4..])?),
            _ if s.starts_with("int") => {
                let n = bits(&s[3..])?;
                if n > 128 {
                    return Err(format!("signed integers above 128 bits are not supported: {s}"));
                }
                Ty::Int(n)
            }
            _ if known.contains(s) => Ty::Struct(s.to_string()),
            _ => return Err(format!("unknown or unsupported EIP-712 type {s:?}")),
        })
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Val {
    Uint(u128),
    Int(i128),
    Bool(bool),
    Address(Address),
    Bytes32(H256),
    Str(String),
    Bytes(Vec<u8>),
    Struct(Vec<Val>),
    Array(Vec<Val>),
}

#[derive(Clone, Debug)]
pub struct Member {
    pub name: String,
    pub ty: Ty,
    pub ty_str: String,
}

/// Limits of the canonical encoding (a typed value travels in a block; it must stay small).
pub const MAX_STRING_BYTES: usize = 256;
pub const MAX_ARRAY_LEN: usize = 256;

#[derive(Clone, Debug, Default)]
pub struct Registry {
    pub types: BTreeMap<String, Vec<Member>>,
}

impl Registry {
    /// Builds the registry from `{"TypeName": [{"name":..,"type":..}, ...], ...}`.
    pub fn from_json(types: &serde_json::Value) -> Result<Registry, String> {
        let obj = types.as_object().ok_or("types must be an object")?;
        let known: BTreeSet<String> = obj.keys().cloned().collect();
        let mut out = BTreeMap::new();
        for (tname, members) in obj {
            if tname.is_empty() || !tname.chars().all(|c| c.is_ascii_alphanumeric() || c == '_') {
                return Err(format!("bad type name {tname:?}"));
            }
            let arr = members.as_array().ok_or_else(|| format!("type {tname} must be an array"))?;
            let mut ms = Vec::new();
            let mut seen = BTreeSet::new();
            for m in arr {
                let name = m.get("name").and_then(|v| v.as_str()).ok_or("member without name")?;
                let ty_str = m.get("type").and_then(|v| v.as_str()).ok_or("member without type")?;
                if !seen.insert(name.to_string()) {
                    return Err(format!("duplicate member {name} in {tname}"));
                }
                ms.push(Member { name: name.to_string(), ty: Ty::parse(ty_str, &known)?, ty_str: ty_str.to_string() });
            }
            out.insert(tname.clone(), ms);
        }
        let reg = Registry { types: out };
        // refuse recursive types (EIP-712 allows them, these messages never need them; a cycle would loop)
        for t in reg.types.keys() {
            reg.deps(t)?;
        }
        Ok(reg)
    }

    fn deps(&self, primary: &str) -> Result<BTreeSet<String>, String> {
        let mut found = BTreeSet::new();
        let mut stack = vec![(primary.to_string(), 0usize)];
        while let Some((t, depth)) = stack.pop() {
            if depth > 8 {
                return Err(format!("type nesting too deep or recursive at {t}"));
            }
            let ms = self.types.get(&t).ok_or_else(|| format!("unknown type {t}"))?;
            for m in ms {
                let mut ty = &m.ty;
                while let Ty::Array(inner) = ty {
                    ty = inner;
                }
                if let Ty::Struct(s) = ty {
                    if s == primary {
                        return Err(format!("recursive type {primary}"));
                    }
                    if found.insert(s.clone()) {
                        stack.push((s.clone(), depth + 1));
                    }
                }
            }
        }
        Ok(found)
    }

    pub fn encode_type(&self, primary: &str) -> Result<String, String> {
        let deps = self.deps(primary)?;
        let mut s = self.encode_one(primary)?;
        for d in deps {
            s.push_str(&self.encode_one(&d)?);
        }
        Ok(s)
    }

    fn encode_one(&self, t: &str) -> Result<String, String> {
        let ms = self.types.get(t).ok_or_else(|| format!("unknown type {t}"))?;
        let inner: Vec<String> = ms.iter().map(|m| format!("{} {}", m.ty_str, m.name)).collect();
        Ok(format!("{t}({})", inner.join(",")))
    }

    pub fn type_hash(&self, primary: &str) -> Result<H256, String> {
        Ok(keccak(self.encode_type(primary)?.as_bytes()))
    }

    pub fn hash_struct(&self, primary: &str, v: &Val) -> Result<H256, String> {
        let fields = match v {
            Val::Struct(f) => f,
            _ => return Err(format!("{primary}: expected a struct value")),
        };
        let ms = self.types.get(primary).ok_or_else(|| format!("unknown type {primary}"))?;
        if fields.len() != ms.len() {
            return Err(format!("{primary}: expected {} fields, got {}", ms.len(), fields.len()));
        }
        let mut buf = Vec::with_capacity(32 * (1 + ms.len()));
        buf.extend_from_slice(&self.type_hash(primary)?);
        for (m, f) in ms.iter().zip(fields) {
            buf.extend_from_slice(&self.encode_field(&m.ty, f)?);
        }
        Ok(keccak(&buf))
    }

    fn encode_field(&self, ty: &Ty, v: &Val) -> Result<H256, String> {
        check_val(ty, v)?;
        Ok(match (ty, v) {
            (Ty::Uint(_), Val::Uint(x)) => word_u128(*x),
            (Ty::Int(_), Val::Int(x)) => word_i128(*x),
            (Ty::Bool, Val::Bool(b)) => word_u128(*b as u128),
            (Ty::Address, Val::Address(a)) => word_address(a),
            (Ty::Bytes32, Val::Bytes32(b)) => *b,
            (Ty::String, Val::Str(s)) => keccak(s.as_bytes()),
            (Ty::Bytes, Val::Bytes(b)) => keccak(b),
            (Ty::Struct(name), v) => self.hash_struct(name, v)?,
            (Ty::Array(inner), Val::Array(items)) => {
                let mut buf = Vec::with_capacity(32 * items.len());
                for it in items {
                    buf.extend_from_slice(&self.encode_field(inner, it)?);
                }
                keccak(&buf)
            }
            _ => return Err("type/value mismatch".into()),
        })
    }

    // ---- canonical byte encoding (input log) ----

    pub fn encode_canonical(&self, primary: &str, v: &Val, out: &mut Vec<u8>) -> Result<(), String> {
        self.enc(&Ty::Struct(primary.to_string()), v, out)
    }

    fn enc(&self, ty: &Ty, v: &Val, out: &mut Vec<u8>) -> Result<(), String> {
        check_val(ty, v)?;
        match (ty, v) {
            (Ty::Uint(bits), Val::Uint(x)) => {
                let n = (*bits / 8) as usize;
                if n > 16 {
                    out.extend_from_slice(&vec![0u8; n - 16]);
                    out.extend_from_slice(&x.to_be_bytes());
                } else {
                    out.extend_from_slice(&x.to_be_bytes()[16 - n..]);
                }
            }
            (Ty::Int(bits), Val::Int(x)) => {
                let n = (*bits / 8) as usize;
                out.extend_from_slice(&x.to_be_bytes()[16 - n..]);
            }
            (Ty::Bool, Val::Bool(b)) => out.push(*b as u8),
            (Ty::Address, Val::Address(a)) => out.extend_from_slice(a),
            (Ty::Bytes32, Val::Bytes32(b)) => out.extend_from_slice(b),
            (Ty::String, Val::Str(s)) => {
                out.extend_from_slice(&(s.len() as u16).to_be_bytes());
                out.extend_from_slice(s.as_bytes());
            }
            (Ty::Bytes, Val::Bytes(b)) => {
                out.extend_from_slice(&(b.len() as u16).to_be_bytes());
                out.extend_from_slice(b);
            }
            (Ty::Struct(name), Val::Struct(fields)) => {
                let ms = self.types.get(name).ok_or_else(|| format!("unknown type {name}"))?;
                if ms.len() != fields.len() {
                    return Err(format!("{name}: field count"));
                }
                for (m, f) in ms.iter().zip(fields) {
                    self.enc(&m.ty, f, out)?;
                }
            }
            (Ty::Array(inner), Val::Array(items)) => {
                out.extend_from_slice(&(items.len() as u16).to_be_bytes());
                for it in items {
                    self.enc(inner, it, out)?;
                }
            }
            _ => return Err("type/value mismatch".into()),
        }
        Ok(())
    }

    pub fn decode_canonical(&self, primary: &str, r: &mut Reader) -> Result<Val, String> {
        self.dec(&Ty::Struct(primary.to_string()), r, 0)
    }

    fn dec(&self, ty: &Ty, r: &mut Reader, depth: usize) -> Result<Val, String> {
        if depth > 8 {
            return Err("nesting too deep".into());
        }
        let v = match ty {
            Ty::Uint(bits) => {
                let n = (*bits / 8) as usize;
                let b = r.take(n)?;
                if n > 16 {
                    if b[..n - 16].iter().any(|x| *x != 0) {
                        return Err("uint value above u128 (unsupported)".into());
                    }
                    Val::Uint(u128::from_be_bytes(b[n - 16..].try_into().unwrap()))
                } else {
                    let mut w = [0u8; 16];
                    w[16 - n..].copy_from_slice(b);
                    Val::Uint(u128::from_be_bytes(w))
                }
            }
            Ty::Int(bits) => {
                let n = (*bits / 8) as usize;
                let b = r.take(n)?;
                let fill = if b[0] & 0x80 != 0 { 0xff } else { 0 };
                let mut w = [fill; 16];
                w[16 - n..].copy_from_slice(b);
                Val::Int(i128::from_be_bytes(w))
            }
            Ty::Bool => match r.take(1)?[0] {
                0 => Val::Bool(false),
                1 => Val::Bool(true),
                _ => return Err("bool byte not 0 or 1".into()),
            },
            Ty::Address => Val::Address(r.take(20)?.try_into().unwrap()),
            Ty::Bytes32 => Val::Bytes32(r.take(32)?.try_into().unwrap()),
            Ty::String => {
                let n = r.u16()? as usize;
                let b = r.take(n)?;
                Val::Str(std::str::from_utf8(b).map_err(|_| "string not UTF-8")?.to_string())
            }
            Ty::Bytes => {
                let n = r.u16()? as usize;
                Val::Bytes(r.take(n)?.to_vec())
            }
            Ty::Struct(name) => {
                let ms = self.types.get(name).ok_or_else(|| format!("unknown type {name}"))?;
                let mut f = Vec::with_capacity(ms.len());
                for m in ms {
                    f.push(self.dec(&m.ty, r, depth + 1)?);
                }
                Val::Struct(f)
            }
            Ty::Array(inner) => {
                let n = r.u16()? as usize;
                if n > MAX_ARRAY_LEN {
                    return Err("array too long".into());
                }
                let mut items = Vec::with_capacity(n);
                for _ in 0..n {
                    items.push(self.dec(inner, r, depth + 1)?);
                }
                Val::Array(items)
            }
        };
        check_val(ty, &v)?;
        Ok(v)
    }

    // ---- JSON <-> typed value (wire integers as decimal strings) ----

    /// Parses a JSON message into a typed value. Integers must be decimal strings (or JSON integers below 2^53);
    /// unknown and missing members are refused.
    pub fn val_from_json(&self, primary: &str, j: &serde_json::Value) -> Result<Val, String> {
        self.from_json_ty(&Ty::Struct(primary.to_string()), j, 0)
    }

    fn from_json_ty(&self, ty: &Ty, j: &serde_json::Value, depth: usize) -> Result<Val, String> {
        if depth > 8 {
            return Err("nesting too deep".into());
        }
        let v = match ty {
            Ty::Uint(_) => Val::Uint(json_uint(j)?),
            Ty::Int(_) => Val::Int(json_int(j)?),
            Ty::Bool => Val::Bool(j.as_bool().ok_or("expected a boolean")?),
            Ty::Address => Val::Address(parse_address(j.as_str().ok_or("expected an address string")?)?),
            Ty::Bytes32 => Val::Bytes32(parse_hex_fixed::<32>(j.as_str().ok_or("expected a bytes32 string")?)?),
            Ty::String => Val::Str(j.as_str().ok_or("expected a string")?.to_string()),
            Ty::Bytes => Val::Bytes(parse_hex_vec(j.as_str().ok_or("expected a hex string")?)?),
            Ty::Struct(name) => {
                let obj = j.as_object().ok_or_else(|| format!("{name}: expected an object"))?;
                let ms = self.types.get(name).ok_or_else(|| format!("unknown type {name}"))?;
                for k in obj.keys() {
                    if !ms.iter().any(|m| &m.name == k) {
                        return Err(format!("{name}: unknown member {k:?}"));
                    }
                }
                let mut f = Vec::with_capacity(ms.len());
                for m in ms {
                    let mj = obj.get(&m.name).ok_or_else(|| format!("{name}: missing member {:?}", m.name))?;
                    f.push(self.from_json_ty(&m.ty, mj, depth + 1).map_err(|e| format!("{name}.{}: {e}", m.name))?);
                }
                Val::Struct(f)
            }
            Ty::Array(inner) => {
                let arr = j.as_array().ok_or("expected an array")?;
                if arr.len() > MAX_ARRAY_LEN {
                    return Err("array too long".into());
                }
                Val::Array(arr.iter().map(|x| self.from_json_ty(inner, x, depth + 1)).collect::<Result<_, _>>()?)
            }
        };
        check_val(ty, &v)?;
        Ok(v)
    }

    pub fn val_to_json(&self, primary: &str, v: &Val) -> serde_json::Value {
        self.to_json_ty(&Ty::Struct(primary.to_string()), v)
    }

    fn to_json_ty(&self, ty: &Ty, v: &Val) -> serde_json::Value {
        use serde_json::Value as J;
        match (ty, v) {
            (_, Val::Uint(x)) => J::String(x.to_string()),
            (_, Val::Int(x)) => J::String(x.to_string()),
            (_, Val::Bool(b)) => J::Bool(*b),
            (_, Val::Address(a)) => J::String(crate::util::to_hex(a)),
            (_, Val::Bytes32(b)) => J::String(crate::util::to_hex(b)),
            (_, Val::Str(s)) => J::String(s.clone()),
            (_, Val::Bytes(b)) => J::String(crate::util::to_hex(b)),
            (Ty::Struct(name), Val::Struct(f)) => {
                let mut m = serde_json::Map::new();
                if let Some(ms) = self.types.get(name) {
                    for (mm, fv) in ms.iter().zip(f) {
                        m.insert(mm.name.clone(), self.to_json_ty(&mm.ty, fv));
                    }
                }
                J::Object(m)
            }
            (Ty::Array(inner), Val::Array(items)) => J::Array(items.iter().map(|x| self.to_json_ty(inner, x)).collect()),
            _ => J::Null,
        }
    }

    pub fn member_index(&self, primary: &str, name: &str) -> Option<usize> {
        self.types.get(primary)?.iter().position(|m| m.name == name)
    }
}

/// Range and size check of a value against its declared type.
pub fn check_val(ty: &Ty, v: &Val) -> Result<(), String> {
    match (ty, v) {
        (Ty::Uint(bits), Val::Uint(x)) => {
            if *bits < 128 && *x >= (1u128 << bits) {
                return Err(format!("value {x} does not fit uint{bits}"));
            }
        }
        (Ty::Int(bits), Val::Int(x)) => {
            if *bits < 128 {
                let lim = 1i128 << (bits - 1);
                if *x < -lim || *x >= lim {
                    return Err(format!("value {x} does not fit int{bits}"));
                }
            }
        }
        (Ty::String, Val::Str(s)) if s.len() > MAX_STRING_BYTES => return Err("string too long".into()),
        (Ty::Bytes, Val::Bytes(b)) if b.len() > MAX_STRING_BYTES => return Err("bytes too long".into()),
        (Ty::Array(_), Val::Array(a)) if a.len() > MAX_ARRAY_LEN => return Err("array too long".into()),
        (Ty::Bool, Val::Bool(_))
        | (Ty::Address, Val::Address(_))
        | (Ty::Bytes32, Val::Bytes32(_))
        | (Ty::String, Val::Str(_))
        | (Ty::Bytes, Val::Bytes(_))
        | (Ty::Struct(_), Val::Struct(_))
        | (Ty::Array(_), Val::Array(_)) => {}
        _ => return Err("type/value mismatch".into()),
    }
    Ok(())
}

fn json_uint(j: &serde_json::Value) -> Result<u128, String> {
    match j {
        serde_json::Value::String(s) => {
            if s.is_empty() || s.len() > 39 || !s.bytes().all(|c| c.is_ascii_digit()) || (s.len() > 1 && s.starts_with('0')) {
                return Err(format!("bad unsigned integer string {s:?}"));
            }
            s.parse::<u128>().map_err(|_| format!("unsigned integer out of range {s:?}"))
        }
        serde_json::Value::Number(n) => {
            let v = n.as_u64().ok_or("expected a non-negative integer")?;
            if v > (1u64 << 53) {
                return Err("integer above 2^53 must be a string".into());
            }
            Ok(v as u128)
        }
        _ => Err("expected an unsigned integer".into()),
    }
}

fn json_int(j: &serde_json::Value) -> Result<i128, String> {
    match j {
        serde_json::Value::String(s) => {
            let (neg, body) = match s.strip_prefix('-') {
                Some(b) => (true, b),
                None => (false, s.as_str()),
            };
            let u = json_uint(&serde_json::Value::String(body.to_string()))?;
            if neg && u == 0 {
                return Err("negative zero".into());
            }
            let v = i128::try_from(u).map_err(|_| "signed integer out of range")?;
            Ok(if neg { -v } else { v })
        }
        serde_json::Value::Number(n) => {
            let v = n.as_i64().ok_or("expected an integer")?;
            if v.unsigned_abs() > (1u64 << 53) {
                return Err("integer above 2^53 must be a string".into());
            }
            Ok(v as i128)
        }
        _ => Err("expected a signed integer".into()),
    }
}

/// Byte reader for the canonical encodings: every read is bounds-checked; callers check `done()`.
pub struct Reader<'a> {
    pub buf: &'a [u8],
    pub pos: usize,
}

impl<'a> Reader<'a> {
    pub fn new(buf: &'a [u8]) -> Self {
        Reader { buf, pos: 0 }
    }
    pub fn take(&mut self, n: usize) -> Result<&'a [u8], String> {
        if self.buf.len() - self.pos < n {
            return Err("truncated encoding".into());
        }
        let s = &self.buf[self.pos..self.pos + n];
        self.pos += n;
        Ok(s)
    }
    pub fn u8(&mut self) -> Result<u8, String> {
        Ok(self.take(1)?[0])
    }
    pub fn u16(&mut self) -> Result<u16, String> {
        Ok(u16::from_be_bytes(self.take(2)?.try_into().unwrap()))
    }
    pub fn u32(&mut self) -> Result<u32, String> {
        Ok(u32::from_be_bytes(self.take(4)?.try_into().unwrap()))
    }
    pub fn u64(&mut self) -> Result<u64, String> {
        Ok(u64::from_be_bytes(self.take(8)?.try_into().unwrap()))
    }
    pub fn u128(&mut self) -> Result<u128, String> {
        Ok(u128::from_be_bytes(self.take(16)?.try_into().unwrap()))
    }
    pub fn h256(&mut self) -> Result<H256, String> {
        Ok(self.take(32)?.try_into().unwrap())
    }
    pub fn addr(&mut self) -> Result<Address, String> {
        Ok(self.take(20)?.try_into().unwrap())
    }
    pub fn done(&self) -> bool {
        self.pos == self.buf.len()
    }
    pub fn expect_done(&self) -> Result<(), String> {
        if self.done() {
            Ok(())
        } else {
            Err(format!("{} trailing bytes", self.buf.len() - self.pos))
        }
    }
}

/// An EIP-712 domain. Only the fields present are part of `EIP712Domain` (in the standard order).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Domain {
    pub name: String,
    pub version: String,
    pub chain_id: u64,
    pub verifying_contract: Option<Address>,
}

impl Domain {
    pub fn separator(&self) -> H256 {
        let ty = if self.verifying_contract.is_some() {
            "EIP712Domain(string name,string version,uint256 chainId,address verifyingContract)"
        } else {
            "EIP712Domain(string name,string version,uint256 chainId)"
        };
        let mut buf = Vec::with_capacity(160);
        buf.extend_from_slice(&keccak(ty.as_bytes()));
        buf.extend_from_slice(&keccak(self.name.as_bytes()));
        buf.extend_from_slice(&keccak(self.version.as_bytes()));
        buf.extend_from_slice(&word_u128(self.chain_id as u128));
        if let Some(vc) = &self.verifying_contract {
            buf.extend_from_slice(&word_address(vc));
        }
        keccak(&buf)
    }

    pub fn to_json(&self) -> serde_json::Value {
        let mut m = serde_json::json!({"name": self.name, "version": self.version, "chainId": self.chain_id});
        if let Some(vc) = &self.verifying_contract {
            m["verifyingContract"] = serde_json::Value::String(crate::util::to_hex(vc));
        }
        m
    }
}

/// `keccak256(0x19 0x01 || domainSeparator || structHash)`.
pub fn digest(domain_separator: &H256, struct_hash: &H256) -> H256 {
    keccak_parts(&[&[0x19, 0x01], domain_separator, struct_hash])
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::util::to_hex;

    /// The example of the EIP-712 specification (Mail / Person, with verifyingContract).
    fn mail_registry() -> Registry {
        Registry::from_json(&serde_json::json!({
            "Person": [{"name":"name","type":"string"},{"name":"wallet","type":"address"}],
            "Mail": [{"name":"from","type":"Person"},{"name":"to","type":"Person"},{"name":"contents","type":"string"}]
        }))
        .unwrap()
    }

    #[test]
    fn eip712_specification_vector() {
        let reg = mail_registry();
        assert_eq!(reg.encode_type("Mail").unwrap(), "Mail(Person from,Person to,string contents)Person(string name,address wallet)");
        let domain = Domain {
            name: "Ether Mail".into(),
            version: "1".into(),
            chain_id: 1,
            verifying_contract: Some(parse_address("0xCcCCccccCCCCcCCCCCCcCcCccCcCCCcCcccccccC").unwrap()),
        };
        assert_eq!(to_hex(&domain.separator()), "0xf2cee375fa42b42143804025fc449deafd50cc031ca257e0b194a650a912090f");
        let msg = reg
            .val_from_json(
                "Mail",
                &serde_json::json!({
                    "from": {"name": "Cow", "wallet": "0xCD2a3d9F938E13CD947Ec05AbC7FE734Df8DD826"},
                    "to": {"name": "Bob", "wallet": "0xbBbBBBBbbBBBbbbBbbBbbbbBBbBbbbbBbBbbBBbB"},
                    "contents": "Hello, Bob!"
                }),
            )
            .unwrap();
        let sh = reg.hash_struct("Mail", &msg).unwrap();
        assert_eq!(to_hex(&sh), "0xc52c0ee5d84264471806290a3f2c4cecfc5490626bf912d01f240d7a274b371e");
        assert_eq!(to_hex(&digest(&domain.separator(), &sh)), "0xbe609aee343fb3c4b28e1df9e632fca64fcfaede20f02e86244efddf30957bd2");
    }

    #[test]
    fn canonical_roundtrip_and_strictness() {
        let reg = Registry::from_json(&serde_json::json!({
            "P": [{"name":"marketId","type":"uint32"},{"name":"price","type":"uint64"},{"name":"src","type":"uint8"}],
            "O": [{"name":"round","type":"uint64"},{"name":"d","type":"int64"},{"name":"ok","type":"bool"},
                  {"name":"name","type":"string"},{"name":"prices","type":"P[]"}]
        }))
        .unwrap();
        assert_eq!(reg.encode_type("O").unwrap(), "O(uint64 round,int64 d,bool ok,string name,P[] prices)P(uint32 marketId,uint64 price,uint8 src)");
        let v = reg
            .val_from_json("O", &serde_json::json!({"round":"7","d":"-5","ok":true,"name":"x",
                "prices":[{"marketId":1,"price":"649924","src":7},{"marketId":2,"price":"1","src":3}]}))
            .unwrap();
        let mut buf = Vec::new();
        reg.encode_canonical("O", &v, &mut buf).unwrap();
        let mut r = Reader::new(&buf);
        assert_eq!(reg.decode_canonical("O", &mut r).unwrap(), v);
        r.expect_done().unwrap();
        // a bool byte other than 0/1 is refused
        let bool_pos = 8 + 8;
        let mut bad = buf.clone();
        bad[bool_pos] = 2;
        assert!(reg.decode_canonical("O", &mut Reader::new(&bad)).is_err());
        // truncation is refused
        assert!(reg.decode_canonical("O", &mut Reader::new(&buf[..buf.len() - 1])).is_err());
        // out-of-range, unknown and missing members are refused
        assert!(reg.val_from_json("P", &serde_json::json!({"marketId":1,"price":"1","src":256})).is_err());
        assert!(reg.val_from_json("P", &serde_json::json!({"marketId":1,"price":"1","src":1,"x":1})).is_err());
        assert!(reg.val_from_json("P", &serde_json::json!({"marketId":1,"price":"1"})).is_err());
        assert!(reg.val_from_json("P", &serde_json::json!({"marketId":1,"price":"01","src":1})).is_err());
        assert!(reg.val_from_json("P", &serde_json::json!({"marketId":1.5,"price":"1","src":1})).is_err());
    }

    #[test]
    fn unsupported_and_recursive_types_refused() {
        assert!(Registry::from_json(&serde_json::json!({"A":[{"name":"x","type":"uint7"}]})).is_err());
        assert!(Registry::from_json(&serde_json::json!({"A":[{"name":"x","type":"bytes31"}]})).is_err());
        assert!(Registry::from_json(&serde_json::json!({"A":[{"name":"x","type":"uint8[3]"}]})).is_err());
        assert!(Registry::from_json(&serde_json::json!({"A":[{"name":"x","type":"B"}],"B":[{"name":"y","type":"A"}]})).is_err());
        assert!(Registry::from_json(&serde_json::json!({"A":[{"name":"x","type":"uint8"},{"name":"x","type":"uint8"}]})).is_err());
    }
}

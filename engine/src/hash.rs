//! Keccak-256 and fixed-width helpers.
use tiny_keccak::{Hasher, Keccak};

pub type H256 = [u8; 32];
pub type Addr = [u8; 20];

pub fn keccak(data: &[u8]) -> H256 {
    let mut k = Keccak::v256();
    k.update(data);
    let mut out = [0u8; 32];
    k.finalize(&mut out);
    out
}

pub fn keccak_parts(parts: &[&[u8]]) -> H256 {
    let mut k = Keccak::v256();
    for p in parts {
        k.update(p);
    }
    let mut out = [0u8; 32];
    k.finalize(&mut out);
    out
}

/// A 32-byte big-endian ABI word of an unsigned integer.
pub fn word_u(v: u128) -> H256 {
    let mut w = [0u8; 32];
    w[16..].copy_from_slice(&v.to_be_bytes());
    w
}

pub fn word_bool(b: bool) -> H256 {
    word_u(b as u128)
}

pub fn word_addr(a: &Addr) -> H256 {
    let mut w = [0u8; 32];
    w[12..].copy_from_slice(a);
    w
}

pub fn hex0x(b: &[u8]) -> String {
    format!("0x{}", hex::encode(b))
}

pub fn parse_addr(s: &str) -> Result<Addr, String> {
    let h = s.strip_prefix("0x").ok_or("address without 0x")?;
    if h.len() != 40 {
        return Err("address length".into());
    }
    let v = hex::decode(h).map_err(|e| e.to_string())?;
    let mut a = [0u8; 20];
    a.copy_from_slice(&v);
    Ok(a)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn keccak_empty() {
        assert_eq!(
            hex::encode(keccak(b"")),
            "c5d2460186f7233c927e7db2dcc703c0e500b653ca82273b7bfad8045d85a470"
        );
    }
}

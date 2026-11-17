//! Small shared helpers: Keccak-256, hex, fixed-width big-endian encoding, decimal strings.
//!
//! UNAUDITED PROTOTYPE.

use sha3::{Digest, Keccak256};

pub type Address = [u8; 20];
pub type H256 = [u8; 32];

pub const ZERO_H256: H256 = [0u8; 32];

pub fn keccak(data: &[u8]) -> H256 {
    let mut h = Keccak256::new();
    h.update(data);
    h.finalize().into()
}

pub fn keccak_parts(parts: &[&[u8]]) -> H256 {
    let mut h = Keccak256::new();
    for p in parts {
        h.update(p);
    }
    h.finalize().into()
}

pub fn to_hex(b: &[u8]) -> String {
    let mut s = String::with_capacity(2 + b.len() * 2);
    s.push_str("0x");
    s.push_str(&hex::encode(b));
    s
}

/// Parses `0x`-prefixed hex of exactly `N` bytes (any case). Anything else is refused.
pub fn parse_hex_fixed<const N: usize>(s: &str) -> Result<[u8; N], String> {
    let body = s
        .strip_prefix("0x")
        .or_else(|| s.strip_prefix("0X"))
        .ok_or_else(|| format!("expected 0x-prefixed hex, got {s:?}"))?;
    if body.len() != N * 2 {
        return Err(format!("expected {N} bytes of hex, got {} characters", body.len()));
    }
    let mut out = [0u8; N];
    hex::decode_to_slice(body, &mut out).map_err(|e| format!("bad hex: {e}"))?;
    Ok(out)
}

pub fn parse_hex_vec(s: &str) -> Result<Vec<u8>, String> {
    let body = s
        .strip_prefix("0x")
        .or_else(|| s.strip_prefix("0X"))
        .ok_or_else(|| format!("expected 0x-prefixed hex, got {s:?}"))?;
    hex::decode(body).map_err(|e| format!("bad hex: {e}"))
}

pub fn parse_address(s: &str) -> Result<Address, String> {
    parse_hex_fixed::<20>(s)
}

/// 32-byte big-endian word of an unsigned integer (ABI / EIP-712 encoding).
pub fn word_u128(v: u128) -> H256 {
    let mut w = [0u8; 32];
    w[16..].copy_from_slice(&v.to_be_bytes());
    w
}

/// 32-byte two's-complement word of a signed integer.
pub fn word_i128(v: i128) -> H256 {
    let mut w = if v < 0 { [0xffu8; 32] } else { [0u8; 32] };
    w[16..].copy_from_slice(&v.to_be_bytes());
    w
}

pub fn word_address(a: &Address) -> H256 {
    let mut w = [0u8; 32];
    w[12..].copy_from_slice(a);
    w
}

/// Converts a human decimal string into an integer scaled by 10^decimals, EXACTLY.
/// A value with more fractional digits than `decimals` is refused (never rounded), as is a sign,
/// an exponent, whitespace, an empty part or an overflow of `max`.
pub fn decimal_to_scaled(s: &str, decimals: u32, max: u128) -> Result<u128, String> {
    if s.is_empty() || s.len() > 60 {
        return Err("empty or too long decimal".into());
    }
    let (int_part, frac_part) = match s.split_once('.') {
        Some((i, f)) => {
            if f.is_empty() {
                return Err(format!("decimal {s:?} ends with '.'"));
            }
            (i, f)
        }
        None => (s, ""),
    };
    if int_part.is_empty() {
        return Err(format!("decimal {s:?} has no integer part"));
    }
    if !int_part.bytes().all(|c| c.is_ascii_digit()) || !frac_part.bytes().all(|c| c.is_ascii_digit()) {
        return Err(format!("decimal {s:?} has a non-digit character"));
    }
    if int_part.len() > 1 && int_part.starts_with('0') {
        return Err(format!("decimal {s:?} has a leading zero"));
    }
    let frac_trimmed = frac_part.trim_end_matches('0');
    if frac_trimmed.len() as u32 > decimals {
        return Err(format!("decimal {s:?} has more than {decimals} fractional digits (never rounded)"));
    }
    let mut v: u128 = 0;
    for c in int_part.bytes() {
        v = v.checked_mul(10).and_then(|x| x.checked_add((c - b'0') as u128)).ok_or("decimal overflow")?;
    }
    for i in 0..decimals as usize {
        let d = frac_trimmed.as_bytes().get(i).map(|c| (c - b'0') as u128).unwrap_or(0);
        v = v.checked_mul(10).and_then(|x| x.checked_add(d)).ok_or("decimal overflow")?;
    }
    if v > max {
        return Err(format!("decimal {s:?} above the maximum {max}"));
    }
    Ok(v)
}

/// Renders a scaled integer as a decimal string with exactly `decimals` fractional digits.
pub fn scaled_to_decimal(v: u128, decimals: u32) -> String {
    if decimals == 0 {
        return v.to_string();
    }
    let p = 10u128.pow(decimals);
    format!("{}.{:0width$}", v / p, v % p, width = decimals as usize)
}

pub fn scaled_to_decimal_signed(v: i128, decimals: u32) -> String {
    if v < 0 {
        format!("-{}", scaled_to_decimal(v.unsigned_abs(), decimals))
    } else {
        scaled_to_decimal(v as u128, decimals)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keccak_empty_vector() {
        assert_eq!(
            to_hex(&keccak(b"")),
            "0xc5d2460186f7233c927e7db2dcc703c0e500b653ca82273b7bfad8045d85a470"
        );
    }

    #[test]
    fn decimal_exact_or_refused() {
        assert_eq!(decimal_to_scaled("67250.5", 1, u128::MAX).unwrap(), 672505);
        assert_eq!(decimal_to_scaled("67250.50", 1, u128::MAX).unwrap(), 672505);
        assert_eq!(decimal_to_scaled("0.0001", 4, u128::MAX).unwrap(), 1);
        assert_eq!(decimal_to_scaled("12", 6, u128::MAX).unwrap(), 12_000_000);
        for bad in ["", ".5", "5.", "-1", "+1", "1e3", " 1", "01", "0.00001", "1,5", "0x10"] {
            assert!(decimal_to_scaled(bad, 4, u128::MAX).is_err(), "{bad:?} must be refused");
        }
        assert!(decimal_to_scaled("100", 0, 99).is_err());
        assert!(decimal_to_scaled("340282366920938463463374607431768211456", 0, u128::MAX).is_err());
        assert_eq!(scaled_to_decimal(672505, 1), "67250.5");
        assert_eq!(scaled_to_decimal(1, 6), "0.000001");
        assert_eq!(scaled_to_decimal_signed(-1_500_000, 6), "-1.500000");
    }

    #[test]
    fn hex_fixed_is_strict() {
        assert!(parse_address("0x00000000000000000000000000000000000000").is_err());
        assert!(parse_address("00000000000000000000000000000000000000ab").is_err());
        assert!(parse_address("0x000000000000000000000000000000000000dEaD").is_ok());
        assert!(parse_address("0x000000000000000000000000000000000000dEaG").is_err());
    }

    #[test]
    fn signed_word_twos_complement() {
        let w = word_i128(-1);
        assert!(w.iter().all(|b| *b == 0xff));
        assert_eq!(word_i128(5), word_u128(5));
    }
}

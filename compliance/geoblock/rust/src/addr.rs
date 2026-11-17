//! EVM address normalisation for screening (port of normalizeEvmAddress in ../src/keccak.mjs).

use sha3::{Digest, Keccak256};

pub fn keccak256(data: &[u8]) -> [u8; 32] {
    Keccak256::digest(data).into()
}

pub fn to_checksum(lower40: &str) -> String {
    let h = keccak256(lower40.as_bytes());
    let mut out = String::from("0x");
    for (i, c) in lower40.chars().enumerate() {
        let nib = (h[i / 2] >> (if i % 2 == 0 { 4 } else { 0 })) & 0xf;
        out.push(if nib >= 8 { c.to_ascii_uppercase() } else { c });
    }
    out
}

/// Lowercase 0x + 40 hex, or None when not a 20-byte hex address or mixed case with a wrong EIP-55 checksum.
pub fn normalize_evm_address(a: &str) -> Option<String> {
    let b = a.as_bytes();
    if b.len() != 42 || &b[..2] != b"0x" || !b[2..].iter().all(|c| c.is_ascii_hexdigit()) {
        return None;
    }
    let body = &a[2..];
    let lower = body.to_ascii_lowercase();
    let all_lower = body == lower;
    let all_upper = body == body.to_ascii_uppercase();
    if !all_lower && !all_upper && to_checksum(&lower) != a {
        return None;
    }
    Some(format!("0x{lower}"))
}

#[cfg(test)]
mod t {
    use super::*;
    #[test]
    fn eip55() {
        for a in ["0x5aAeb6053F3E94C9b9A09f33669435E7Ef1BeAed", "0xfB6916095ca1df60bB79Ce92cE3Ea74c37c5d359",
            "0xdbF03B407c01E7cD3CBea99509d93f8DDDC8C6FB", "0xD1220A0cf47c7B9Be7A2E6BA89F429762e7b9aDb"] {
            assert_eq!(to_checksum(&a[2..].to_ascii_lowercase()), a);
            assert_eq!(normalize_evm_address(a).unwrap(), a.to_ascii_lowercase());
        }
        assert_eq!(normalize_evm_address("0x5aaeb6053F3E94C9b9A09f33669435E7Ef1BeAed"), None);
        assert_eq!(normalize_evm_address("0X5aaeb6053f3e94c9b9a09f33669435e7ef1beaed"), None);
    }
}

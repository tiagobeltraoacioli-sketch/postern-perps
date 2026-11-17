//! secp256k1 signatures in the Ethereum form, with the strict rules of the bridge vault (`_checkSignatures`):
//! exactly 65 bytes `r || s || v`, `v` in {27, 28}, `1 <= r < n`, `1 <= s <= n/2` (low-s, no malleable twin).
//! Verification is done in process (never by a subprocess exit code).
//!
//! UNAUDITED PROTOTYPE.

use crate::util::{keccak, Address, H256};
use k256::ecdsa::{RecoveryId, Signature, SigningKey, VerifyingKey};
use k256::elliptic_curve::scalar::IsHigh;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SigError {
    Length,
    BadV,
    RangeR,
    RangeS,
    HighS,
    Recover,
}

impl std::fmt::Display for SigError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let s = match self {
            SigError::Length => "signature is not 65 bytes",
            SigError::BadV => "v is not 27 or 28",
            SigError::RangeR => "r out of range",
            SigError::RangeS => "s out of range",
            SigError::HighS => "s above n/2 (high-s)",
            SigError::Recover => "public key does not recover",
        };
        f.write_str(s)
    }
}

pub fn address_of(vk: &VerifyingKey) -> Address {
    let p = vk.to_encoded_point(false);
    let h = keccak(&p.as_bytes()[1..]);
    h[12..].try_into().unwrap()
}

/// Recovers the signer address of a 32-byte digest under the strict rules above.
pub fn recover(digest: &H256, sig: &[u8]) -> Result<Address, SigError> {
    if sig.len() != 65 {
        return Err(SigError::Length);
    }
    let v = sig[64];
    if v != 27 && v != 28 {
        return Err(SigError::BadV);
    }
    let r: [u8; 32] = sig[..32].try_into().unwrap();
    let s: [u8; 32] = sig[32..64].try_into().unwrap();
    // from_scalars refuses zero and values >= n for r and s
    let signature = Signature::from_scalars(r, s).map_err(|_| {
        if r == [0u8; 32] || !scalar_in_range(&r) {
            SigError::RangeR
        } else {
            SigError::RangeS
        }
    })?;
    if bool::from(signature.s().is_high()) {
        return Err(SigError::HighS);
    }
    let rid = RecoveryId::from_byte(v - 27).ok_or(SigError::BadV)?;
    let vk = VerifyingKey::recover_from_prehash(digest, &signature, rid).map_err(|_| SigError::Recover)?;
    Ok(address_of(&vk))
}

fn scalar_in_range(b: &[u8; 32]) -> bool {
    // n = FFFFFFFF FFFFFFFF FFFFFFFF FFFFFFFE BAAEDCE6 AF48A03B BFD25E8C D0364141
    const N: [u8; 32] = [
        0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xfe, 0xba, 0xae, 0xdc, 0xe6,
        0xaf, 0x48, 0xa0, 0x3b, 0xbf, 0xd2, 0x5e, 0x8c, 0xd0, 0x36, 0x41, 0x41,
    ];
    b.as_slice() < N.as_slice() && b.iter().any(|x| *x != 0)
}

/// Signs a digest; the result is always low-s with v in {27, 28} (k256 normalises s).
pub fn sign(key: &SigningKey, digest: &H256) -> [u8; 65] {
    let (sig, rid) = key.sign_prehash_recoverable(digest).expect("signing a 32-byte prehash cannot fail");
    let (sig, rid) = match sig.normalize_s() {
        Some(n) => (n, RecoveryId::new(!rid.is_y_odd(), rid.is_x_reduced())),
        None => (sig, rid),
    };
    let mut out = [0u8; 65];
    out[..64].copy_from_slice(&sig.to_bytes());
    out[64] = 27 + rid.to_byte();
    out
}

/// A THROWAWAY key derived from a label and an index, for dev mode and tests only. Never used for anything real.
pub fn throwaway_key(label: &str, i: u64) -> SigningKey {
    let mut ctr = 0u32;
    loop {
        let seed = keccak(format!("postern-perps THROWAWAY dev key | {label} | {i} | {ctr}").as_bytes());
        if let Ok(k) = SigningKey::from_bytes((&seed).into()) {
            return k;
        }
        ctr += 1;
    }
}

pub fn key_address(k: &SigningKey) -> Address {
    address_of(k.verifying_key())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::util::{parse_hex_fixed, to_hex};
    use k256::elliptic_curve::PrimeField;

    /// EIP-712 specification example: the key keccak256("cow") signs the Mail digest with this exact signature.
    #[test]
    fn eip712_spec_signature_recovers_cow() {
        let digest = parse_hex_fixed::<32>("0xbe609aee343fb3c4b28e1df9e632fca64fcfaede20f02e86244efddf30957bd2").unwrap();
        let mut sig = [0u8; 65];
        sig[..32].copy_from_slice(&parse_hex_fixed::<32>("0x4355c47d63924e8a72e509b65029052eb6c299d53a04e167c5775fd466751c9d").unwrap());
        sig[32..64].copy_from_slice(&parse_hex_fixed::<32>("0x07299936d304c153f6443dfa05f40ff007d72911b6f72307f996231605b91562").unwrap());
        sig[64] = 28;
        assert_eq!(to_hex(&recover(&digest, &sig).unwrap()), "0xcd2a3d9f938e13cd947ec05abc7fe734df8dd826");
        let cow = SigningKey::from_bytes((&keccak(b"cow")).into()).unwrap();
        assert_eq!(to_hex(&key_address(&cow)), "0xcd2a3d9f938e13cd947ec05abc7fe734df8dd826");
        assert_eq!(sign(&cow, &digest), sig, "RFC 6979 deterministic signature equals the specification's");
    }

    #[test]
    fn strict_rules() {
        let k = throwaway_key("test", 1);
        let d = keccak(b"message");
        let s = sign(&k, &d);
        assert_eq!(recover(&d, &s).unwrap(), key_address(&k));
        assert_eq!(recover(&d, &s[..64]), Err(SigError::Length));
        let mut bad_v = s;
        bad_v[64] = 0;
        assert_eq!(recover(&d, &bad_v), Err(SigError::BadV));
        bad_v[64] = 29;
        assert_eq!(recover(&d, &bad_v), Err(SigError::BadV));
        // high-s twin: s' = n - s, v flipped; recovers the same key in a lax verifier, refused here
        let n = k256::Scalar::ZERO - k256::Scalar::ONE; // n - 1
        let s_scalar: k256::Scalar = Option::from(k256::Scalar::from_repr(<[u8; 32]>::try_from(&s[32..64]).unwrap().into())).unwrap();
        let twin = (n - s_scalar) + k256::Scalar::ONE; // n - s
        let mut hs = s;
        hs[32..64].copy_from_slice(&twin.to_bytes());
        hs[64] = if s[64] == 27 { 28 } else { 27 };
        assert_eq!(recover(&d, &hs), Err(SigError::HighS));
        let mut zr = s;
        zr[..32].fill(0);
        assert_eq!(recover(&d, &zr), Err(SigError::RangeR));
        let mut zs = s;
        zs[32..64].fill(0);
        assert_eq!(recover(&d, &zs), Err(SigError::RangeS));
        let mut big_r = s;
        big_r[..32].fill(0xff);
        assert_eq!(recover(&d, &big_r), Err(SigError::RangeR));
        // a different digest recovers a different address (never the signer)
        assert_ne!(recover(&keccak(b"other"), &s).ok(), Some(key_address(&k)));
    }
}

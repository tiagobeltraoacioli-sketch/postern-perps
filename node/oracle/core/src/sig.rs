//! 65-byte secp256k1 signatures `r || s || v` (README section 3).
//!
//! Accepted: `r` and `s` in `[1, n-1]`, `s <= n/2` (low-s, so a signature has exactly one accepted form and an
//! observation's bytes cannot be malleated by a relaying sequencer), `v` in `{27, 28}`. The signer is the Ethereum
//! address of the recovered key. Same rules as the bridge's `_checkSignatures` and 389's action verification.

use k256::ecdsa::{RecoveryId, Signature, SigningKey, VerifyingKey};

use crate::keccak256;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SigError {
    BadV,
    BadScalar,
    HighS,
    Unrecoverable,
}

pub fn address_of(vk: &VerifyingKey) -> [u8; 20] {
    let p = vk.to_encoded_point(false);
    let h = keccak256(&p.as_bytes()[1..]);
    h[12..].try_into().unwrap()
}

pub fn recover(digest: &[u8; 32], sig: &[u8; 65]) -> Result<[u8; 20], SigError> {
    let v = sig[64];
    if v != 27 && v != 28 {
        return Err(SigError::BadV);
    }
    // from_slice refuses r or s equal to 0 or >= n.
    let s = Signature::from_slice(&sig[..64]).map_err(|_| SigError::BadScalar)?;
    if s.normalize_s().is_some() {
        return Err(SigError::HighS);
    }
    let rid = RecoveryId::from_byte(v - 27).ok_or(SigError::BadV)?;
    let vk = VerifyingKey::recover_from_prehash(digest, &s, rid).map_err(|_| SigError::Unrecoverable)?;
    Ok(address_of(&vk))
}

/// Signs a digest (RFC 6979 deterministic nonce), returning the low-s form with `v` in `{27, 28}`.
pub fn sign(sk: &SigningKey, digest: &[u8; 32]) -> [u8; 65] {
    let (s, rid) = sk.sign_prehash_recoverable(digest).expect("signing a 32-byte prehash cannot fail");
    let (s, rid) = match s.normalize_s() {
        Some(low) => (low, RecoveryId::new(!rid.is_y_odd(), rid.is_x_reduced())),
        None => (s, rid),
    };
    let mut out = [0u8; 65];
    out[..64].copy_from_slice(&s.to_bytes());
    out[64] = 27 + rid.to_byte();
    out
}

/// The secp256k1 group order n, big-endian.
pub const N: [u8; 32] = [
    0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFE, 0xBA, 0xAE, 0xDC,
    0xE6, 0xAF, 0x48, 0xA0, 0x3B, 0xBF, 0xD2, 0x5E, 0x8C, 0xD0, 0x36, 0x41, 0x41,
];

/// `n - s` (big-endian 256-bit subtraction), used by tests to build the high-s twin of a signature.
pub fn neg_scalar(s: &[u8; 32]) -> [u8; 32] {
    let mut out = [0u8; 32];
    let mut borrow = 0i16;
    for i in (0..32).rev() {
        let mut d = i16::from(N[i]) - i16::from(s[i]) - borrow;
        borrow = 0;
        if d < 0 {
            d += 256;
            borrow = 1;
        }
        out[i] = d as u8;
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key(i: u8) -> SigningKey {
        SigningKey::from_bytes(&keccak256(&[b'k', i]).into()).unwrap()
    }

    #[test]
    fn sign_recover_and_refusals() {
        let sk = key(1);
        let addr = address_of(sk.verifying_key());
        for i in 0..64u8 {
            let d = keccak256(&[i]);
            let sig = sign(&sk, &d);
            assert_eq!(recover(&d, &sig), Ok(addr));
            assert!(sig[32] < 0x80, "low-s");

            // The high-s twin (n - s, flipped v) is the same ECDSA signature mathematically: refused.
            let mut twin = sig;
            let s: [u8; 32] = sig[32..64].try_into().unwrap();
            twin[32..64].copy_from_slice(&neg_scalar(&s));
            twin[64] = if sig[64] == 27 { 28 } else { 27 };
            assert_eq!(recover(&d, &twin), Err(SigError::HighS));

            // Flipped v recovers another key (or nothing): never the signer.
            let mut flip = sig;
            flip[64] = if sig[64] == 27 { 28 } else { 27 };
            assert_ne!(recover(&d, &flip), Ok(addr));

            for v in [0u8, 1, 26, 29, 35, 255] {
                let mut bad = sig;
                bad[64] = v;
                assert_eq!(recover(&d, &bad), Err(SigError::BadV));
            }
        }
    }

    #[test]
    fn zero_and_out_of_range_scalars() {
        let d = keccak256(b"x");
        let sig = sign(&key(2), &d);
        let mut z = sig;
        z[..32].fill(0);
        assert_eq!(recover(&d, &z), Err(SigError::BadScalar));
        let mut z = sig;
        z[32..64].fill(0);
        assert_eq!(recover(&d, &z), Err(SigError::BadScalar));
        let mut big = sig;
        big[..32].copy_from_slice(&N);
        assert_eq!(recover(&d, &big), Err(SigError::BadScalar));
    }
}

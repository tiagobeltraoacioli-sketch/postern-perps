//! EIP-712 hashing and secp256k1 recovery (SPEC 2.1 to 2.3, S-2.2).
use crate::hash::{keccak, keccak_parts, word_addr, word_bool, word_u, Addr, H256};
use crate::types::{Action, OraclePrice};
use k256::ecdsa::{RecoveryId, Signature, VerifyingKey};

pub const DOMAIN_NAME: &str = "PosternPerps";
pub const DOMAIN_VERSION: &str = "1";

pub const T_DOMAIN: &str = "EIP712Domain(string name,string version,uint256 chainId,address verifyingContract)";
pub const T_PLACE: &str = "PlaceOrder(address account,uint64 nonce,uint32 marketId,bool isBuy,uint8 orderType,uint8 timeInForce,uint64 price,uint64 size,bool reduceOnly,uint64 triggerPrice,bytes32 clientOrderId)";
pub const T_CANCEL: &str = "CancelOrder(address account,uint64 nonce,uint32 marketId,uint64 orderId)";
pub const T_CANCEL_ALL: &str = "CancelAll(address account,uint64 nonce,uint32 marketId)";
pub const T_LEVERAGE: &str = "UpdateLeverage(address account,uint64 nonce,uint32 marketId,uint32 leverage,bool isolated)";
pub const T_MARGIN: &str = "UpdateMargin(address account,uint64 nonce,uint32 marketId,uint64 amount,bool add)";
pub const T_WITHDRAW: &str = "Withdraw(address account,uint64 nonce,uint64 amount,address recipient)";
pub const T_APPROVE: &str = "ApproveAgent(address account,uint64 nonce,address agent,string name,uint64 expiresAt)";
pub const T_REVOKE: &str = "RevokeAgent(address account,uint64 nonce,address agent)";
pub const T_ORACLE_OBS: &str =
    "OracleObservation(uint64 round,uint64 timeMs,OraclePrice[] prices)OraclePrice(uint32 marketId,uint64 price,uint8 sources)";
pub const T_ORACLE_PRICE: &str = "OraclePrice(uint32 marketId,uint64 price,uint8 sources)";

pub fn domain_separator(chain_id: u64, verifying_contract: &Addr) -> H256 {
    keccak_parts(&[
        &keccak(T_DOMAIN.as_bytes()),
        &keccak(DOMAIN_NAME.as_bytes()),
        &keccak(DOMAIN_VERSION.as_bytes()),
        &word_u(chain_id as u128),
        &word_addr(verifying_contract),
    ])
}

pub fn struct_hash(a: &Action) -> H256 {
    let mut enc: Vec<u8> = Vec::with_capacity(32 * 12);
    let mut push = |w: H256| enc.extend_from_slice(&w);
    match a {
        Action::PlaceOrder(p) => {
            push(keccak(T_PLACE.as_bytes()));
            push(word_addr(&p.account));
            push(word_u(p.nonce as u128));
            push(word_u(p.market_id as u128));
            push(word_bool(p.is_buy));
            push(word_u(p.order_type as u128));
            push(word_u(p.time_in_force as u128));
            push(word_u(p.price as u128));
            push(word_u(p.size as u128));
            push(word_bool(p.reduce_only));
            push(word_u(p.trigger_price as u128));
            push(p.client_order_id);
        }
        Action::CancelOrder { account, nonce, market_id, order_id } => {
            push(keccak(T_CANCEL.as_bytes()));
            push(word_addr(account));
            push(word_u(*nonce as u128));
            push(word_u(*market_id as u128));
            push(word_u(*order_id as u128));
        }
        Action::CancelAll { account, nonce, market_id } => {
            push(keccak(T_CANCEL_ALL.as_bytes()));
            push(word_addr(account));
            push(word_u(*nonce as u128));
            push(word_u(*market_id as u128));
        }
        Action::UpdateLeverage { account, nonce, market_id, leverage, isolated } => {
            push(keccak(T_LEVERAGE.as_bytes()));
            push(word_addr(account));
            push(word_u(*nonce as u128));
            push(word_u(*market_id as u128));
            push(word_u(*leverage as u128));
            push(word_bool(*isolated));
        }
        Action::UpdateMargin { account, nonce, market_id, amount, add } => {
            push(keccak(T_MARGIN.as_bytes()));
            push(word_addr(account));
            push(word_u(*nonce as u128));
            push(word_u(*market_id as u128));
            push(word_u(*amount as u128));
            push(word_bool(*add));
        }
        Action::Withdraw { account, nonce, amount, recipient } => {
            push(keccak(T_WITHDRAW.as_bytes()));
            push(word_addr(account));
            push(word_u(*nonce as u128));
            push(word_u(*amount as u128));
            push(word_addr(recipient));
        }
        Action::ApproveAgent { account, nonce, agent, name, expires_at } => {
            push(keccak(T_APPROVE.as_bytes()));
            push(word_addr(account));
            push(word_u(*nonce as u128));
            push(word_addr(agent));
            push(keccak(name.as_bytes()));
            push(word_u(*expires_at as u128));
        }
        Action::RevokeAgent { account, nonce, agent } => {
            push(keccak(T_REVOKE.as_bytes()));
            push(word_addr(account));
            push(word_u(*nonce as u128));
            push(word_addr(agent));
        }
    }
    keccak(&enc)
}

pub fn oracle_struct_hash(round: u64, time_ms: u64, prices: &[OraclePrice]) -> H256 {
    let th = keccak(T_ORACLE_PRICE.as_bytes());
    let mut arr = Vec::with_capacity(prices.len() * 32);
    for p in prices {
        arr.extend_from_slice(&keccak_parts(&[
            &th,
            &word_u(p.market_id as u128),
            &word_u(p.price as u128),
            &word_u(p.sources as u128),
        ]));
    }
    keccak_parts(&[
        &keccak(T_ORACLE_OBS.as_bytes()),
        &word_u(round as u128),
        &word_u(time_ms as u128),
        &keccak(&arr),
    ])
}

pub fn digest(domain_sep: &H256, struct_hash: &H256) -> H256 {
    keccak_parts(&[&[0x19, 0x01], domain_sep, struct_hash])
}

/// S-2.2: strict recovery. 65 bytes r||s||v, v in {27,28}, 1 <= r,s < n, s <= n/2.
pub fn recover(digest: &H256, sig: &[u8; 65]) -> Option<Addr> {
    let v = sig[64];
    if v != 27 && v != 28 {
        return None;
    }
    let mut r = [0u8; 32];
    let mut s = [0u8; 32];
    r.copy_from_slice(&sig[0..32]);
    s.copy_from_slice(&sig[32..64]);
    // from_scalars rejects zero and values >= n.
    let signature = Signature::from_scalars(r, s).ok()?;
    // normalize_s returns Some when s was high: high-s is refused (no malleability).
    if signature.normalize_s().is_some() {
        return None;
    }
    let recid = RecoveryId::from_byte(v - 27)?;
    let vk = VerifyingKey::recover_from_prehash(digest, &signature, recid).ok()?;
    Some(address_of(&vk))
}

pub fn address_of(vk: &VerifyingKey) -> Addr {
    let p = vk.to_encoded_point(false);
    let h = keccak(&p.as_bytes()[1..]);
    let mut a = [0u8; 20];
    a.copy_from_slice(&h[12..]);
    a
}

/// Test and tooling helper: sign a digest with a throwaway key, low-s, v in {27,28}.
pub fn sign_digest(key: &k256::ecdsa::SigningKey, digest: &H256) -> [u8; 65] {
    let (sig, mut recid) = key.sign_prehash_recoverable(digest).expect("sign");
    let sig = match sig.normalize_s() {
        Some(n) => {
            recid = RecoveryId::new(!recid.is_y_odd(), recid.is_x_reduced());
            n
        }
        None => sig,
    };
    let mut out = [0u8; 65];
    out[..64].copy_from_slice(&sig.to_bytes());
    out[64] = 27 + recid.to_byte();
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use k256::ecdsa::SigningKey;

    fn key(n: u8) -> SigningKey {
        let mut b = [0u8; 32];
        b[31] = n;
        SigningKey::from_slice(&b).unwrap()
    }

    #[test]
    fn sign_recover_roundtrip_and_strictness() {
        let k = key(7);
        let addr = address_of(k.verifying_key());
        let d = keccak(b"x");
        let sig = sign_digest(&k, &d);
        assert_eq!(recover(&d, &sig), Some(addr));
        // wrong v
        let mut bad = sig;
        bad[64] = 0;
        assert_eq!(recover(&d, &bad), None);
        // high-s variant of the same signature is refused
        let n = hex::decode("fffffffffffffffffffffffffffffffebaaedce6af48a03bbfd25e8cd0364141").unwrap();
        let mut s = [0u8; 32];
        s.copy_from_slice(&sig[32..64]);
        let mut hs = [0u8; 32];
        let mut borrow = 0i16;
        for i in (0..32).rev() {
            let x = n[i] as i16 - s[i] as i16 - borrow;
            borrow = if x < 0 { 1 } else { 0 };
            hs[i] = (x + 256 * borrow) as u8;
        }
        let mut high = sig;
        high[32..64].copy_from_slice(&hs);
        high[64] = if sig[64] == 27 { 28 } else { 27 };
        assert_eq!(recover(&d, &high), None);
        // zero r
        let mut z = sig;
        z[..32].fill(0);
        assert_eq!(recover(&d, &z), None);
    }

    #[test]
    fn known_address_of_key_one() {
        // private key 1 -> 0x7E5F4552091A69125d5DfCb7b8C2659029395Bdf (well-known)
        assert_eq!(hex::encode(address_of(key(1).verifying_key())), "7e5f4552091a69125d5dfcb7b8c2659029395bdf");
    }
}

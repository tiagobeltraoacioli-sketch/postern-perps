//! Independent implementation (agent 395, written from docs/SPEC.md v0.1 only, without reading engine code) of:
//! the EIP-712 action digest (SPEC 2.1-2.3), the signature format (S-2.2), the canonical action and input
//! encodings (SPEC 3.2) and the log hash (SPEC 3.1). Used to generate adversarial input logs and to cross-check
//! the engine's digests. UNAUDITED review tooling.
use k256::ecdsa::{RecoveryId, Signature, SigningKey, VerifyingKey};
use sha3::{Digest, Keccak256};

pub fn keccak(data: &[u8]) -> [u8; 32] {
    let mut h = Keccak256::new();
    h.update(data);
    h.finalize().into()
}

#[derive(Clone, Debug)]
pub enum Val {
    Address([u8; 20]),
    U64(u64),
    U32(u32),
    U8(u8),
    Bool(bool),
    B32([u8; 32]),
    Str(String),
}

impl Val {
    fn sol_type(&self) -> &'static str {
        match self {
            Val::Address(_) => "address",
            Val::U64(_) => "uint64",
            Val::U32(_) => "uint32",
            Val::U8(_) => "uint8",
            Val::Bool(_) => "bool",
            Val::B32(_) => "bytes32",
            Val::Str(_) => "string",
        }
    }
    fn word(&self) -> [u8; 32] {
        let mut w = [0u8; 32];
        match self {
            Val::Address(a) => w[12..].copy_from_slice(a),
            Val::U64(v) => w[24..].copy_from_slice(&v.to_be_bytes()),
            Val::U32(v) => w[28..].copy_from_slice(&v.to_be_bytes()),
            Val::U8(v) => w[31] = *v,
            Val::Bool(b) => w[31] = *b as u8,
            Val::B32(b) => w = *b,
            Val::Str(s) => w = keccak(s.as_bytes()),
        }
        w
    }
    fn wire(&self, out: &mut Vec<u8>) {
        match self {
            Val::Address(a) => out.extend_from_slice(a),
            Val::U64(v) => out.extend_from_slice(&v.to_be_bytes()),
            Val::U32(v) => out.extend_from_slice(&v.to_be_bytes()),
            Val::U8(v) => out.push(*v),
            Val::Bool(b) => out.push(*b as u8),
            Val::B32(b) => out.extend_from_slice(b),
            Val::Str(s) => {
                out.extend_from_slice(&(s.len() as u16).to_be_bytes());
                out.extend_from_slice(s.as_bytes());
            }
        }
    }
}

/// (type name, typeId of SPEC 3.2, field names in normative order)
pub const TYPES: &[(&str, u8, &[&str])] = &[
    ("PlaceOrder", 1, &["account", "nonce", "marketId", "isBuy", "orderType", "timeInForce", "price", "size", "reduceOnly", "triggerPrice", "clientOrderId"]),
    ("CancelOrder", 2, &["account", "nonce", "marketId", "orderId"]),
    ("CancelAll", 3, &["account", "nonce", "marketId"]),
    ("UpdateLeverage", 4, &["account", "nonce", "marketId", "leverage", "isolated"]),
    ("UpdateMargin", 5, &["account", "nonce", "marketId", "amount", "add"]),
    ("Withdraw", 6, &["account", "nonce", "amount", "recipient"]),
    ("ApproveAgent", 7, &["account", "nonce", "agent", "name", "expiresAt"]),
    ("RevokeAgent", 8, &["account", "nonce", "agent"]),
];

#[derive(Clone, Debug)]
pub struct Action {
    pub ty: &'static str,
    pub fields: Vec<Val>,
}

impl Action {
    fn def(&self) -> (&'static str, u8, &'static [&'static str]) {
        *TYPES.iter().find(|t| t.0 == self.ty).expect("known type")
    }
    pub fn type_string(&self) -> String {
        let (name, _, names) = self.def();
        assert_eq!(names.len(), self.fields.len(), "field count");
        let parts: Vec<String> = names.iter().zip(&self.fields).map(|(n, v)| format!("{} {}", v.sol_type(), n)).collect();
        format!("{}({})", name, parts.join(","))
    }
    pub fn hash_struct(&self) -> [u8; 32] {
        let mut enc = keccak(self.type_string().as_bytes()).to_vec();
        for v in &self.fields {
            enc.extend_from_slice(&v.word());
        }
        keccak(&enc)
    }
    pub fn digest(&self, domain: &Domain) -> [u8; 32] {
        let mut m = vec![0x19, 0x01];
        m.extend_from_slice(&domain.separator());
        m.extend_from_slice(&self.hash_struct());
        keccak(&m)
    }
    /// SPEC 3.2 tag 0x04 payload: typeId || fields || signature
    pub fn wire_payload(&self, sig: &[u8; 65]) -> Vec<u8> {
        let mut out = vec![self.def().1];
        for v in &self.fields {
            v.wire(&mut out);
        }
        out.extend_from_slice(sig);
        out
    }
}

#[derive(Clone, Debug)]
pub struct Domain {
    pub name: String,
    pub version: String,
    pub chain_id: u64,
    pub verifying_contract: [u8; 20],
}

impl Domain {
    pub fn production_like(vault: [u8; 20]) -> Self {
        Domain { name: "PosternPerps".into(), version: "1".into(), chain_id: 700770, verifying_contract: vault }
    }
    pub fn separator(&self) -> [u8; 32] {
        let th = keccak(b"EIP712Domain(string name,string version,uint256 chainId,address verifyingContract)");
        let mut e = th.to_vec();
        e.extend_from_slice(&keccak(self.name.as_bytes()));
        e.extend_from_slice(&keccak(self.version.as_bytes()));
        let mut c = [0u8; 32];
        c[24..].copy_from_slice(&self.chain_id.to_be_bytes());
        e.extend_from_slice(&c);
        let mut a = [0u8; 32];
        a[12..].copy_from_slice(&self.verifying_contract);
        e.extend_from_slice(&a);
        keccak(&e)
    }
}

pub fn address_of(vk: &VerifyingKey) -> [u8; 20] {
    let p = vk.to_encoded_point(false);
    let h = keccak(&p.as_bytes()[1..]);
    let mut a = [0u8; 20];
    a.copy_from_slice(&h[12..]);
    a
}

/// S-2.2 form: r || s || v, low-s, v in {27, 28}.
pub fn sign(sk: &SigningKey, digest: &[u8; 32]) -> [u8; 65] {
    let (sig, rid): (Signature, RecoveryId) = sk.sign_prehash_recoverable(digest).expect("sign");
    let (sig, rid) = match sig.normalize_s() {
        Some(n) => (n, RecoveryId::new(!rid.is_y_odd(), rid.is_x_reduced())),
        None => (sig, rid),
    };
    let mut out = [0u8; 65];
    out[..64].copy_from_slice(&sig.to_bytes());
    out[64] = 27 + rid.to_byte();
    out
}

/// The high-s twin of a valid signature (same signer, must be refused by S-2.2).
pub fn high_s_twin(sig: &[u8; 65]) -> [u8; 65] {
    let s = Signature::from_slice(&sig[..64]).unwrap();
    let (r, sv) = s.split_scalars();
    let neg = -*sv;
    let twin = Signature::from_scalars(r.to_bytes(), neg.to_bytes()).unwrap();
    let mut out = [0u8; 65];
    out[..64].copy_from_slice(&twin.to_bytes());
    out[64] = if sig[64] == 27 { 28 } else { 27 };
    out
}

pub fn recover(digest: &[u8; 32], sig: &[u8; 65]) -> Option<[u8; 20]> {
    let s = Signature::from_slice(&sig[..64]).ok()?;
    let rid = RecoveryId::from_byte(sig[64].checked_sub(27)?)?;
    VerifyingKey::recover_from_prehash(digest, &s, rid).ok().map(|vk| address_of(&vk))
}

pub fn input(tag: u8, payload: &[u8]) -> Vec<u8> {
    let mut o = vec![tag];
    o.extend_from_slice(&(payload.len() as u32).to_be_bytes());
    o.extend_from_slice(payload);
    o
}

pub fn inputs_encoded(inputs: &[Vec<u8>]) -> Vec<u8> {
    let mut o = (inputs.len() as u32).to_be_bytes().to_vec();
    for i in inputs {
        o.extend_from_slice(i);
    }
    o
}

pub fn deposit_payload(index: u64, account: [u8; 20], amount: u64) -> Vec<u8> {
    let mut o = index.to_be_bytes().to_vec();
    o.extend_from_slice(&account);
    o.extend_from_slice(&amount.to_be_bytes());
    o
}

pub fn log_hash(prev: &[u8; 32], height: u64, time: u64, inputs_enc: &[u8]) -> [u8; 32] {
    let mut m = prev.to_vec();
    m.extend_from_slice(&height.to_be_bytes());
    m.extend_from_slice(&time.to_be_bytes());
    m.extend_from_slice(&keccak(inputs_enc));
    keccak(&m)
}

pub fn action_hash(digest: &[u8; 32], signer: &[u8; 20]) -> [u8; 32] {
    let mut m = digest.to_vec();
    m.extend_from_slice(signer);
    keccak(&m)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key(i: u8) -> SigningKey {
        let mut b = [0u8; 32];
        b[31] = i;
        b[0] = 0x42;
        SigningKey::from_slice(&b).unwrap()
    }

    #[test]
    fn sign_recovers_low_s_and_twin_differs() {
        let sk = key(1);
        let addr = address_of(sk.verifying_key());
        let d = keccak(b"x");
        let s = sign(&sk, &d);
        assert_eq!(recover(&d, &s), Some(addr));
        let hs = high_s_twin(&s);
        // plain ecrecover (EVM) accepts the twin; S-2.2 requires the engine to refuse it by the s bound
        assert_ne!(hs[..64], s[..64]);
        println!("k256 recover of high-s twin: {:?}", recover(&d, &hs).map(hex::encode));
    }

    #[test]
    fn domains_separate() {
        let a = Domain::production_like([1u8; 20]);
        let b = Domain::production_like([2u8; 20]);
        let mut v = a.clone();
        v.name = "PosternPerpsVault".into();
        assert_ne!(a.separator(), b.separator());
        assert_ne!(a.separator(), v.separator());
    }

    #[test]
    fn type_strings_match_spec_2_3() {
        let acct = [0u8; 20];
        let a = Action { ty: "Withdraw", fields: vec![Val::Address(acct), Val::U64(1), Val::U64(2), Val::Address(acct)] };
        assert_eq!(a.type_string(), "Withdraw(address account,uint64 nonce,uint64 amount,address recipient)");
    }
    /// Cross-check against the mock's viem-checked EIP-712 library (node/mock/lib/eip712.mjs at f008363), SPEC 0.1
    /// domain with verifyingContract 0x11..11: two independent implementations must agree byte for byte.
    #[test]
    fn digests_match_mock_library() {
        let d = Domain::production_like([0x11; 20]);
        let a = [0x22u8; 20];
        let po = Action { ty: "PlaceOrder", fields: vec![Val::Address(a), Val::U64(1794878412000), Val::U32(1), Val::Bool(true), Val::U8(1), Val::U8(1), Val::U64(6725000000000), Val::U64(50000000), Val::Bool(false), Val::U64(0), Val::B32([0; 32])] };
        assert_eq!(hex::encode(po.digest(&d)), "a5eaf474abcc78196b5e311bc8e97b3668d133a533db09d45381ef4c986707c6");
        let aa = Action { ty: "ApproveAgent", fields: vec![Val::Address(a), Val::U64(1794878412001), Val::Address([0x33; 20]), Val::Str("bot".into()), Val::U64(1794964812000)] };
        assert_eq!(hex::encode(aa.digest(&d)), "0a3d57f11c2c0ff1c9c025578bba5878447a50dbef3a6b4d5911ef9756eff38c");
    }
}

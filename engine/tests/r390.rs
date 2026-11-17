//! VAULT.md 0.2 (390, 64072cb) forced-request event: `ForcedRequest(uint64 index, address account, uint8 kind,
//! uint64 deadline, uint64 arg, bytes32 head)`, kinds 0 CancelAll, 1 CloseAll, 2 Withdraw (arg = gross amount),
//! `forcedHead(i+1) = keccak256(abi.encode(forcedHead(i), uint256(i), address account, uint256(kind),
//! uint256(deadline), uint256(arg)))`. The input is built from canonical BYTES (SPEC 3.2 tag 0x03) so this test
//! compiles against any engine version and fails on one that does not decode the v0.2 fields.
use postern_perps_engine::codec::decode_inputs;
use postern_perps_engine::hash::{hex0x, keccak, Addr, H256};
use postern_perps_engine::testkit::*;
use postern_perps_engine::*;

fn word(x: u128) -> [u8; 32] {
    let mut w = [0u8; 32];
    w[16..].copy_from_slice(&x.to_be_bytes());
    w
}
fn aword(a: &Addr) -> [u8; 32] {
    let mut w = [0u8; 32];
    w[12..].copy_from_slice(a);
    w
}
/// Independent recomputation of the vault's chain (not the engine's helpers).
fn vault_forced_head(reqs: &[(Addr, u8, u64, u64)]) -> H256 {
    let mut tag = [0u8; 32];
    tag[..22].copy_from_slice(b"PosternPerps.forced.v1");
    let mut vault = [0u8; 32];
    vault[12..].copy_from_slice(&hex::decode(&DEV_VAULT[2..]).unwrap());
    let mut h = keccak(&[tag, word(700770), vault].concat());
    for (i, (a, kind, deadline, arg)) in reqs.iter().enumerate() {
        h = keccak(&[h, word(i as u128), aword(a), word(*kind as u128), word(*deadline as u128), word(*arg as u128)].concat());
    }
    h
}
/// SPEC 3.2 tag 0x03, v0.2: be64 index || account:20 || kind:u8 || be64 deadline || be64 arg
fn forced_bytes(index: u64, a: &Addr, kind: u8, deadline: u64, arg: u64) -> Vec<u8> {
    let mut p = Vec::new();
    p.extend_from_slice(&index.to_be_bytes());
    p.extend_from_slice(a);
    p.push(kind);
    p.extend_from_slice(&deadline.to_be_bytes());
    p.extend_from_slice(&arg.to_be_bytes());
    p
}

#[test]
fn forced_requests_use_vault_0_2_fields_and_head() {
    let a = addr(&key(1));
    let mut e = Engine::new(genesis(5, vec![market(1, "BTC-USD")]));
    let reqs = [(a, 0u8, 1_794_966_400u64, 0u64), (a, 2u8, 1_794_966_401u64, 5_000_000u64)];
    let mut enc = vec![0, 0, 0, 3];
    let dep = postern_perps_engine::codec::encode_input(&Input::Deposit { index: 0, account: a, amount: 100_000_000 });
    enc.extend_from_slice(&dep);
    for (i, (acc, kind, dl, arg)) in reqs.iter().enumerate() {
        let p = forced_bytes(i as u64, acc, *kind, *dl, *arg);
        enc.push(0x03);
        enc.extend_from_slice(&(p.len() as u32).to_be_bytes());
        enc.extend_from_slice(&p);
    }
    let inputs = decode_inputs(&enc).expect("v0.2 forced-request bytes decode");
    let out = e.apply_block(&Block { height: 1, time: 1_794_880_000_000, inputs }).unwrap();
    assert_eq!(out.outcomes, vec![Ok(()), Ok(()), Ok(())]);
    let cp = e.checkpoint_fields();
    assert_eq!(cp.forced_processed, 2, "cancel-all served; withdraw of 5 USDC (no position) served");
    let want = vault_forced_head(&reqs);
    eprintln!("shared vector: forcedHead(2) = {}", hex0x(&want));
    assert_eq!(cp.forced_head, want, "engine head = vault head");
    assert_eq!(e.withdrawals.len(), 1);
    assert_eq!((e.withdrawals[0].recipient, e.withdrawals[0].amount), (a, 4_000_000));
}

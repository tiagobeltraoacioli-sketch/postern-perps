//! Round 2 (agent 430): the snapshot format of SPEC section 16 (version 1). The property
//! `state_root(restore(snapshot(s))) == state_root(s)` is checked after every block of every fuzz run
//! (`tests/fuzz.rs`, P-4); here: strictness of the loader on a rich state.
mod common;
use postern_perps_engine::hash::keccak;
use postern_perps_engine::snapshot::{SnapshotError, MAGIC, VERSION};
use postern_perps_engine::testkit::*;
use postern_perps_engine::vectors::rehearsal;
use postern_perps_engine::*;

fn fix_checksum(b: &mut [u8]) {
    let n = b.len() - 32;
    let sum = keccak(&b[..n]);
    b[n..].copy_from_slice(&sum);
}

/// Offsets of the header fields (SPEC 16).
const GENESIS_HASH: std::ops::Range<usize> = 10..42;
const LOG_HASH: std::ops::Range<usize> = 58..90;

#[test]
fn round_trip_of_a_rich_state_and_of_the_genesis_state() {
    let (g, blocks, e) = rehearsal();
    let snap = e.snapshot();
    assert_eq!(&snap[..8], MAGIC);
    assert_eq!(u16::from_be_bytes([snap[8], snap[9]]), VERSION);
    assert_eq!(snap[GENESIS_HASH], g.hash);
    assert_eq!(snap[LOG_HASH], e.log_hash);
    let back = Engine::restore(g.clone(), &snap, Some(&e.state_root())).unwrap();
    assert_eq!(back.state_root(), e.state_root());
    assert_eq!(back.snapshot(), snap);
    assert_eq!((back.accounts.clone(), back.orders.clone(), back.by_addr.clone(), back.agent_owner.clone(), back.nonces.clone()), (e.accounts.clone(), e.orders.clone(), e.by_addr.clone(), e.agent_owner.clone(), e.nonces.clone()));
    assert_eq!((back.withdrawals.clone(), back.forced_queue.clone(), back.total_deposits, back.total_withdrawn, back.dust, back.launched), (e.withdrawals.clone(), e.forced_queue.clone(), e.total_deposits, e.total_withdrawn, e.dust, e.launched));
    for (mid, m) in &e.markets {
        assert_eq!(&back.markets[mid], m, "market {mid} (book queues, triggers, oracle state, samples)");
    }
    // the state is rich enough to mean something
    assert!(e.accounts.len() >= 6 && !e.orders.is_empty() && e.withdrawals.len() >= 2 && !e.nonces.is_empty());
    assert!(e.accounts.iter().any(|a| !a.agents.is_empty()) && e.accounts.iter().any(|a| !a.positions.is_empty()) && e.accounts.iter().any(|a| !a.leverage.is_empty()));
    assert!(e.markets.values().any(|m| !m.triggers.is_empty()) && e.markets.values().any(|m| !m.book.bids.is_empty()) && e.markets.values().any(|m| !m.book.asks.is_empty()));
    assert!(e.markets.values().any(|m| m.oracle.status != postern_perps_engine::oracle::Status::Active), "a halted market is in the snapshot");
    // genesis state (height 0)
    let e0 = Engine::new(g.clone());
    let s0 = e0.snapshot();
    let b0 = Engine::restore(g.clone(), &s0, Some(&e0.state_root())).unwrap();
    assert_eq!((b0.height, b0.snapshot()), (0, s0));
    // a restored engine continues exactly like the original: snapshot at every height of the rehearsal, then the
    // rest of the log
    let mut orig = Engine::new(g.clone());
    for (i, b) in blocks.iter().enumerate() {
        let mut resumed = Engine::restore(g.clone(), &orig.snapshot(), Some(&orig.state_root())).unwrap();
        for later in &blocks[i..] {
            resumed.apply_block(later).unwrap();
        }
        assert_eq!(resumed.snapshot(), snap, "resumed from height {i}");
        orig.apply_block(b).unwrap();
    }
}

#[test]
fn header_and_checksum_errors() {
    let (g, _, e) = rehearsal();
    let snap = e.snapshot();
    let root = e.state_root();
    assert_eq!(Engine::restore(g.clone(), &[], None).err(), Some(SnapshotError::Format("too short")));
    assert_eq!(Engine::restore(g.clone(), &snap[..73], None).err(), Some(SnapshotError::Format("too short")));
    let mut b = snap.clone();
    b[0] ^= 1;
    fix_checksum(&mut b);
    assert_eq!(Engine::restore(g.clone(), &b, None).err(), Some(SnapshotError::Format("magic")));
    let mut b = snap.clone();
    b[9] = 2;
    fix_checksum(&mut b);
    assert_eq!(Engine::restore(g.clone(), &b, None).err(), Some(SnapshotError::Version(2)), "an unknown version is refused, never guessed");
    let other = genesis(5, vec![market(1, "BTC-USD")]);
    assert_eq!(Engine::restore(other, &snap, None).err(), Some(SnapshotError::Genesis));
    let mut wrong = root;
    wrong[0] ^= 1;
    assert!(matches!(Engine::restore(g.clone(), &snap, Some(&wrong)), Err(SnapshotError::Root { .. })));
    // every truncation and one appended byte
    for n in 0..snap.len() {
        assert!(Engine::restore(g.clone(), &snap[..n], Some(&root)).is_err(), "truncated to {n} bytes");
    }
    let mut longer = snap.clone();
    longer.push(0);
    assert_eq!(Engine::restore(g.clone(), &longer, Some(&root)).err(), Some(SnapshotError::Checksum));
    // every single-bit damage is caught by the checksum
    for i in 0..snap.len() {
        let mut b = snap.clone();
        b[i] ^= 1 << (i % 8);
        assert!(Engine::restore(g.clone(), &b, Some(&root)).is_err(), "bit flip in byte {i} accepted");
        if i >= 10 && i < snap.len() - 32 {
            assert_eq!(Engine::restore(g.clone(), &b, None).err(), Some(SnapshotError::Checksum), "byte {i}");
        }
    }
}

/// Forgery (the checksum recomputed after the change): with the expected state root, EVERY byte of the body is
/// bound, except the 32 bytes of `logHash`, which the state root does not cover (SPEC 16: the node compares
/// `logHash` with its own log; stated, and pinned here so that a change of this fact is noticed).
#[test]
fn every_forged_byte_is_refused_against_the_expected_root_except_log_hash() {
    let (g, _, e) = rehearsal();
    let snap = e.snapshot();
    let root = e.state_root();
    let body = snap.len() - 32;
    let (mut refused, mut log_hash_accepted) = (0usize, 0usize);
    let mut kinds: std::collections::BTreeMap<&'static str, usize> = Default::default();
    for i in 0..body {
        for delta in [1u8, 0x80] {
            let mut b = snap.clone();
            b[i] ^= delta;
            fix_checksum(&mut b);
            match Engine::restore(g.clone(), &b, Some(&root)) {
                Ok(x) => {
                    assert!(LOG_HASH.contains(&i), "forged byte {i} (xor {delta:#x}) accepted with the expected root");
                    assert_eq!(x.state_root(), root);
                    assert_ne!(x.log_hash, e.log_hash);
                    log_hash_accepted += 1;
                }
                Err(er) => {
                    assert!(!LOG_HASH.contains(&i), "logHash byte {i} refused: the root covers it now? update SPEC 16");
                    refused += 1;
                    *kinds
                        .entry(match er {
                            SnapshotError::Format(_) => "format",
                            SnapshotError::Version(_) => "version",
                            SnapshotError::Checksum => "checksum",
                            SnapshotError::Genesis => "genesis",
                            SnapshotError::Invalid(_) => "invalid",
                            SnapshotError::Root { .. } => "root",
                        })
                        .or_default() += 1;
                }
            }
        }
    }
    // extreme magnitudes: every 8-byte window set to 0xff.. and every 16-byte window to 0x7f ff.. (u64::MAX,
    // i128::MAX and everything they overlap). None may be accepted outside logHash, and NONE MAY PANIC (S-0.2: the
    // loader checks magnitudes before it computes with them; release builds keep overflow checks).
    let mut windows = 0usize;
    for (len, first) in [(8usize, 0xffu8), (16, 0x7f)] {
        for i in 0..=(body - len) {
            let mut b = snap.clone();
            b[i] = first;
            for x in &mut b[i + 1..i + len] {
                *x = 0xff;
            }
            if b == snap {
                continue;
            }
            fix_checksum(&mut b);
            let inside_log_hash = i >= LOG_HASH.start && i + len <= LOG_HASH.end;
            match Engine::restore(g.clone(), &b, Some(&root)) {
                Ok(_) => assert!(inside_log_hash, "forged window {i}+{len} accepted"),
                Err(_) => assert!(!inside_log_hash),
            }
            // and without an expected root it must still never panic (it may load: the caller chose not to bind it)
            let _ = Engine::restore(g.clone(), &b, None);
            windows += 1;
        }
    }
    assert!(windows > 2 * (body - 100));
    eprintln!("forged-byte census over {body} bytes: {kinds:?}; logHash accepted {log_hash_accepted}; extreme windows {windows}");
    assert_eq!(log_hash_accepted, 64);
    assert_eq!(refused, (body - 32) * 2);
    assert!(kinds.get("root").copied().unwrap_or(0) > 0 && kinds.get("invalid").copied().unwrap_or(0) > 0 && kinds.get("genesis").copied().unwrap_or(0) == 64);
    assert_eq!(kinds.get("checksum"), None, "the checksum was recomputed: every refusal here is a semantic one");
}

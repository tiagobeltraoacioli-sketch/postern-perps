//! Round 2 (agent 431): the input log's index (`blocks.idx`) and the torn-tail rule.
//!
//! Gap listed by 351/389: "the torn-tail rule can take a damaged length field of the last record for a torn write".
//! A record that was durable (fsynced, its block possibly published and signed) and is damaged afterwards looked
//! like a half-written append and was silently cut: the node restarted one block short and would re-produce that
//! height with other content. The index records every completed append (after the record's fsync), so the log can
//! tell "this append never completed" (cut it) from "this record was complete and is damaged" (refuse to open).
//! Tests 1, 2 and 5 failed on `70578a5` (the log opened and dropped a durable block). THROWAWAY keys only.

mod common;

use common::*;
use perps_node::genesis::Genesis;
use perps_node::log::InputLog;
use perps_node::sig::key_address;
use std::path::PathBuf;
use std::sync::Arc;

/// A data directory with `n` blocks (one deposit each), closed. Returns it with the genesis and each record's
/// (offset, total length) in `blocks.log`.
fn make(name: &str, n: u64) -> (PathBuf, Arc<Genesis>, Vec<(usize, usize)>) {
    let mut net = net(name, 1000);
    let ua = key_address(&user(21));
    for i in 0..n {
        net.seq_view.lock().unwrap().deposits.insert(i, deposit(i, ua, 1_000_000 + i));
        net.seq.produce(now() + i).unwrap().unwrap();
    }
    assert_eq!(net.seq.log.last_height, n);
    let (dir, g) = (net.dir.clone(), net.g.clone());
    drop(net);
    let log = std::fs::read(dir.join("blocks.log")).unwrap();
    let mut recs = Vec::new();
    let mut pos = 0;
    while pos < log.len() {
        assert_eq!(&log[pos..pos + 4], b"PPLB");
        let len = u32::from_be_bytes(log[pos + 4..pos + 8].try_into().unwrap()) as usize;
        recs.push((pos, 8 + len + 32));
        pos += 8 + len + 32;
    }
    assert_eq!(recs.len() as u64, n);
    (dir, g, recs)
}

fn open(dir: &PathBuf, g: &Genesis) -> Result<(u64, u64), String> {
    InputLog::open(dir, g).map(|(l, r)| (l.last_height, r.torn_tail_bytes))
}

fn edit(dir: &PathBuf, file: &str, f: impl FnOnce(&mut Vec<u8>)) {
    let p = dir.join(file);
    let mut b = std::fs::read(&p).unwrap();
    f(&mut b);
    std::fs::write(&p, &b).unwrap();
}

/// 1. The length field of the last DURABLE record is damaged upward: before the index this was "a torn tail"
/// (the declared extent passes the end of the file) and the block was cut.
#[test]
fn a_damaged_length_field_of_the_last_durable_record_is_refused_not_cut() {
    let (dir, g, recs) = make("idx-len", 4);
    let (off, _) = recs[3];
    edit(&dir, "blocks.log", |b| b[off + 5] ^= 0x40); // len grows by 4 MiB: extent beyond the end of the file
    let r = open(&dir, &g);
    assert!(r.is_err(), "a durable block must never be dropped as a torn write: {r:?}");
    assert!(r.unwrap_err().contains("index"), "the refusal names the index's evidence");
    // the file was not truncated by the refused open
    assert_eq!(std::fs::metadata(dir.join("blocks.log")).unwrap().len() as usize, off + recs[3].1);
}

/// 2. A byte of the last durable record's body is damaged (its length is right, its checksum fails at the very
/// end of the file): the same silent cut before the index.
#[test]
fn a_damaged_body_of_the_last_durable_record_is_refused_not_cut() {
    let (dir, g, recs) = make("idx-body", 4);
    let (off, len) = recs[3];
    edit(&dir, "blocks.log", |b| b[off + len / 2] ^= 0x01);
    assert!(open(&dir, &g).is_err());
    // and its checksum bytes
    let (dir, g, recs) = make("idx-sum", 4);
    let (off, len) = recs[3];
    edit(&dir, "blocks.log", |b| b[off + len - 1] ^= 0x01);
    assert!(open(&dir, &g).is_err());
}

/// 3. An append that never completed (the crash the rule exists for) is still cut: a partial record with no
/// index entry.
#[test]
fn an_append_that_never_completed_is_still_cut() {
    let (dir, g, recs) = make("idx-torn", 4);
    let (off, len) = recs[3];
    let full = std::fs::read(dir.join("blocks.log")).unwrap();
    // (a) a header and some bytes; (b) a whole record minus its last byte; (c) seven bytes of a header
    for extra in [b"PPLB\x00\x00\x10\x00partial".to_vec(), full[off..off + len - 1].to_vec(), b"PPLB\x00\x00\x00".to_vec()] {
        let mut b = full.clone();
        b.extend_from_slice(&extra);
        std::fs::write(dir.join("blocks.log"), &b).unwrap();
        assert_eq!(open(&dir, &g), Ok((4, extra.len() as u64)));
        assert_eq!(std::fs::read(dir.join("blocks.log")).unwrap(), full, "cut back to the last complete record");
    }
}

/// 4. A crash between the record's fsync and the index entry's: the record is complete and valid, the index does
/// not know it yet. The block is kept (it is durable and signed) and the index is repaired, so later damage to
/// that record is refused like any other.
#[test]
fn a_complete_record_without_its_index_entry_is_kept_and_indexed() {
    let (dir, g, recs) = make("idx-repair", 4);
    let idx = std::fs::read(dir.join("blocks.idx")).unwrap();
    let entry = (idx.len() - 8) / 4;
    edit(&dir, "blocks.idx", |b| b.truncate(b.len() - entry)); // the last entry was never written
    assert_eq!(open(&dir, &g), Ok((4, 0)));
    assert_eq!(std::fs::read(dir.join("blocks.idx")).unwrap(), idx, "the missing entry was added: byte-identical index");
    let (off, len) = recs[3];
    edit(&dir, "blocks.log", |b| b[off + len / 2] ^= 0x01);
    assert!(open(&dir, &g).is_err(), "now the record is known to be complete: damage is refused");
}

/// 5. The log lost its last record entirely (a truncated file): the index knows four blocks were durable.
#[test]
fn a_log_shorter_than_its_index_is_refused() {
    let (dir, g, recs) = make("idx-short", 4);
    edit(&dir, "blocks.log", |b| b.truncate(recs[3].0));
    let r = open(&dir, &g);
    assert!(r.is_err(), "three blocks in the log, four in the index: {r:?}");
    // and a log cut inside the last record
    let (dir, g, recs) = make("idx-short2", 4);
    edit(&dir, "blocks.log", |b| b.truncate(recs[3].0 + 20));
    assert!(open(&dir, &g).is_err(), "a partial last record whose append the index recorded as complete");
}

/// 6. A data directory written before the index existed opens and gets one (the rule protects it from then on).
#[test]
fn a_data_directory_without_an_index_gets_one() {
    let (dir, g, recs) = make("idx-new", 4);
    let idx = std::fs::read(dir.join("blocks.idx")).unwrap();
    std::fs::remove_file(dir.join("blocks.idx")).unwrap();
    assert_eq!(open(&dir, &g), Ok((4, 0)));
    assert_eq!(std::fs::read(dir.join("blocks.idx")).unwrap(), idx);
    let (off, _) = recs[3];
    edit(&dir, "blocks.log", |b| b[off + 5] ^= 0x40);
    assert!(open(&dir, &g).is_err());
}

/// 7. A partial last index entry means the index write had begun, so the record's own fsync had completed: the
/// record is durable. With an intact record the entry is repaired; with a damaged record the log refuses.
#[test]
fn a_partial_index_entry_proves_the_record_was_durable() {
    let (dir, g, recs) = make("idx-partial", 4);
    let idx = std::fs::read(dir.join("blocks.idx")).unwrap();
    edit(&dir, "blocks.idx", |b| b.truncate(b.len() - 10));
    assert_eq!(open(&dir, &g), Ok((4, 0)));
    assert_eq!(std::fs::read(dir.join("blocks.idx")).unwrap(), idx);
    edit(&dir, "blocks.idx", |b| b.truncate(b.len() - 10));
    let (off, len) = recs[3];
    edit(&dir, "blocks.log", |b| b[off + len / 2] ^= 0x01);
    assert!(open(&dir, &g).is_err());
}

/// 8. An index that disagrees with the log about an earlier record (offset, length or body hash), or that is
/// damaged itself, refuses the open: neither file is trusted over the other silently.
#[test]
fn an_index_that_disagrees_with_the_log_is_refused() {
    let (dir, g, _) = make("idx-mismatch", 4);
    let idx = std::fs::read(dir.join("blocks.idx")).unwrap();
    let entry = (idx.len() - 8) / 4;
    for at in [8 + entry + 3, 8 + 2 * entry + 20, 8 + entry - 1, 2] {
        std::fs::write(dir.join("blocks.idx"), &idx).unwrap();
        edit(&dir, "blocks.idx", |b| b[at] ^= 0x01);
        assert!(open(&dir, &g).is_err(), "index byte {at} altered");
    }
    std::fs::write(dir.join("blocks.idx"), &idx).unwrap();
    assert_eq!(open(&dir, &g), Ok((4, 0)));
}

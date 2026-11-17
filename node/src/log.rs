//! The durable, hash-chained input log.
//!
//! File `blocks.log`: a sequence of records `magic "PPLB" | len:u32be | body[len] | keccak256(body)`, body =
//! `Block::to_bytes()`. A block is appended and `fsync`ed BEFORE any result of it is published (an action is
//! reported `included` only once it is durable).
//!
//! Opening verifies every record: checksum, `logHash` recomputed from its inputs, `prevLogHash` chained from the
//! genesis `logHash(0)`, heights consecutive from 1, times strictly increasing, sequencer signature. A torn LAST
//! record (crash during append: short read or bad checksum at the very end) is cut off and reported; any defect
//! followed by more data is corruption and the log refuses to open (fail closed: never silently skip a block).
//!
//! UNAUDITED PROTOTYPE.

use crate::eip712::digest;
use crate::genesis::Genesis;
use crate::sig::recover;
use crate::types::Block;
use crate::util::{keccak, word_u128, H256};
use std::fs::{File, OpenOptions};
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};

const MAGIC: &[u8; 4] = b"PPLB";
pub const MAX_BLOCK_BYTES: usize = 64 * 1024 * 1024;

pub const BLOCK_SIG_TYPE: &str = "Block(uint64 height,uint64 time,bytes32 prevLogHash,bytes32 logHash,bytes32 stateRoot)";

/// What the sequencer signs for each block (engine domain; a different primary type from every user action).
pub fn block_digest(g: &Genesis, b: &Block) -> H256 {
    let h = &b.header;
    let mut s = Vec::with_capacity(32 * 6);
    s.extend_from_slice(&keccak(BLOCK_SIG_TYPE.as_bytes()));
    s.extend_from_slice(&word_u128(h.height as u128));
    s.extend_from_slice(&word_u128(h.time as u128));
    s.extend_from_slice(&h.prev_log_hash);
    s.extend_from_slice(&h.log_hash);
    s.extend_from_slice(&h.state_root);
    digest(&g.engine_domain.separator(), &keccak(&s))
}

/// Chain checks of one block against its predecessor (height, prevLogHash, time, logHash, sequencer signature).
pub fn check_block_link(g: &Genesis, prev_height: u64, prev_log_hash: &H256, prev_time: u64, b: &Block) -> Result<(), String> {
    let h = &b.header;
    if h.height != prev_height + 1 {
        return Err(format!("height {} does not follow {prev_height}", h.height));
    }
    if &h.prev_log_hash != prev_log_hash {
        return Err(format!("block {}: prevLogHash does not chain", h.height));
    }
    if h.time <= prev_time {
        return Err(format!("block {}: time not strictly increasing", h.height));
    }
    b.check_log_hash()?;
    let s = recover(&block_digest(g, b), &b.sequencer_signature).map_err(|e| format!("block {}: sequencer signature: {e}", h.height))?;
    if s != g.sequencer {
        return Err(format!("block {}: not signed by the genesis sequencer", h.height));
    }
    Ok(())
}

pub struct InputLog {
    path: PathBuf,
    file: File,
    /// byte offset of each record (index = height - 1)
    offsets: Vec<u64>,
    pub last_height: u64,
    pub last_log_hash: H256,
    pub last_time: u64,
    pub last_state_root: H256,
    pub torn_tail_bytes: u64,
    pub fsync: bool,
}

#[derive(Debug)]
pub struct OpenReport {
    pub blocks: u64,
    pub torn_tail_bytes: u64,
}

impl InputLog {
    pub fn open(dir: &Path, g: &Genesis) -> Result<(InputLog, OpenReport), String> {
        std::fs::create_dir_all(dir).map_err(|e| format!("create {}: {e}", dir.display()))?;
        let path = dir.join("blocks.log");
        let mut file = OpenOptions::new().read(true).append(true).create(true).open(&path).map_err(|e| format!("open log: {e}"))?;
        let mut data = Vec::new();
        file.read_to_end(&mut data).map_err(|e| format!("read log: {e}"))?;
        let mut offsets = Vec::new();
        let mut pos = 0usize;
        let (mut lh, mut lt, mut lr, mut height) = (g.genesis_log_hash(), g.genesis_time, crate::util::ZERO_H256, 0u64);
        let mut torn = 0u64;
        while pos < data.len() {
            let rest = &data[pos..];
            let parsed = (|| -> Result<(Block, usize), String> {
                if rest.len() < 8 {
                    return Err("short header".into());
                }
                if &rest[..4] != MAGIC {
                    return Err("bad magic".into());
                }
                let len = u32::from_be_bytes(rest[4..8].try_into().unwrap()) as usize;
                if len > MAX_BLOCK_BYTES {
                    return Err("record too large".into());
                }
                if rest.len() < 8 + len + 32 {
                    return Err("short record".into());
                }
                let body = &rest[8..8 + len];
                if keccak(body)[..] != rest[8 + len..8 + len + 32] {
                    return Err("checksum".into());
                }
                Ok((Block::from_bytes(body)?, 8 + len + 32))
            })();
            match parsed {
                Ok((b, n)) => {
                    check_block_link(g, height, &lh, lt, &b).map_err(|e| format!("input log corrupt at offset {pos}: {e}"))?;
                    offsets.push(pos as u64);
                    height = b.header.height;
                    lh = b.header.log_hash;
                    lt = b.header.time;
                    lr = b.header.state_root;
                    pos += n;
                }
                Err(e) => {
                    // Only the final append can be torn (each append is fsynced before the next). Torn means: the
                    // header is incomplete, or the header is valid and the record's declared extent reaches or
                    // passes the end of the file. Anything else (bad magic with 8+ bytes left, a bad checksum with
                    // data after the record) is corruption: refuse to open.
                    let torn_here = rest.len() < 8
                        || (&rest[..4] == MAGIC && {
                            let len = u32::from_be_bytes(rest[4..8].try_into().unwrap()) as usize;
                            len <= MAX_BLOCK_BYTES && rest.len() <= 8 + len + 32
                        });
                    if !torn_here {
                        return Err(format!("input log corrupt at offset {pos} ({e}): refusing to open"));
                    }
                    torn = (data.len() - pos) as u64;
                    break;
                }
            }
        }
        if torn > 0 {
            file.set_len((data.len() as u64) - torn).map_err(|e| format!("truncate torn tail: {e}"))?;
            file.sync_all().map_err(|e| format!("fsync: {e}"))?;
        }
        file.seek(SeekFrom::End(0)).map_err(|e| e.to_string())?;
        let log = InputLog { path, file, offsets, last_height: height, last_log_hash: lh, last_time: lt, last_state_root: lr, torn_tail_bytes: torn, fsync: true };
        let rep = OpenReport { blocks: height, torn_tail_bytes: torn };
        Ok((log, rep))
    }

    /// Appends a block that the caller has already linked (`check_block_link`) and applied; durable on return.
    pub fn append(&mut self, g: &Genesis, b: &Block) -> Result<(), String> {
        check_block_link(g, self.last_height, &self.last_log_hash, self.last_time, b)?;
        let body = b.to_bytes();
        let mut rec = Vec::with_capacity(8 + body.len() + 32);
        rec.extend_from_slice(MAGIC);
        rec.extend_from_slice(&(body.len() as u32).to_be_bytes());
        rec.extend_from_slice(&body);
        rec.extend_from_slice(&keccak(&body));
        let off = self.file.seek(SeekFrom::End(0)).map_err(|e| e.to_string())?;
        self.file.write_all(&rec).map_err(|e| format!("append: {e}"))?;
        if self.fsync {
            self.file.sync_data().map_err(|e| format!("fsync: {e}"))?;
        }
        self.offsets.push(off);
        self.last_height = b.header.height;
        self.last_log_hash = b.header.log_hash;
        self.last_time = b.header.time;
        self.last_state_root = b.header.state_root;
        Ok(())
    }

    pub fn read(&self, height: u64) -> Result<Option<Block>, String> {
        if height == 0 || height > self.last_height {
            return Ok(None);
        }
        let off = self.offsets[(height - 1) as usize];
        let mut f = File::open(&self.path).map_err(|e| e.to_string())?;
        f.seek(SeekFrom::Start(off)).map_err(|e| e.to_string())?;
        let mut hdr = [0u8; 8];
        f.read_exact(&mut hdr).map_err(|e| e.to_string())?;
        let len = u32::from_be_bytes(hdr[4..8].try_into().unwrap()) as usize;
        let mut body = vec![0u8; len + 32];
        f.read_exact(&mut body).map_err(|e| e.to_string())?;
        if keccak(&body[..len])[..] != body[len..] {
            return Err(format!("checksum of block {height} changed on disk"));
        }
        Ok(Some(Block::from_bytes(&body[..len])?))
    }

    pub fn path(&self) -> &Path {
        &self.path
    }
}

// ---------------- snapshots ----------------

/// Snapshot file `snap-<height>.bin`: `"PPSN" | height:u64 | time:u64 | logHash | stateRoot | engineName len:u8 + bytes
/// | len:u64 | engine snapshot | keccak256(all previous bytes)`. Written to a temporary name, fsynced, renamed, and
/// the directory fsynced, so a crash leaves either the old or the new snapshot, never half of one.
pub struct SnapshotMeta {
    pub height: u64,
    pub time: u64,
    pub log_hash: H256,
    pub state_root: H256,
}

pub fn write_snapshot(dir: &Path, meta: &SnapshotMeta, engine_name: &str, bytes: &[u8], keep: usize) -> Result<PathBuf, String> {
    let mut b = Vec::with_capacity(bytes.len() + 128);
    b.extend_from_slice(b"PPSN");
    b.extend_from_slice(&meta.height.to_be_bytes());
    b.extend_from_slice(&meta.time.to_be_bytes());
    b.extend_from_slice(&meta.log_hash);
    b.extend_from_slice(&meta.state_root);
    b.push(engine_name.len() as u8);
    b.extend_from_slice(engine_name.as_bytes());
    b.extend_from_slice(&(bytes.len() as u64).to_be_bytes());
    b.extend_from_slice(bytes);
    let c = keccak(&b);
    b.extend_from_slice(&c);
    let tmp = dir.join(format!("snap-{:012}.tmp", meta.height));
    let fin = dir.join(format!("snap-{:012}.bin", meta.height));
    {
        let mut f = File::create(&tmp).map_err(|e| format!("snapshot create: {e}"))?;
        f.write_all(&b).map_err(|e| format!("snapshot write: {e}"))?;
        f.sync_all().map_err(|e| format!("snapshot fsync: {e}"))?;
    }
    std::fs::rename(&tmp, &fin).map_err(|e| format!("snapshot rename: {e}"))?;
    if let Ok(d) = File::open(dir) {
        let _ = d.sync_all();
    }
    // keep the newest `keep` snapshots
    let mut snaps = list_snapshots(dir);
    snaps.sort();
    while snaps.len() > keep {
        let (_, p) = snaps.remove(0);
        let _ = std::fs::remove_file(p);
    }
    Ok(fin)
}

pub fn list_snapshots(dir: &Path) -> Vec<(u64, PathBuf)> {
    let mut v = Vec::new();
    if let Ok(rd) = std::fs::read_dir(dir) {
        for e in rd.flatten() {
            let n = e.file_name().to_string_lossy().to_string();
            if let Some(h) = n.strip_prefix("snap-").and_then(|s| s.strip_suffix(".bin")).and_then(|s| s.parse::<u64>().ok()) {
                v.push((h, e.path()));
            }
        }
    }
    v
}

pub fn read_snapshot(path: &Path, engine_name: &str) -> Result<(SnapshotMeta, Vec<u8>), String> {
    let b = std::fs::read(path).map_err(|e| format!("read snapshot: {e}"))?;
    if b.len() < 4 + 8 + 8 + 64 + 1 + 8 + 32 || &b[..4] != b"PPSN" {
        return Err("not a snapshot".into());
    }
    let (body, chk) = b.split_at(b.len() - 32);
    if keccak(body)[..] != *chk {
        return Err("snapshot checksum".into());
    }
    let mut r = crate::eip712::Reader::new(body);
    r.take(4)?;
    let meta = SnapshotMeta { height: r.u64()?, time: r.u64()?, log_hash: r.h256()?, state_root: r.h256()? };
    let nl = r.u8()? as usize;
    let name = r.take(nl)?;
    if name != engine_name.as_bytes() {
        return Err("snapshot of another engine".into());
    }
    let n = r.u64()? as usize;
    let eng = r.take(n)?.to_vec();
    r.expect_done()?;
    Ok((meta, eng))
}

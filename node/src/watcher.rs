//! L2 watcher: reads the vault's `Deposit` and `ForcedRequest` events from the Bloch L2 (chain 700770) at the
//! `finalized` tag, from TWO independent endpoints that must agree, pinned to one block hash.
//!
//! One poll:
//! 1. `eth_chainId` of both endpoints must be 700770 (checked once per endpoint and on every reconnect).
//! 2. `eth_getBlockByNumber("finalized")` from both; `F = min(numbers)`.
//! 3. The hash of block `F` from both must be equal (the pin). If not: no progress, `endpointsAgree = false`.
//! 4. `eth_getLogs{fromBlock: next, toBlock: F, address: vault, topics: [[Deposit, ForcedRequest]]}` from both;
//!    the two log lists must be identical; every log has `removed = false`, lies in `[next, F]`, and its
//!    `blockHash` equals the hash of its block number as BOTH endpoints report it.
//! 5. Events are decoded strictly; deposit and forced indices must continue the previous ones without gap.
//! Any failure leaves the view unchanged (fail closed: a deposit is credited late, never on one endpoint's word).
//!
//! Events of `docs/VAULT.md` v0.1 (agent 390):
//! `Deposit(uint64 indexed index, address indexed account, address indexed sender, uint64 amount, bytes32 head)`,
//! `ForcedRequest(uint64 indexed index, address indexed account, uint8 kind, uint32 marketId, uint64 amount, uint64 deadline, bytes32 head)`.
//! 6. Every event's `head` must equal the chain head the watcher computes itself (VAULT.md 5.4) from the previous
//!    head (`head0` = keccak256(abi.encode(bytes32 tag, chainid, vault))): a log that does not chain is refused.
//!
//! Transport: plain HTTP/1.1 (loopback, or a local TLS-terminating proxy for a remote endpoint): no TLS client is
//! linked in this prototype (stated gap).
//!
//! UNAUDITED PROTOTYPE.

use crate::types::{DepositEvent, ForcedEvent, ForcedKind};
use crate::vaultfmt::{deposit_head0, deposit_head_next, forced_head0, forced_head_next};
use crate::util::{keccak, parse_hex_fixed, parse_hex_vec, to_hex, Address, H256};
use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;

pub const L2_CHAIN_ID: u64 = 700_770;

#[derive(Clone, Debug)]
pub struct WatcherConfig {
    pub endpoints: [String; 2],
    pub vault: Address,
    pub deposit_sig: String,
    pub forced_sig: String,
    pub poll_ms: u64,
    pub max_range: u64,
    pub start_block: u64,
}

impl WatcherConfig {
    pub fn new(endpoints: [String; 2], vault: Address) -> Self {
        WatcherConfig {
            endpoints,
            vault,
            deposit_sig: "Deposit(uint64,address,address,uint64,bytes32)".into(),
            forced_sig: "ForcedRequest(uint64,address,uint8,uint32,uint64,uint64,bytes32)".into(),
            poll_ms: 1000,
            max_range: 2000,
            start_block: 0,
        }
    }
}

#[derive(Clone, Debug, Default)]
pub struct L2View {
    pub finalized_block: u64,
    pub finalized_hash: Option<H256>,
    pub scanned_to: u64,
    pub deposits: BTreeMap<u64, DepositEvent>,
    pub forced: BTreeMap<u64, ForcedEvent>,
    pub endpoints_agree: bool,
    pub last_error: Option<String>,
    pub polls: u64,
    pub disagreements: u64,
}

pub type SharedView = Arc<Mutex<L2View>>;

// ---------------- minimal JSON-RPC over HTTP/1.1 ----------------

/// Minimal HTTP/1.1 client (one request per connection, `Connection: close`), shared by the watcher, the remote
/// validator and the tests. Returns (status, body).
pub async fn http_call(method: &str, url: &str, body: Option<&str>, extra_headers: &[(&str, &str)]) -> Result<(u16, Vec<u8>), String> {
    let rest = url.strip_prefix("http://").ok_or("only http:// endpoints (see module note)")?;
    let (hostport, path) = match rest.find('/') {
        Some(i) => (&rest[..i], &rest[i..]),
        None => (rest, "/"),
    };
    let mut req = format!("{method} {path} HTTP/1.1\r\nHost: {hostport}\r\nConnection: close\r\n");
    for (k, v) in extra_headers {
        req.push_str(&format!("{k}: {v}\r\n"));
    }
    if let Some(b) = body {
        req.push_str(&format!("Content-Type: application/json\r\nContent-Length: {}\r\n\r\n{b}", b.len()));
    } else {
        req.push_str("\r\n");
    }
    let fut = async {
        let mut s = TcpStream::connect(hostport).await.map_err(|e| format!("connect {hostport}: {e}"))?;
        s.write_all(req.as_bytes()).await.map_err(|e| e.to_string())?;
        let mut buf = Vec::new();
        let mut chunk = [0u8; 16384];
        loop {
            let n = s.read(&mut chunk).await.map_err(|e| e.to_string())?;
            if n == 0 {
                break;
            }
            buf.extend_from_slice(&chunk[..n]);
            if buf.len() > 64 * 1024 * 1024 {
                return Err("response too large".into());
            }
        }
        Ok::<Vec<u8>, String>(buf)
    };
    let buf = tokio::time::timeout(Duration::from_secs(10), fut).await.map_err(|_| "http timeout".to_string())??;
    let split = buf.windows(4).position(|w| w == b"\r\n\r\n").ok_or("bad HTTP response")?;
    let head = std::str::from_utf8(&buf[..split]).map_err(|_| "bad HTTP head")?;
    let status: u16 = head.lines().next().and_then(|l| l.split(' ').nth(1)).and_then(|c| c.parse().ok()).ok_or("bad status line")?;
    let mut b = buf[split + 4..].to_vec();
    if head.to_ascii_lowercase().contains("transfer-encoding: chunked") {
        b = dechunk(&b)?;
    }
    Ok((status, b))
}

pub async fn rpc(url: &str, method: &str, params: serde_json::Value) -> Result<serde_json::Value, String> {
    let body = serde_json::json!({"jsonrpc": "2.0", "id": 1, "method": method, "params": params}).to_string();
    let (st, b) = http_call("POST", url, Some(&body), &[]).await?;
    if st != 200 {
        return Err(format!("HTTP status {st}"));
    }
    let v: serde_json::Value = serde_json::from_slice(&b).map_err(|e| format!("rpc json: {e}"))?;
    if let Some(e) = v.get("error") {
        return Err(format!("rpc error: {e}"));
    }
    v.get("result").cloned().ok_or_else(|| "rpc without result".into())
}

fn dechunk(b: &[u8]) -> Result<Vec<u8>, String> {
    let mut out = Vec::new();
    let mut p = 0;
    loop {
        let le = b[p..].windows(2).position(|w| w == b"\r\n").ok_or("chunk")? + p;
        let n = usize::from_str_radix(std::str::from_utf8(&b[p..le]).map_err(|_| "chunk")?.split(';').next().unwrap().trim(), 16).map_err(|_| "chunk size")?;
        p = le + 2;
        if n == 0 {
            return Ok(out);
        }
        if p + n > b.len() {
            return Err("chunk overrun".into());
        }
        out.extend_from_slice(&b[p..p + n]);
        p += n + 2;
    }
}

fn hex_u64(v: &serde_json::Value) -> Result<u64, String> {
    let s = v.as_str().ok_or("expected hex quantity")?;
    let body = s.strip_prefix("0x").ok_or("expected 0x quantity")?;
    if body.is_empty() || body.len() > 16 || (body.len() > 1 && body.starts_with('0')) {
        return Err(format!("bad quantity {s}"));
    }
    u64::from_str_radix(body, 16).map_err(|_| format!("bad quantity {s}"))
}

fn qty(n: u64) -> String {
    format!("0x{n:x}")
}

pub async fn chain_id(url: &str) -> Result<u64, String> {
    hex_u64(&rpc(url, "eth_chainId", serde_json::json!([])).await?)
}

async fn block_num_hash(url: &str, tag: serde_json::Value) -> Result<(u64, H256), String> {
    let b = rpc(url, "eth_getBlockByNumber", serde_json::json!([tag, false])).await?;
    if b.is_null() {
        return Err("block not found".into());
    }
    let n = hex_u64(b.get("number").ok_or("block.number")?)?;
    let h = parse_hex_fixed::<32>(b.get("hash").and_then(|v| v.as_str()).ok_or("block.hash")?)?;
    Ok((n, h))
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RawLog {
    pub block_number: u64,
    pub block_hash: H256,
    pub tx_hash: H256,
    pub log_index: u32,
    pub topics: Vec<H256>,
    pub data: Vec<u8>,
}

fn parse_logs(v: &serde_json::Value, vault: &Address, from: u64, to: u64) -> Result<Vec<RawLog>, String> {
    let arr = v.as_array().ok_or("logs must be an array")?;
    let mut out = Vec::with_capacity(arr.len());
    for l in arr {
        if l.get("removed").and_then(|x| x.as_bool()).unwrap_or(false) {
            return Err("removed log at a finalized range".into());
        }
        let addr = parse_hex_fixed::<20>(l.get("address").and_then(|x| x.as_str()).ok_or("log.address")?)?;
        if &addr != vault {
            return Err("log from another address".into());
        }
        let bn = hex_u64(l.get("blockNumber").ok_or("log.blockNumber")?)?;
        if bn < from || bn > to {
            return Err("log outside the requested range".into());
        }
        let li = hex_u64(l.get("logIndex").ok_or("log.logIndex")?)?;
        out.push(RawLog {
            block_number: bn,
            block_hash: parse_hex_fixed::<32>(l.get("blockHash").and_then(|x| x.as_str()).ok_or("log.blockHash")?)?,
            tx_hash: parse_hex_fixed::<32>(l.get("transactionHash").and_then(|x| x.as_str()).ok_or("log.transactionHash")?)?,
            log_index: u32::try_from(li).map_err(|_| "logIndex")?,
            topics: l
                .get("topics")
                .and_then(|x| x.as_array())
                .ok_or("log.topics")?
                .iter()
                .map(|t| parse_hex_fixed::<32>(t.as_str().unwrap_or("")))
                .collect::<Result<_, _>>()?,
            data: parse_hex_vec(l.get("data").and_then(|x| x.as_str()).ok_or("log.data")?)?,
        });
    }
    out.sort_by_key(|l| (l.block_number, l.log_index));
    for w in out.windows(2) {
        if (w[0].block_number, w[0].log_index) == (w[1].block_number, w[1].log_index) {
            return Err("duplicate log position".into());
        }
    }
    Ok(out)
}

fn word_to_u64(w: &[u8]) -> Result<u64, String> {
    if w.len() != 32 || w[..24].iter().any(|b| *b != 0) {
        return Err("word does not fit u64".into());
    }
    Ok(u64::from_be_bytes(w[24..].try_into().unwrap()))
}

fn word_to_addr(w: &[u8]) -> Result<Address, String> {
    if w.len() != 32 || w[..12].iter().any(|b| *b != 0) {
        return Err("word is not an address".into());
    }
    Ok(w[12..].try_into().unwrap())
}

pub enum Event {
    Deposit(DepositEvent),
    Forced(ForcedEvent),
}

pub fn decode_event(cfg: &WatcherConfig, l: &RawLog) -> Result<Event, String> {
    let t0 = *l.topics.first().ok_or("log without topic")?;
    let h = |w: &[u8]| -> H256 { w.try_into().unwrap() };
    if t0 == keccak(cfg.deposit_sig.as_bytes()) {
        if l.topics.len() != 4 || l.data.len() != 64 {
            return Err("Deposit log shape".into());
        }
        Ok(Event::Deposit(DepositEvent {
            index: word_to_u64(&l.topics[1])?,
            account: word_to_addr(&l.topics[2])?,
            sender: word_to_addr(&l.topics[3])?,
            amount: word_to_u64(&l.data[..32])?,
            head: h(&l.data[32..64]),
            l2_block: l.block_number,
            l2_block_hash: l.block_hash,
            l2_tx_hash: l.tx_hash,
            log_index: l.log_index,
        }))
    } else if t0 == keccak(cfg.forced_sig.as_bytes()) {
        if l.topics.len() != 3 || l.data.len() != 160 {
            return Err("ForcedRequest log shape".into());
        }
        let k = word_to_u64(&l.data[..32])?;
        let kind = ForcedKind::from_vault(k).ok_or("unknown forced kind")?;
        let market_id = u32::try_from(word_to_u64(&l.data[32..64])?).map_err(|_| "forced marketId")?;
        let amount = word_to_u64(&l.data[64..96])?;
        let deadline = word_to_u64(&l.data[96..128])?;
        let arg = match kind {
            ForcedKind::CancelAll => 0,
            ForcedKind::Close => market_id as u64,
            ForcedKind::Withdraw => amount,
        };
        Ok(Event::Forced(ForcedEvent {
            index: word_to_u64(&l.topics[1])?,
            account: word_to_addr(&l.topics[2])?,
            kind,
            deadline,
            arg,
            market_id,
            amount,
            head: h(&l.data[128..160]),
            l2_block: l.block_number,
            l2_block_hash: l.block_hash,
            l2_tx_hash: l.tx_hash,
            log_index: l.log_index,
        }))
    } else {
        Err(format!("unexpected topic {}", to_hex(&t0)))
    }
}

/// One poll; on success the view advances, on any failure it is unchanged except for the error fields.
pub async fn poll_once(cfg: &WatcherConfig, view: &SharedView) -> Result<bool, String> {
    let r = poll_inner(cfg, view).await;
    let mut v = view.lock().unwrap();
    v.polls += 1;
    match &r {
        Ok(_) => {
            v.endpoints_agree = true;
            v.last_error = None;
        }
        Err(e) => {
            v.endpoints_agree = false;
            v.disagreements += 1;
            v.last_error = Some(e.clone());
        }
    }
    r
}

async fn poll_inner(cfg: &WatcherConfig, view: &SharedView) -> Result<bool, String> {
    let [a, b] = &cfg.endpoints;
    for e in [a, b] {
        let c = chain_id(e).await?;
        if c != L2_CHAIN_ID {
            return Err(format!("endpoint {e} reports chain {c}, not {L2_CHAIN_ID}"));
        }
    }
    let (na, _) = block_num_hash(a, serde_json::json!("finalized")).await?;
    let (nb, _) = block_num_hash(b, serde_json::json!("finalized")).await?;
    let f = na.min(nb);
    let (fa, ha) = block_num_hash(a, serde_json::json!(qty(f))).await?;
    let (fb, hb) = block_num_hash(b, serde_json::json!(qty(f))).await?;
    if fa != f || fb != f || ha != hb {
        return Err(format!("endpoints disagree on the hash of finalized block {f}"));
    }
    let (next, prev_dep, prev_forced) = {
        let v = view.lock().unwrap();
        let next = if v.scanned_to == 0 && v.deposits.is_empty() && v.forced.is_empty() { cfg.start_block } else { v.scanned_to + 1 };
        (
            next,
            v.deposits.values().next_back().map(|d| (d.index, d.head)),
            v.forced.values().next_back().map(|f| (f.index, f.head)),
        )
    };
    if f < next {
        let mut v = view.lock().unwrap();
        if f >= v.finalized_block {
            v.finalized_block = f;
            v.finalized_hash = Some(ha);
        }
        return Ok(false);
    }
    let to = f.min(next.saturating_add(cfg.max_range - 1));
    let topics = serde_json::json!([[to_hex(&keccak(cfg.deposit_sig.as_bytes())), to_hex(&keccak(cfg.forced_sig.as_bytes()))]]);
    let filter = serde_json::json!([{"fromBlock": qty(next), "toBlock": qty(to), "address": to_hex(&cfg.vault), "topics": topics}]);
    let la = parse_logs(&rpc(a, "eth_getLogs", filter.clone()).await?, &cfg.vault, next, to)?;
    let lb = parse_logs(&rpc(b, "eth_getLogs", filter).await?, &cfg.vault, next, to)?;
    if la != lb {
        return Err(format!("endpoints return different logs for blocks {next}..{to}"));
    }
    // every block that carries a log: both endpoints give the same hash, and it is the log's blockHash
    let mut checked: BTreeMap<u64, H256> = BTreeMap::new();
    for l in &la {
        if let Some(h) = checked.get(&l.block_number) {
            if *h != l.block_hash {
                return Err("two logs of one block carry different block hashes".into());
            }
            continue;
        }
        let (_, x) = block_num_hash(a, serde_json::json!(qty(l.block_number))).await?;
        let (_, y) = block_num_hash(b, serde_json::json!(qty(l.block_number))).await?;
        if x != y || x != l.block_hash {
            return Err(format!("block {} hash differs between endpoints or from its logs", l.block_number));
        }
        checked.insert(l.block_number, x);
    }
    // if the range ends below F, the boundary block `to` must also agree (pin of the scanned prefix)
    if to < f {
        let (_, x) = block_num_hash(a, serde_json::json!(qty(to))).await?;
        let (_, y) = block_num_hash(b, serde_json::json!(qty(to))).await?;
        if x != y {
            return Err(format!("endpoints disagree on block {to}"));
        }
    }
    let mut deps = Vec::new();
    let mut forced = Vec::new();
    let mut nd = prev_dep.map(|x| x.0 + 1).unwrap_or(0);
    let mut dh = prev_dep.map(|x| x.1).unwrap_or_else(|| deposit_head0(L2_CHAIN_ID, &cfg.vault));
    let mut nf = prev_forced.map(|x| x.0 + 1).unwrap_or(0);
    let mut fh = prev_forced.map(|x| x.1).unwrap_or_else(|| forced_head0(L2_CHAIN_ID, &cfg.vault));
    for l in &la {
        match decode_event(cfg, l)? {
            Event::Deposit(d) => {
                if d.index != nd {
                    return Err(format!("deposit index {} where {nd} expected (gap or repeat): stop", d.index));
                }
                if d.amount == 0 || d.account == [0u8; 20] {
                    return Err("deposit with zero amount or account".into());
                }
                let want = deposit_head_next(&dh, d.index, &d.account, d.amount);
                if want != d.head {
                    return Err(format!("deposit {}: event head does not chain (VAULT.md 5.4)", d.index));
                }
                dh = want;
                nd += 1;
                deps.push(d);
            }
            Event::Forced(x) => {
                if x.index != nf {
                    return Err(format!("forced index {} where {nf} expected: stop", x.index));
                }
                let want = forced_head_next(&fh, x.index, &x.account, x.kind.vault_code(), x.market_id, x.amount, x.deadline);
                if want != x.head {
                    return Err(format!("forced request {}: event head does not chain", x.index));
                }
                fh = want;
                nf += 1;
                forced.push(x);
            }
        }
    }
    let mut v = view.lock().unwrap();
    for d in deps {
        v.deposits.insert(d.index, d);
    }
    for x in forced {
        v.forced.insert(x.index, x);
    }
    v.scanned_to = to;
    v.finalized_block = f;
    v.finalized_hash = Some(ha);
    Ok(true)
}

pub async fn run(cfg: WatcherConfig, view: SharedView, stop: tokio::sync::watch::Receiver<bool>) {
    loop {
        if *stop.borrow() {
            return;
        }
        let _ = poll_once(&cfg, &view).await;
        tokio::time::sleep(Duration::from_millis(cfg.poll_ms)).await;
    }
}

//! Snapshots (SPEC section 16, format version 1): the canonical serialization of the WHOLE engine state, so a node
//! recovers without replaying the log from genesis. UNAUDITED PROTOTYPE.
//!
//! Rules: one encoding per state (ascending keys, fixed widths, big-endian); the decoder is strict (S-0.4): it
//! refuses a wrong magic, version, genesis, checksum, a short or over-long input, a non-0/1 flag, an unknown
//! status, keys out of order, dangling references, and any state whose re-encoding is not byte-identical or whose
//! structural invariants (section 8) do not hold. Nothing is trusted because it parsed: `restore` rebuilds every
//! index from the primary data and the caller compares the state root with one it already trusts.
//!
//! Not in a snapshot (derived at load): the genesis (bound by its hash), the EIP-712 domain separator, the
//! address -> account index, the agent -> owner index, each account's order list, each market's trigger set, the
//! withdrawal ids (their positions), the events of the last block.
use crate::book::Book;
use crate::engine::{Account, Agent, Engine, ForcedReq, Market, Order, Position, WithdrawalRecord, FIRST_USER, NONCE_SET};
use crate::genesis::Checked;
use crate::hash::{keccak, Addr, H256};
use crate::oracle::{HaltReason, OracleState, Status};
use std::collections::{BTreeMap, BTreeSet, VecDeque};

pub const MAGIC: &[u8; 8] = b"PPSNAP\x00\x00";
pub const VERSION: u16 = 1;
/// Smallest possible snapshot: header (8 + 2 + 32) and checksum (32).
const MIN_LEN: usize = 8 + 2 + 32 + 32;

// Magnitude bounds checked at load BEFORE any arithmetic on loaded values (S-0.2: the engine never panics on input
// data; release builds keep overflow checks, so an unchecked forged magnitude would stop the process). Each is far
// above anything a real state reaches and low enough that every sum and product of `invariants::check`,
// `state_root` and the next `apply_block` fits i128.
/// Counters (`height`, `time`, order / deposit / forced counters): they are incremented, so not near u64::MAX.
const MAX_COUNTER: u64 = 1 << 62;
/// Sum of |balance| over all accounts, USDC units (10^21 USDC); times 10^10 still fits i128.
const MAX_TOTAL_BALANCE: i128 = 1_000_000_000_000_000_000_000_000_000;
/// |openNotional| of one position: MAX_PRICE x MAX_SIZE (SPEC section 1).
const MAX_OPEN_NOTIONAL: i128 = 1_000_000_000_000_000_000_000_000_000_000;
/// Sum of |openNotional| over all positions.
const MAX_TOTAL_NOTIONAL: i128 = 1_000_000_000_000_000_000_000_000_000_000_000_000;
/// |premium sample| <= MAX_PRICE x RATE_SCALE; |cumulative funding| and a day's volume: generous fixed bounds.
const MAX_PREMIUM: i128 = 1_000_000_000_000_000_000_000_000;
const MAX_CUM_FUNDING: i128 = 1_000_000_000_000_000_000_000_000_000_000;
const MAX_DAY_VOLUME: u128 = 1_000_000_000_000_000_000_000_000_000_000;

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SnapshotError {
    /// Not a snapshot at all (length or magic).
    Format(&'static str),
    /// A format version this engine does not read.
    Version(u16),
    /// The trailing keccak256 does not match the bytes: damaged or altered.
    Checksum,
    /// Taken under another genesis.
    Genesis,
    /// Bytes that parse but are not the canonical encoding of a valid state.
    Invalid(String),
    /// The restored state's root is not the root the caller trusts.
    Root { expected: H256, got: H256 },
}

impl std::fmt::Display for SnapshotError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            SnapshotError::Format(m) => write!(f, "not a snapshot: {m}"),
            SnapshotError::Version(v) => write!(f, "snapshot format version {v} is not supported (this engine reads {VERSION})"),
            SnapshotError::Checksum => write!(f, "snapshot checksum mismatch"),
            SnapshotError::Genesis => write!(f, "snapshot of another genesis"),
            SnapshotError::Invalid(m) => write!(f, "invalid snapshot: {m}"),
            SnapshotError::Root { expected, got } => write!(f, "snapshot state root 0x{} is not the expected 0x{}", hex::encode(got), hex::encode(expected)),
        }
    }
}

struct W(Vec<u8>);

impl W {
    fn u8(&mut self, v: u8) {
        self.0.push(v);
    }
    fn flag(&mut self, v: bool) {
        self.0.push(v as u8);
    }
    fn u16(&mut self, v: u16) {
        self.0.extend_from_slice(&v.to_be_bytes());
    }
    fn u32(&mut self, v: u32) {
        self.0.extend_from_slice(&v.to_be_bytes());
    }
    fn u64(&mut self, v: u64) {
        self.0.extend_from_slice(&v.to_be_bytes());
    }
    fn i64(&mut self, v: i64) {
        self.0.extend_from_slice(&v.to_be_bytes());
    }
    fn u128(&mut self, v: u128) {
        self.0.extend_from_slice(&v.to_be_bytes());
    }
    fn i128(&mut self, v: i128) {
        self.0.extend_from_slice(&v.to_be_bytes());
    }
    fn bytes(&mut self, v: &[u8]) {
        self.0.extend_from_slice(v);
    }
    fn count(&mut self, n: usize) {
        self.u32(u32::try_from(n).expect("count fits u32"));
    }
}

struct R<'a> {
    b: &'a [u8],
    pos: usize,
}

type Res<T> = Result<T, SnapshotError>;

fn inv<T>(m: impl Into<String>) -> Res<T> {
    Err(SnapshotError::Invalid(m.into()))
}

impl<'a> R<'a> {
    fn take(&mut self, n: usize) -> Res<&'a [u8]> {
        if self.b.len() - self.pos < n {
            return inv("short");
        }
        let s = &self.b[self.pos..self.pos + n];
        self.pos += n;
        Ok(s)
    }
    fn u8(&mut self) -> Res<u8> {
        Ok(self.take(1)?[0])
    }
    fn flag(&mut self) -> Res<bool> {
        match self.u8()? {
            0 => Ok(false),
            1 => Ok(true),
            _ => inv("flag not 0 or 1"),
        }
    }
    fn u16(&mut self) -> Res<u16> {
        Ok(u16::from_be_bytes(self.take(2)?.try_into().unwrap()))
    }
    fn u32(&mut self) -> Res<u32> {
        Ok(u32::from_be_bytes(self.take(4)?.try_into().unwrap()))
    }
    fn u64(&mut self) -> Res<u64> {
        Ok(u64::from_be_bytes(self.take(8)?.try_into().unwrap()))
    }
    fn i64(&mut self) -> Res<i64> {
        Ok(i64::from_be_bytes(self.take(8)?.try_into().unwrap()))
    }
    fn u128(&mut self) -> Res<u128> {
        Ok(u128::from_be_bytes(self.take(16)?.try_into().unwrap()))
    }
    fn i128(&mut self) -> Res<i128> {
        Ok(i128::from_be_bytes(self.take(16)?.try_into().unwrap()))
    }
    fn addr(&mut self) -> Res<Addr> {
        Ok(self.take(20)?.try_into().unwrap())
    }
    fn h256(&mut self) -> Res<H256> {
        Ok(self.take(32)?.try_into().unwrap())
    }
    /// A count whose elements take at least `min_each` bytes: refused when the remaining input cannot hold it, so
    /// a forged count never drives an allocation.
    fn count(&mut self, min_each: usize) -> Res<usize> {
        let n = self.u32()? as usize;
        match n.checked_mul(min_each.max(1)) {
            Some(need) if need <= self.b.len() - self.pos => Ok(n),
            _ => inv("count larger than the input"),
        }
    }
}

fn status_byte(s: Status) -> u8 {
    match s {
        Status::Active => 0,
        Status::Halted(HaltReason::Stale) => 1,
        Status::Halted(HaltReason::Jump) => 2,
        Status::Halted(HaltReason::Arithmetic) => 3,
    }
}

fn write_side(w: &mut W, side: &BTreeMap<u64, VecDeque<u64>>) {
    w.count(side.len());
    for (price, q) in side {
        w.u64(*price);
        w.count(q.len());
        for id in q {
            w.u64(*id);
        }
    }
}

fn read_side(r: &mut R) -> Res<BTreeMap<u64, VecDeque<u64>>> {
    let n = r.count(8 + 4 + 8)?;
    let mut side = BTreeMap::new();
    let mut prev: Option<u64> = None;
    for _ in 0..n {
        let price = r.u64()?;
        if prev.is_some_and(|p| price <= p) {
            return inv("book levels not ascending");
        }
        prev = Some(price);
        let k = r.count(8)?;
        if k == 0 {
            return inv("empty book level");
        }
        let mut q = VecDeque::with_capacity(k);
        for _ in 0..k {
            q.push_back(r.u64()?);
        }
        side.insert(price, q);
    }
    Ok(side)
}

impl Engine {
    /// The canonical snapshot of this state (SPEC 16). Deterministic: equal states give equal bytes.
    pub fn snapshot(&self) -> Vec<u8> {
        let mut w = W(Vec::with_capacity(4096));
        w.bytes(MAGIC);
        w.u16(VERSION);
        w.bytes(&self.g.hash);
        // --- scalars
        w.u64(self.height);
        w.u64(self.time);
        w.bytes(&self.log_hash);
        w.u64(self.next_order_id);
        w.u64(self.next_deposit);
        w.u64(self.next_forced);
        w.u64(self.forced_processed);
        w.bytes(&self.deposits_head);
        w.bytes(&self.forced_head);
        w.i128(self.dust);
        match self.last_oracle_round {
            Some(x) => {
                w.flag(true);
                w.u64(x);
            }
            None => w.flag(false),
        }
        w.i128(self.total_deposits);
        w.i128(self.total_withdrawn);
        w.flag(self.launched);
        // --- accounts, in index order
        w.count(self.accounts.len());
        for a in &self.accounts {
            w.bytes(&a.address);
            w.i128(a.balance);
            w.count(a.positions.len());
            for (mid, p) in &a.positions {
                w.u32(*mid);
                w.i64(p.size);
                w.i128(p.open_notional);
            }
            w.count(a.leverage.len());
            for (mid, l) in &a.leverage {
                w.u32(*mid);
                w.u32(*l);
            }
            w.count(a.agents.len());
            for (ag, x) in &a.agents {
                w.bytes(ag);
                w.u16(x.name.len() as u16);
                w.bytes(x.name.as_bytes());
                w.u64(x.expires_at);
            }
            w.count(a.volume.len());
            for (day, v) in &a.volume {
                w.u64(*day);
                w.u128(*v);
            }
        }
        // --- markets, ascending id (the ids are the genesis ids)
        w.count(self.markets.len());
        for (mid, m) in &self.markets {
            w.u32(*mid);
            w.u64(m.oracle.index);
            w.u64(m.oracle.index_time);
            w.u8(status_byte(m.oracle.status));
            match m.oracle.run {
                Some((last, count, t)) => {
                    w.flag(true);
                    w.u64(last);
                    w.u32(count);
                    w.u64(t);
                }
                None => w.flag(false),
            }
            w.u64(m.oracle.halt_low);
            w.u64(m.oracle.halt_high);
            w.u64(m.mark);
            match m.last_trade {
                Some((p, t)) => {
                    w.flag(true);
                    w.u64(p);
                    w.u64(t);
                }
                None => w.flag(false),
            }
            w.u64(m.long_oi);
            w.count(m.premium_samples.len());
            for x in &m.premium_samples {
                w.i128(*x);
            }
            w.i128(m.cum_funding);
            // time priority inside a level is the queue order: it cannot be derived from the order ids (a fired
            // trigger keeps its id and joins the queue later), so the queues are stored
            write_side(&mut w, &m.book.bids);
            write_side(&mut w, &m.book.asks);
        }
        // --- orders (resting and triggers), ascending id
        w.count(self.orders.len());
        for (id, o) in &self.orders {
            w.u64(*id);
            w.u32(o.account);
            w.u32(o.market);
            w.flag(o.is_buy);
            w.u64(o.price);
            w.u64(o.remaining);
            w.flag(o.reduce_only);
            w.bytes(&o.client_order_id);
            match o.trigger {
                Some((ty, tp)) => {
                    w.flag(true);
                    w.u8(ty);
                    w.u64(tp);
                }
                None => w.flag(false),
            }
            w.u64(o.height);
        }
        // --- withdrawal records, id = position
        w.count(self.withdrawals.len());
        for r in &self.withdrawals {
            w.bytes(&r.account);
            w.bytes(&r.recipient);
            w.u64(r.amount);
            w.u64(r.height);
        }
        // --- forced requests received and not processed, in index order
        w.count(self.forced_queue.len());
        for r in &self.forced_queue {
            w.u64(r.index);
            w.bytes(&r.account);
            w.u8(r.kind);
            w.u64(r.deadline);
            w.u64(r.arg);
        }
        // --- nonce sets, ascending signer
        w.count(self.nonces.len());
        for (s, set) in &self.nonces {
            w.bytes(s);
            w.count(set.len());
            for n in set {
                w.u64(*n);
            }
        }
        let sum = keccak(&w.0);
        w.bytes(&sum);
        w.0
    }

    /// Load a snapshot taken under genesis `g`. With `expected_root` the restored state's root must equal it
    /// (`SnapshotError::Root` otherwise): a node passes the root of its own log or of a finalized checkpoint, so a
    /// snapshot is never trusted on its checksum alone (the checksum detects damage, not forgery).
    pub fn restore(g: Checked, bytes: &[u8], expected_root: Option<&H256>) -> Result<Engine, SnapshotError> {
        if bytes.len() < MIN_LEN {
            return Err(SnapshotError::Format("too short"));
        }
        if &bytes[..8] != MAGIC {
            return Err(SnapshotError::Format("magic"));
        }
        let version = u16::from_be_bytes([bytes[8], bytes[9]]);
        if version != VERSION {
            return Err(SnapshotError::Version(version));
        }
        let (body, sum) = bytes.split_at(bytes.len() - 32);
        if keccak(body) != sum {
            return Err(SnapshotError::Checksum);
        }
        if body[10..42] != g.hash {
            return Err(SnapshotError::Genesis);
        }
        let mut r = R { b: body, pos: 42 };
        let mut e = Engine::new(g);
        e.height = r.u64()?;
        e.time = r.u64()?;
        e.log_hash = r.h256()?;
        e.next_order_id = r.u64()?;
        e.next_deposit = r.u64()?;
        e.next_forced = r.u64()?;
        e.forced_processed = r.u64()?;
        e.deposits_head = r.h256()?;
        e.forced_head = r.h256()?;
        e.dust = r.i128()?;
        e.last_oracle_round = if r.flag()? { Some(r.u64()?) } else { None };
        e.total_deposits = r.i128()?;
        e.total_withdrawn = r.i128()?;
        e.launched = r.flag()?;
        if e.next_order_id == 0 {
            return inv("nextOrderId 0");
        }
        if [e.height, e.time, e.next_order_id, e.next_deposit, e.next_forced].iter().any(|x| *x > MAX_COUNTER) || e.forced_processed > e.next_forced {
            return inv("counter out of range");
        }
        if e.dust < 0 || e.dust >= crate::num::N_PER_USDC {
            return inv("dust out of range (whole units are swept every block)");
        }
        if e.total_deposits < 0 || e.total_deposits > MAX_TOTAL_BALANCE || e.total_withdrawn < 0 || e.total_withdrawn > u64::MAX as i128 || e.total_withdrawn > e.total_deposits {
            return inv("deposit / withdrawal totals out of range");
        }
        let (mut sum_balance, mut sum_notional): (i128, i128) = (0, 0);
        // --- accounts
        let n = r.count(20 + 16 + 16)?;
        if n < FIRST_USER as usize {
            return inv("system accounts missing");
        }
        let mut accounts: Vec<Account> = Vec::with_capacity(n);
        let mut by_addr: BTreeMap<Addr, u32> = BTreeMap::new();
        let mut agent_owner: BTreeMap<Addr, u32> = BTreeMap::new();
        for i in 0..n {
            let address = r.addr()?;
            if i < FIRST_USER as usize && address != crate::engine::system_address(i as u32) {
                return inv("system account address");
            }
            if by_addr.insert(address, i as u32).is_some() {
                return inv("duplicate account address");
            }
            let balance = r.i128()?;
            sum_balance = match balance.checked_abs().and_then(|b| sum_balance.checked_add(b)) {
                Some(x) if x <= MAX_TOTAL_BALANCE => x,
                _ => return inv("balances out of range"),
            };
            let mut a = Account { address, balance, positions: BTreeMap::new(), leverage: BTreeMap::new(), orders: BTreeSet::new(), agents: BTreeMap::new(), volume: BTreeMap::new() };
            let k = r.count(4 + 8 + 16)?;
            let mut prev: Option<u32> = None;
            for _ in 0..k {
                let mid = r.u32()?;
                if prev.is_some_and(|p| mid <= p) || !e.markets.contains_key(&mid) {
                    return inv("position market ids");
                }
                prev = Some(mid);
                let size = r.i64()?;
                let open_notional = r.i128()?;
                if size == 0 || size.unsigned_abs() > crate::num::MAX_SIZE {
                    return inv("position size");
                }
                sum_notional = match open_notional.checked_abs().filter(|n| *n <= MAX_OPEN_NOTIONAL).and_then(|n| sum_notional.checked_add(n)) {
                    Some(x) if x <= MAX_TOTAL_NOTIONAL => x,
                    _ => return inv("open notional out of range"),
                };
                a.positions.insert(mid, Position { size, open_notional });
            }
            let k = r.count(8)?;
            let mut prev: Option<u32> = None;
            for _ in 0..k {
                let mid = r.u32()?;
                if prev.is_some_and(|p| mid <= p) {
                    return inv("leverage market ids");
                }
                prev = Some(mid);
                let lev = r.u32()?;
                match e.markets.get(&mid) {
                    Some(m) if lev >= 1 && lev <= m.p.max_leverage => {}
                    _ => return inv("leverage setting"),
                }
                a.leverage.insert(mid, lev);
            }
            let k = r.count(20 + 2 + 8)?;
            let mut prev: Option<Addr> = None;
            for _ in 0..k {
                let ag = r.addr()?;
                if prev.is_some_and(|p| ag <= p) {
                    return inv("agents not ascending");
                }
                prev = Some(ag);
                let len = r.u16()? as usize;
                if len > 32 {
                    return inv("agent name longer than 32 bytes");
                }
                let name = match std::str::from_utf8(r.take(len)?) {
                    Ok(s) => s.to_string(),
                    Err(_) => return inv("agent name utf-8"),
                };
                let expires_at = r.u64()?;
                if agent_owner.insert(ag, i as u32).is_some() {
                    return inv("agent of two accounts");
                }
                a.agents.insert(ag, Agent { name, expires_at });
            }
            if a.agents.len() > crate::engine::MAX_AGENTS {
                return inv("too many agents");
            }
            let k = r.count(8 + 16)?;
            let mut prev: Option<u64> = None;
            for _ in 0..k {
                let day = r.u64()?;
                if prev.is_some_and(|p| day <= p) {
                    return inv("volume days not ascending");
                }
                prev = Some(day);
                let vol = r.u128()?;
                if vol > MAX_DAY_VOLUME {
                    return inv("volume out of range");
                }
                a.volume.insert(day, vol);
            }
            accounts.push(a);
        }
        // --- markets
        let n = r.count(4)?;
        if n != e.markets.len() {
            return inv("market count differs from the genesis");
        }
        let ids: Vec<u32> = e.markets.keys().copied().collect();
        for want in ids {
            let mid = r.u32()?;
            if mid != want {
                return inv("market ids differ from the genesis");
            }
            let index = r.u64()?;
            let index_time = r.u64()?;
            let status = match r.u8()? {
                0 => Status::Active,
                1 => Status::Halted(HaltReason::Stale),
                2 => Status::Halted(HaltReason::Jump),
                3 => Status::Halted(HaltReason::Arithmetic),
                _ => return inv("market status"),
            };
            let run = if r.flag()? { Some((r.u64()?, r.u32()?, r.u64()?)) } else { None };
            let halt_low = r.u64()?;
            let halt_high = r.u64()?;
            let mark = r.u64()?;
            let last_trade = if r.flag()? { Some((r.u64()?, r.u64()?)) } else { None };
            let long_oi = r.u64()?;
            let k = r.count(16)?;
            let mut premium_samples = Vec::with_capacity(k);
            for _ in 0..k {
                premium_samples.push(r.i128()?);
            }
            let cum_funding = r.i128()?;
            let maxp = crate::num::MAX_PRICE;
            if index > maxp || mark > maxp || halt_low > maxp || halt_high > maxp || run.is_some_and(|x| x.0 > maxp) || last_trade.is_some_and(|x| x.0 > maxp) {
                return inv("market price above MAX_PRICE");
            }
            if premium_samples.iter().any(|x| x.checked_abs().is_none_or(|v| v > MAX_PREMIUM)) || cum_funding.checked_abs().is_none_or(|v| v > MAX_CUM_FUNDING) {
                return inv("funding state out of range");
            }
            let bids = read_side(&mut r)?;
            let asks = read_side(&mut r)?;
            let m: &mut Market = e.markets.get_mut(&mid).unwrap();
            m.oracle = OracleState { index, index_time, status, run, halt_low, halt_high };
            m.mark = mark;
            m.last_trade = last_trade;
            m.long_oi = long_oi;
            m.premium_samples = premium_samples;
            m.cum_funding = cum_funding;
            m.book = Book { bids, asks };
        }
        // --- orders
        let n = r.count(8 + 4 + 4 + 1 + 8 + 8 + 1 + 32 + 1 + 8)?;
        let mut prev: Option<u64> = None;
        for _ in 0..n {
            let id = r.u64()?;
            if prev.is_some_and(|p| id <= p) || id == 0 || id >= e.next_order_id {
                return inv("order ids");
            }
            prev = Some(id);
            let account = r.u32()?;
            let market = r.u32()?;
            let is_buy = r.flag()?;
            let price = r.u64()?;
            let remaining = r.u64()?;
            let reduce_only = r.flag()?;
            let client_order_id = r.h256()?;
            let trigger = if r.flag()? { Some((r.u8()?, r.u64()?)) } else { None };
            let height = r.u64()?;
            if (account as usize) >= accounts.len() || account < FIRST_USER {
                return inv("order of an unknown or system account");
            }
            if accounts[account as usize].orders.len() >= crate::engine::MAX_ORDERS_TOTAL {
                return inv("more orders than MAX_ORDERS_TOTAL for one account");
            }
            let Some(m) = e.markets.get_mut(&market) else { return inv("order in an unknown market") };
            if remaining == 0 || remaining > crate::num::MAX_SIZE || price == 0 || price > crate::num::MAX_PRICE || height > e.height {
                return inv("order fields");
            }
            if let Some((ty, tp)) = trigger {
                if !(crate::types::ORDER_STOP_MARKET..=crate::types::ORDER_TP_LIMIT).contains(&ty) || tp == 0 || tp > crate::num::MAX_PRICE {
                    return inv("trigger fields");
                }
                m.triggers.insert(id);
            }
            accounts[account as usize].orders.insert(id);
            e.orders.insert(id, Order { id, account, market, is_buy, price, remaining, reduce_only, client_order_id, trigger, height });
        }
        // every id in a book queue is a resting order, once
        let mut seen: BTreeSet<u64> = BTreeSet::new();
        for m in e.markets.values() {
            for q in m.book.bids.values().chain(m.book.asks.values()) {
                for id in q {
                    if !seen.insert(*id) {
                        return inv("order twice in a book");
                    }
                }
            }
        }
        // --- withdrawals
        let n = r.count(20 + 20 + 8 + 8)?;
        for id in 0..n {
            let account = r.addr()?;
            let recipient = r.addr()?;
            let amount = r.u64()?;
            let height = r.u64()?;
            if height > e.height || height == 0 {
                return inv("withdrawal height");
            }
            e.withdrawals.push(WithdrawalRecord { id: id as u64, account, recipient, amount, height });
        }
        // --- forced queue
        let n = r.count(8 + 20 + 1 + 8 + 8)?;
        for _ in 0..n {
            e.forced_queue.push_back(ForcedReq { index: r.u64()?, account: r.addr()?, kind: r.u8()?, deadline: r.u64()?, arg: r.u64()? });
        }
        // --- nonces
        let n = r.count(20 + 4)?;
        let mut prev: Option<Addr> = None;
        for _ in 0..n {
            let s = r.addr()?;
            if prev.is_some_and(|p| s <= p) {
                return inv("nonce signers not ascending");
            }
            prev = Some(s);
            let k = r.count(8)?;
            if k == 0 || k > NONCE_SET {
                return inv("nonce set size");
            }
            let mut set = BTreeSet::new();
            let mut pn: Option<u64> = None;
            for _ in 0..k {
                let x = r.u64()?;
                if pn.is_some_and(|p| x <= p) {
                    return inv("nonces not ascending");
                }
                pn = Some(x);
                set.insert(x);
            }
            e.nonces.insert(s, set);
        }
        if r.pos != body.len() {
            return inv("trailing bytes");
        }
        e.accounts = accounts;
        e.by_addr = by_addr;
        e.agent_owner = agent_owner;
        e.last_passes = (0, true);
        // structural invariants of section 8 (I-1, I-2, I-3, book and order cross-references, forced queue)
        if let Err(m) = crate::invariants::check(&e) {
            return inv(format!("invariant: {m}"));
        }
        // one encoding per state: whatever parsed must re-encode to exactly these bytes
        if e.snapshot() != bytes {
            return inv("non-canonical encoding");
        }
        if let Some(want) = expected_root {
            let got = e.state_root();
            if got != *want {
                return Err(SnapshotError::Root { expected: *want, got });
            }
        }
        Ok(e)
    }
}

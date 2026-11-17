//! The deterministic exchange state machine (SPEC sections 3 to 9, 11). UNAUDITED PROTOTYPE.
//!
//! Every map is a `BTreeMap`/`BTreeSet` (defined iteration order, S-0.1). Bounds of SPEC section 1 and the open
//! interest cap (genesis `oiCap <= MAX_SIZE`) guarantee that every product below fits in i128; release builds keep
//! `overflow-checks = true`, so a violated bound stops the process instead of wrapping.
use crate::book::Book;
use crate::codec::{encode_inputs, log_hash};
use crate::eip712::{digest, domain_separator, recover, struct_hash};
use crate::genesis::{Checked, MarketParams, Tier};
use crate::hash::{keccak, keccak_parts, Addr, H256};
use crate::merkle;
use crate::num::*;
use crate::oracle::{committee_prices, HaltReason, OracleState, Status};
use crate::types::*;
use std::collections::{BTreeMap, BTreeSet, VecDeque};

pub const MAX_INPUTS: usize = 10_000;
pub const MAX_BLOCK_BYTES: usize = 4 << 20;
pub const NONCE_SET: usize = 64;
pub const NONCE_PAST_MS: u64 = 120_000;
pub const NONCE_FUTURE_MS: u64 = 30_000;
pub const MAX_AGENTS: usize = 4;
pub const MAX_AGENT_TTL_MS: u64 = 180 * 86_400_000;
pub const MAX_ORDERS_PER_MARKET: usize = 200;
pub const MAX_ORDERS_TOTAL: usize = 1000;
pub const MAX_SYSTEM_PASSES: usize = 8;
pub const MARK_TRADE_MAX_AGE_MS: u64 = 60_000;
pub const DAY_MS: u64 = 86_400_000;
pub const ACCT_PROTOCOL_FEE: u32 = 0;
pub const ACCT_INSURANCE: u32 = 1;
pub const ACCT_BACKSTOP: u32 = 2;
pub const FIRST_USER: u32 = 3;
pub const ACCOUNT_DEPTH: usize = 32;
pub const MARKET_DEPTH: usize = 8;
pub const WITHDRAWAL_DEPTH: usize = 32;

/// System accounts have no key: addresses 0x…01, 0x…02, 0x…03 (SPEC section 8).
pub fn system_address(i: u32) -> Addr {
    let mut a = [0u8; 20];
    a[19] = (i + 1) as u8;
    a
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Position {
    pub size: i64,
    /// Signed sum of size x price of the open part, notional units.
    pub open_notional: i128,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Agent {
    pub name: String,
    pub expires_at: u64,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Account {
    pub address: Addr,
    /// Cross collateral, USDC units. Signed: a negative balance is bad debt (recorded, never hidden).
    pub balance: i128,
    pub positions: BTreeMap<u32, Position>,
    pub leverage: BTreeMap<u32, u32>,
    pub orders: BTreeSet<u64>,
    pub agents: BTreeMap<Addr, Agent>,
    /// Traded notional per UTC day (USDC units, floor), kept for the last 15 days (fee tiers, S-9.4).
    pub volume: BTreeMap<u64, u128>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Order {
    pub id: u64,
    pub account: u32,
    pub market: u32,
    pub is_buy: bool,
    pub price: u64,
    pub remaining: u64,
    pub reduce_only: bool,
    pub client_order_id: H256,
    /// `Some((orderType, triggerPrice))` while the order waits for its trigger (S-4.6).
    pub trigger: Option<(u8, u64)>,
    /// Block in which the order was placed (R395-13: liquidations never fill against same-block orders).
    pub height: u64,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Market {
    pub p: MarketParams,
    pub book: Book,
    pub triggers: BTreeSet<u64>,
    pub oracle: OracleState,
    pub mark: u64,
    pub last_trade: Option<(u64, u64)>,
    pub long_oi: u64,
    /// Premium samples of the current funding interval (RATE_SCALE), taken after the block's actions (R395-16).
    pub premium_samples: Vec<i128>,
    pub cum_funding: i128,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct WithdrawalRecord {
    pub id: u64,
    pub account: Addr,
    pub recipient: Addr,
    pub amount: u64,
    pub height: u64,
}

/// A forced request received from the vault and not yet processed (SPEC section 14).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ForcedReq {
    pub index: u64,
    pub account: Addr,
    pub kind: u8,
    pub deadline: u64,
    pub arg: u64,
}

enum ForcedResult {
    Served,
    /// Refused for a reason provable from finalized state that the user caused: consumed.
    Rejected(Error),
    /// Not servable now for a reason the user does not control (halt, uncredited deposit): stays at the head.
    Pending(Error),
}

/// abi.encode(bytes32 text) word: ASCII left-aligned, zero-padded (VAULT.md 5.4).
fn text_word(t: &str) -> H256 {
    let mut w = [0u8; 32];
    w[..t.len()].copy_from_slice(t.as_bytes());
    w
}

/// The vault's chain heads (VAULT.md 5.4), recomputed by the engine from the inputs it APPLIED (R395-1).
pub fn chain_genesis(tag: &str, chain_id: u64, vault: &Addr) -> H256 {
    keccak_parts(&[&text_word(tag), &crate::hash::word_u(chain_id as u128), &crate::hash::word_addr(vault)])
}

pub fn deposit_link(head: &H256, i: u64, account: &Addr, amount: u64) -> H256 {
    use crate::hash::{word_addr, word_u};
    keccak_parts(&[head, &word_u(i as u128), &word_addr(account), &word_u(amount as u128)])
}

pub fn forced_link(head: &H256, r: &ForcedReq) -> H256 {
    use crate::hash::{word_addr, word_u};
    keccak_parts(&[head, &word_u(r.index as u128), &word_addr(&r.account), &word_u(r.kind as u128), &word_u(r.deadline as u128), &word_u(r.arg as u128)])
}

/// The fields of `systemHash` (SPEC 11, 0.1.5). `next_forced_index` is the number of PROCESSED forced requests
/// (VAULT.md 5.2's `nextForcedIndex`); requests received but pending are in `auxHash`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SystemFields {
    pub next_deposit_index: u64,
    pub next_forced_index: u64,
    pub next_order_id: u64,
    pub total_escape: u128,
    pub dust: u128,
    pub aux_hash: H256,
    pub withdrawal_count: u64,
    pub withdrawals_total: u64,
}

pub fn system_hash_of(f: &SystemFields) -> H256 {
    keccak_parts(&[
        b"PPv1sys",
        &f.next_deposit_index.to_be_bytes(),
        &f.next_forced_index.to_be_bytes(),
        &f.next_order_id.to_be_bytes(),
        &f.total_escape.to_be_bytes(),
        &f.dust.to_be_bytes(),
        &f.aux_hash,
        &f.withdrawal_count.to_be_bytes(),
        &f.withdrawals_total.to_be_bytes(),
    ])
}

pub fn state_root_of(height: u64, time: u64, accounts_root: &H256, markets_root: &H256, withdrawals_root: &H256, system_hash: &H256) -> H256 {
    keccak_parts(&[b"PosternPerps/state/v1", &height.to_be_bytes(), &time.to_be_bytes(), accounts_root, markets_root, withdrawals_root, system_hash])
}

/// SPEC 11 account leaf data from its fields (shared with the vault's vectors).
pub fn account_leaf_bytes(address: &Addr, index: u32, balance: i128, escape: u64, positions_hash: &H256) -> Vec<u8> {
    let mut d = Vec::with_capacity(8 + 20 + 4 + 16 + 8 + 32);
    d.extend_from_slice(b"PPv1acct");
    d.extend_from_slice(address);
    d.extend_from_slice(&index.to_be_bytes());
    d.extend_from_slice(&balance.to_be_bytes());
    d.extend_from_slice(&escape.to_be_bytes());
    d.extend_from_slice(positions_hash);
    d
}

/// The engine-side fields of a checkpoint (VAULT.md 5.2): what validators sign is COMPUTED from applied inputs,
/// never copied from an L2 read (R395-1).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CheckpointFields {
    pub deposits_credited: u64,
    pub deposits_head: H256,
    pub forced_processed: u64,
    pub forced_head: H256,
    pub withdrawal_count: u64,
    pub withdrawals_total: u128,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FillKind {
    Book,
    Liquidation,
    Backstop,
    Adl,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Event {
    Deposit { index: u64, account: Addr, amount: u64 },
    Fill { market: u32, kind: FillKind, taker: u32, maker: u32, taker_order: u64, maker_order: u64, taker_is_buy: bool, price: u64, size: u64, taker_fee: i128, maker_fee: i128 },
    Rested { order: u64 },
    TriggerStored { order: u64 },
    Triggered { order: u64 },
    Cancelled { order: u64, reason: Error },
    RemainderCancelled { order: u64, size: u64, reason: Error },
    Funding { market: u32, rate: i128 },
    Liquidated { account: u32 },
    InsuranceCover { account: u32, amount: i128 },
    LiquidationFee { account: u32, amount: i128 },
    Withdrawal(WithdrawalRecord),
    Halt { market: u32 },
    Resume { market: u32 },
    BadDebt { account: u32, amount: i128 },
    Launched,
    ForcedServed { index: u64 },
    ForcedRejected { index: u64, reason: Error },
    ForcedPending { index: u64, reason: Error },
    MarketSettled { market: u32, price: u64 },
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum BlockError {
    Height,
    Time,
    Order,
    TooLarge,
}

#[derive(Clone, Debug)]
pub struct BlockOutcome {
    pub outcomes: Vec<Result<(), Error>>,
    pub events: Vec<Event>,
    pub state_root: H256,
    pub log_hash: H256,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Mode {
    Normal,
    Liquidation,
    Forced,
}

#[derive(Clone, Debug)]
pub struct Engine {
    pub g: Checked,
    pub domain_sep: H256,
    pub height: u64,
    pub time: u64,
    pub log_hash: H256,
    pub accounts: Vec<Account>,
    pub by_addr: BTreeMap<Addr, u32>,
    pub agent_owner: BTreeMap<Addr, u32>,
    pub nonces: BTreeMap<Addr, BTreeSet<u64>>,
    pub markets: BTreeMap<u32, Market>,
    pub orders: BTreeMap<u64, Order>,
    pub next_order_id: u64,
    pub withdrawals: Vec<WithdrawalRecord>,
    pub next_deposit: u64,
    /// Forced requests received (vault index order); `forced_processed <= next_forced`.
    pub next_forced: u64,
    pub forced_processed: u64,
    pub forced_queue: VecDeque<ForcedReq>,
    pub deposits_head: H256,
    pub forced_head: H256,
    /// Rounding residuals (notional units) owed to the insurance fund (S-0.3, I-2).
    pub dust: i128,
    pub last_oracle_round: Option<u64>,
    pub total_deposits: i128,
    pub total_withdrawn: i128,
    /// Launch gate latch (S-4.11): one-way.
    pub launched: bool,
    events: Vec<Event>,
}

fn new_account(address: Addr) -> Account {
    Account { address, balance: 0, positions: BTreeMap::new(), leverage: BTreeMap::new(), orders: BTreeSet::new(), agents: BTreeMap::new(), volume: BTreeMap::new() }
}

fn abs64(x: i64) -> u64 {
    x.unsigned_abs()
}

/// Continuous piecewise requirement (SPEC 6.2): sum over tiers of (part of n in the tier) * fraction, ceil.
fn tier_req(n: i128, tiers: &[Tier], frac: impl Fn(&Tier) -> i128) -> i128 {
    if n <= 0 {
        return 0;
    }
    let mut acc: i128 = 0;
    for (i, t) in tiers.iter().enumerate() {
        let lo = (t.floor as i128) * N_PER_USDC;
        if n <= lo {
            break;
        }
        let hi = tiers.get(i + 1).map(|x| (x.floor as i128) * N_PER_USDC).unwrap_or(i128::MAX);
        let part = n.min(hi) - lo;
        acc += part * frac(t);
    }
    div_ceil(acc, PPM)
}

impl Engine {
    pub fn new(g: Checked) -> Engine {
        let domain_sep = domain_separator(g.g.chain_id, &g.verifying_contract);
        let mut markets = BTreeMap::new();
        for m in &g.g.markets {
            markets.insert(
                m.market_id,
                Market { p: m.clone(), book: Book::default(), triggers: BTreeSet::new(), oracle: OracleState::default(), mark: 0, last_trade: None, long_oi: 0, premium_samples: Vec::new(), cum_funding: 0 },
            );
        }
        let log_hash = keccak_parts(&[b"PosternPerps/genesis/v1", &g.g.encoding()]);
        let mut e = Engine {
            g,
            domain_sep,
            height: 0,
            time: 0,
            log_hash,
            accounts: Vec::new(),
            by_addr: BTreeMap::new(),
            agent_owner: BTreeMap::new(),
            nonces: BTreeMap::new(),
            markets,
            orders: BTreeMap::new(),
            next_order_id: 1,
            withdrawals: Vec::new(),
            next_deposit: 0,
            next_forced: 0,
            forced_processed: 0,
            forced_queue: VecDeque::new(),
            deposits_head: [0u8; 32],
            forced_head: [0u8; 32],
            dust: 0,
            last_oracle_round: None,
            total_deposits: 0,
            total_withdrawn: 0,
            launched: false,
            events: Vec::new(),
        };
        e.deposits_head = chain_genesis("PosternPerps.deposit.v1", e.g.g.chain_id, &e.g.verifying_contract);
        e.forced_head = chain_genesis("PosternPerps.forced.v1", e.g.g.chain_id, &e.g.verifying_contract);
        for i in 0..FIRST_USER {
            let a = system_address(i);
            e.by_addr.insert(a, i);
            e.accounts.push(new_account(a));
        }
        e
    }

    // ------------------------------------------------------------------ block

    pub fn apply_block(&mut self, b: &Block) -> Result<BlockOutcome, BlockError> {
        if b.height != self.height + 1 {
            return Err(BlockError::Height);
        }
        if b.time <= self.time {
            return Err(BlockError::Time);
        }
        if b.inputs.len() > MAX_INPUTS {
            return Err(BlockError::TooLarge);
        }
        let mut prev_tag = 0u8;
        let mut oracles = 0;
        for i in &b.inputs {
            if i.tag() < prev_tag {
                return Err(BlockError::Order);
            }
            prev_tag = i.tag();
            if i.tag() == 0x01 {
                oracles += 1;
            }
        }
        if oracles > 1 {
            return Err(BlockError::Order);
        }
        let enc = encode_inputs(&b.inputs);
        if enc.len() > MAX_BLOCK_BYTES {
            return Err(BlockError::TooLarge);
        }
        // From here on nothing fails the block.
        self.events.clear();
        let prev_t = self.time;
        self.height = b.height;
        self.time = b.time;
        let t = b.time;
        let mut outcomes: Vec<Result<(), Error>> = Vec::with_capacity(b.inputs.len());

        // 2. H1
        let ids: Vec<u32> = self.markets.keys().copied().collect();
        for id in &ids {
            let m = self.markets.get_mut(id).unwrap();
            let before = m.oracle.status;
            m.oracle.h1(t, &m.p.oracle);
            if before == Status::Active && m.oracle.status != Status::Active {
                self.events.push(Event::Halt { market: *id });
            }
        }
        // 3. oracle input
        let mut round_accepted = false;
        let mut rest = &b.inputs[..];
        if let Some(Input::Oracle(u)) = b.inputs.first() {
            let r = self.apply_oracle(u, t);
            round_accepted = r.is_ok();
            outcomes.push(r);
            rest = &b.inputs[1..];
        }
        // 4. mark
        self.update_marks();
        // 5. funding
        for id in &ids {
            let iv = self.markets[id].p.funding_interval_ms;
            if prev_t > 0 && t / iv > prev_t / iv {
                self.settle_funding(*id);
            }
        }
        // 5b. X3 settlement of markets halted longer than maxHaltMs
        self.settle_due_markets();
        // 7. deposits, forced (received, then the queue is served from its head), actions
        let mut forced_done = false;
        for i in rest {
            if !forced_done && matches!(i, Input::Action(_)) {
                self.process_forced();
                forced_done = true;
            }
            let r = match i {
                Input::Oracle(_) => unreachable!("checked: at most one oracle input, first"),
                Input::Deposit { index, account, amount } => self.apply_deposit(*index, *account, *amount),
                Input::Forced { index, account, kind, deadline, arg } => {
                    self.receive_forced(ForcedReq { index: *index, account: *account, kind: *kind, deadline: *deadline, arg: *arg })
                }
                Input::Action(sa) => self.apply_action(sa),
            };
            outcomes.push(r);
        }
        if !forced_done {
            self.process_forced();
        }
        // 8. mark again; premium sample on the POST-action book (R395-16: quotes placed as a block's last actions
        // and cancelled as the next block's first are now exposed to every action in between); 9. triggers and
        // liquidations
        self.update_marks();
        if round_accepted {
            for id in &ids {
                self.sample_premium(*id);
            }
        }
        for _ in 0..MAX_SYSTEM_PASSES {
            let a = self.trigger_pass();
            self.update_marks();
            let l = self.liquidation_pass();
            self.update_marks();
            if !a && !l {
                break;
            }
        }
        self.settle_bad_debt();
        // 10. fee sweep (day boundary)
        if prev_t > 0 && t / DAY_MS > prev_t / DAY_MS {
            self.sweep_fees();
        }
        self.sweep_dust();
        self.log_hash = log_hash(&self.log_hash, b.height, b.time, &enc);
        let state_root = self.state_root();
        Ok(BlockOutcome { outcomes, events: std::mem::take(&mut self.events), state_root, log_hash: self.log_hash })
    }

    // ------------------------------------------------------------------ oracle and mark

    fn apply_oracle(&mut self, u: &OracleUpdate, t: u64) -> Result<(), Error> {
        let mp: Vec<(u32, crate::genesis::OracleParams)> = self.markets.iter().map(|(k, m)| (*k, m.p.oracle.clone())).collect();
        let refs: Vec<(u32, &crate::genesis::OracleParams)> = mp.iter().map(|(k, p)| (*k, p)).collect();
        let (prices, tau) = committee_prices(
            u,
            t,
            self.last_oracle_round,
            &self.g.oracle_signers,
            self.g.g.oracle_quorum as usize,
            self.g.g.oracle_max_future_ms,
            self.g.g.oracle_max_age_ms,
            &self.domain_sep,
            &refs,
        )?;
        self.last_oracle_round = Some(u.round);
        for (mid, price) in prices {
            let m = self.markets.get_mut(&mid).unwrap();
            let before = m.oracle.status;
            m.oracle.consume(price, tau, &m.p.oracle);
            let after = m.oracle.status;
            if before == Status::Active && after != Status::Active {
                self.events.push(Event::Halt { market: mid });
            } else if before != Status::Active && after == Status::Active {
                self.events.push(Event::Resume { market: mid });
            }
        }
        Ok(())
    }

    fn update_marks(&mut self) {
        let t = self.time;
        for m in self.markets.values_mut() {
            let index = m.oracle.index;
            if index == 0 {
                m.mark = 0;
                continue;
            }
            let bid = m.book.best_bid().unwrap_or(index);
            let ask = m.book.best_ask().unwrap_or(index);
            let last = match m.last_trade {
                Some((p, lt)) if t.saturating_sub(lt) <= MARK_TRADE_MAX_AGE_MS => p,
                _ => index,
            };
            let book_ref = median3(bid, ask, last) as i128;
            let band = (index as i128) * (m.p.mark_band_bps as i128) / 10000;
            let mark = index as i128 + (book_ref - index as i128).clamp(-band, band);
            m.mark = mark.clamp(1, MAX_PRICE as i128) as u64;
        }
    }

    fn active(&self, mid: u32) -> bool {
        self.markets.get(&mid).is_some_and(|m| m.oracle.status == Status::Active && m.mark > 0)
    }

    /// Average fill price of a market order of `impactNotional` on one side; `None` when too thin.
    fn impact_price(&self, mid: u32, buy: bool) -> Option<u64> {
        let m = &self.markets[&mid];
        let target = (m.p.impact_notional as i128) * N_PER_USDC;
        let mut notional: i128 = 0;
        let mut size: i128 = 0;
        let levels: Box<dyn Iterator<Item = (&u64, &std::collections::VecDeque<u64>)>> =
            if buy { Box::new(m.book.asks.iter()) } else { Box::new(m.book.bids.iter().rev()) };
        for (p, q) in levels {
            for id in q {
                let o = &self.orders[id];
                let need = target - notional;
                let lvl = (o.remaining as i128) * (*p as i128);
                if lvl >= need {
                    let s = div_ceil(need, *p as i128);
                    size += s;
                    notional += s * (*p as i128);
                    return Some((notional / size) as u64);
                }
                notional += lvl;
                size += o.remaining as i128;
            }
        }
        None
    }

    fn sample_premium(&mut self, mid: u32) {
        if !self.active(mid) {
            return;
        }
        let index = self.markets[&mid].oracle.index as i128;
        let ib = self.impact_price(mid, false).map(|x| x as i128).unwrap_or(index);
        let ia = self.impact_price(mid, true).map(|x| x as i128).unwrap_or(index);
        let num = (ib - index).max(0) - (index - ia).max(0);
        let prem = num * RATE_SCALE / index; // toward zero (S-7.1)
        self.markets.get_mut(&mid).unwrap().premium_samples.push(prem);
    }

    fn settle_funding(&mut self, mid: u32) {
        let (mut samples, interest, clampc, maxr, mark) = {
            let m = self.markets.get_mut(&mid).unwrap();
            (std::mem::take(&mut m.premium_samples), m.p.interest as i128, m.p.funding_clamp as i128, m.p.max_funding_rate as i128, m.mark as i128)
        };
        if samples.is_empty() || !self.active(mid) {
            return;
        }
        // R395-16: the MEDIAN of the interval's samples (lower middle), not the mean: a minority of manipulated
        // samples cannot move it.
        samples.sort_unstable();
        let p = samples[(samples.len() - 1) / 2];
        let rate = (p + (interest - p).clamp(-clampc, clampc)).clamp(-maxr, maxr);
        if rate == 0 {
            return;
        }
        let mut credited_total: i128 = 0;
        let mut changes: Vec<(usize, i128)> = Vec::new();
        for (ai, a) in self.accounts.iter().enumerate() {
            if let Some(pos) = a.positions.get(&mid) {
                if pos.size == 0 {
                    continue;
                }
                // f = size * mark * rate / RATE_SCALE; the account is credited -f, floored (S-7.3, S-0.3).
                let Some(x) = (pos.size as i128).checked_mul(mark).and_then(|v| v.checked_mul(rate)) else {
                    self.halt_arith(mid);
                    return;
                };
                let credit = div_floor(-x, RATE_SCALE * N_PER_USDC);
                credited_total += credit;
                changes.push((ai, credit));
            }
        }
        for (ai, c) in changes {
            self.accounts[ai].balance += c;
        }
        // sum of exact credits is 0 (I-1); the floored sum is <= 0; the difference is dust.
        self.dust += -credited_total * N_PER_USDC;
        let m = self.markets.get_mut(&mid).unwrap();
        m.cum_funding += rate;
        self.events.push(Event::Funding { market: mid, rate });
    }

    fn halt_arith(&mut self, mid: u32) {
        let m = self.markets.get_mut(&mid).unwrap();
        m.oracle.status = Status::Halted(HaltReason::Arithmetic);
        self.events.push(Event::Halt { market: mid });
    }

    // ------------------------------------------------------------------ margin

    fn upnl(&self, ai: u32) -> i128 {
        let a = &self.accounts[ai as usize];
        a.positions.iter().map(|(mid, p)| (p.size as i128) * (self.markets[mid].mark as i128) - p.open_notional).sum()
    }

    /// Equity E (notional units, SPEC 6.2).
    pub fn equity(&self, ai: u32) -> i128 {
        self.accounts[ai as usize].balance * N_PER_USDC + self.upnl(ai)
    }

    fn leverage_of(&self, ai: u32, mid: u32) -> u32 {
        self.accounts[ai as usize].leverage.get(&mid).copied().unwrap_or(self.markets[&mid].p.default_leverage)
    }

    /// (IR, MR) of one market for account `ai` with position size `size` and, optionally, an extra resting order.
    fn market_req(&self, ai: u32, mid: u32, size: i64, extra: Option<(bool, u64, u64)>) -> (i128, i128) {
        self.market_req_at(ai, mid, size, extra, self.markets[&mid].mark)
    }

    /// As `market_req` with the position valued at `price` instead of the mark.
    fn market_req_at(&self, ai: u32, mid: u32, size: i64, extra: Option<(bool, u64, u64)>, price: u64) -> (i128, i128) {
        let m = &self.markets[&mid];
        let mark = price as i128;
        let lev = self.leverage_of(ai, mid) as i128;
        let lev_frac = div_ceil(PPM, lev);
        let (mut bids, mut asks, mut pb, mut pa) = (0i128, 0i128, 0i128, 0i128);
        for oid in &self.accounts[ai as usize].orders {
            let o = &self.orders[oid];
            if o.market != mid || o.trigger.is_some() || o.reduce_only {
                continue;
            }
            if o.is_buy {
                bids += o.remaining as i128;
                pb = pb.max(o.price as i128);
            } else {
                asks += o.remaining as i128;
                pa = pa.max(o.price as i128);
            }
        }
        if let Some((b, p, s)) = extra {
            if b {
                bids += s as i128;
                pb = pb.max(p as i128);
            } else {
                asks += s as i128;
                pa = pa.max(p as i128);
            }
        }
        let s = size as i128;
        let n_pos = s.abs() * mark;
        let n_buy = (s + bids).abs() * mark.max(if bids > 0 { pb } else { 0 });
        let n_sell = (s - asks).abs() * mark.max(if asks > 0 { pa } else { 0 });
        let n_ir = n_pos.max(n_buy).max(n_sell);
        let ir = tier_req(n_ir, &m.p.tiers, |t| (t.imf_ppm as i128).max(lev_frac));
        let mr = tier_req(n_pos, &m.p.tiers, |t| t.mmf_ppm as i128);
        (ir, mr)
    }

    /// (IR, MR) over all markets; `hypo` replaces one market's position size and adds an optional order.
    fn account_req(&self, ai: u32, hypo: Option<(u32, i64, Option<(bool, u64, u64)>)>) -> (i128, i128) {
        let a = &self.accounts[ai as usize];
        let mut mids: BTreeSet<u32> = a.positions.keys().copied().collect();
        for oid in &a.orders {
            mids.insert(self.orders[oid].market);
        }
        if let Some((m, _, _)) = hypo {
            mids.insert(m);
        }
        let (mut ir, mut mr) = (0, 0);
        for mid in mids {
            let (size, extra) = match hypo {
                Some((m, s, e)) if m == mid => (s, e),
                _ => (a.positions.get(&mid).map(|p| p.size).unwrap_or(0), None),
            };
            let (i, m) = self.market_req(ai, mid, size, extra);
            ir += i;
            mr += m;
        }
        (ir, mr)
    }

    fn pos_size(&self, ai: u32, mid: u32) -> i64 {
        self.accounts[ai as usize].positions.get(&mid).map(|p| p.size).unwrap_or(0)
    }

    fn reducible(&self, ai: u32, mid: u32, is_buy: bool) -> u64 {
        let s = self.pos_size(ai, mid);
        if is_buy && s < 0 {
            abs64(s)
        } else if !is_buy && s > 0 {
            s as u64
        } else {
            0
        }
    }

    // ------------------------------------------------------------------ fills

    /// Update a position by a signed trade `d` at `p`; return the realised amount (notional units).
    fn trade_position(&mut self, ai: u32, mid: u32, d: i64, p: u64) -> i128 {
        let a = &mut self.accounts[ai as usize];
        let pos = a.positions.entry(mid).or_default();
        let (s, n) = (pos.size as i128, pos.open_notional);
        let (d, p) = (d as i128, p as i128);
        let mut realized = 0;
        if s == 0 || (s > 0) == (d > 0) {
            pos.open_notional = n + d * p;
            pos.size = (s + d) as i64;
        } else {
            let closed = d.abs().min(s.abs());
            let k = n * closed / s.abs(); // toward zero; exactness of I-2 does not depend on this rounding
            let c_signed = closed * s.signum();
            realized = c_signed * p - k;
            let mut new_n = n - k;
            let mut new_s = s - c_signed;
            let rem = d.abs() - closed;
            if rem > 0 {
                debug_assert_eq!(new_s, 0);
                new_n += d.signum() * rem * p;
                new_s = d.signum() * rem;
            }
            pos.size = new_s as i64;
            pos.open_notional = new_n;
        }
        if pos.size == 0 {
            debug_assert_eq!(pos.open_notional, 0);
            a.positions.remove(&mid);
        }
        realized
    }

    fn credit_notional(&mut self, ai: u32, x: i128) {
        let (u, d) = to_usdc_toward_protocol(x);
        self.accounts[ai as usize].balance += u;
        self.dust += d;
    }

    fn transfer(&mut self, from: u32, to: u32, amount: i128) {
        self.accounts[from as usize].balance -= amount;
        self.accounts[to as usize].balance += amount;
    }

    /// Execute a fill of `q` between `a` (signed direction `a_buy`) and `b` at price `p`.
    #[allow(clippy::too_many_arguments)]
    fn exec_fill(&mut self, mid: u32, kind: FillKind, a: u32, b: u32, a_buy: bool, p: u64, q: u64, a_fee_ppm: u32, b_fee_ppm: u32, a_order: u64, b_order: u64) {
        let d: i64 = if a_buy { q as i64 } else { -(q as i64) };
        let (sa, sb) = (self.pos_size(a, mid), self.pos_size(b, mid));
        let ra = self.trade_position(a, mid, d, p);
        let rb = self.trade_position(b, mid, -d, p);
        self.credit_notional(a, ra);
        self.credit_notional(b, rb);
        let (na, nb) = (self.pos_size(a, mid), self.pos_size(b, mid));
        let oi_delta = (na.max(0) - sa.max(0)) as i128 + (nb.max(0) - sb.max(0)) as i128;
        let notional = (q as i128) * (p as i128);
        let fa = div_ceil(notional * a_fee_ppm as i128, PPM * N_PER_USDC);
        let fb = div_ceil(notional * b_fee_ppm as i128, PPM * N_PER_USDC);
        if kind == FillKind::Book || kind == FillKind::Liquidation {
            let day = self.time / DAY_MS;
            let v = (notional / N_PER_USDC) as u128;
            for x in [a, b] {
                let vol = &mut self.accounts[x as usize].volume;
                *vol.entry(day).or_default() += v;
                while vol.keys().next().is_some_and(|d| *d + 14 < day) {
                    let k = *vol.keys().next().unwrap();
                    vol.remove(&k);
                }
            }
        }
        if fa > 0 {
            self.transfer(a, ACCT_PROTOCOL_FEE, fa);
        }
        if fb > 0 {
            self.transfer(b, ACCT_PROTOCOL_FEE, fb);
        }
        let t = self.time;
        let m = self.markets.get_mut(&mid).unwrap();
        m.long_oi = (m.long_oi as i128 + oi_delta) as u64;
        if kind == FillKind::Book || kind == FillKind::Liquidation {
            m.last_trade = Some((p, t));
        }
        self.events.push(Event::Fill { market: mid, kind, taker: a, maker: b, taker_order: a_order, maker_order: b_order, taker_is_buy: a_buy, price: p, size: q, taker_fee: fa, maker_fee: fb });
    }

    fn remove_order(&mut self, oid: u64, reason: Option<Error>) {
        if let Some(o) = self.orders.remove(&oid) {
            let m = self.markets.get_mut(&o.market).unwrap();
            if o.trigger.is_some() {
                m.triggers.remove(&oid);
            } else {
                m.book.remove(o.is_buy, o.price, oid);
            }
            self.accounts[o.account as usize].orders.remove(&oid);
            if let Some(r) = reason {
                self.events.push(Event::Cancelled { order: oid, reason: r });
            }
        }
    }

    /// Match a taker against the book. Returns (filled, stop reason).
    #[allow(clippy::too_many_arguments)]
    fn take(&mut self, ai: u32, mid: u32, is_buy: bool, limit: u64, size: u64, reduce_only: bool, mode: Mode, taker_order: u64) -> (u64, Option<Error>) {
        let mut remaining = size;
        let mut stop = None;
        while remaining > 0 {
            let Some((price, maker_id)) = self.markets[&mid].book.best_maker(is_buy, limit) else { break };
            let maker = self.orders[&maker_id].clone();
            if mode == Mode::Liquidation && maker.height == self.height {
                // R395-13: the sequencer knows which accounts step 9 liquidates; an order it placed in this very
                // block is not liquidity for them. The remainder goes to the backstop / ADL at the bound price.
                break;
            }
            if maker.account == ai {
                self.remove_order(maker_id, Some(Error::SelfTrade));
                continue;
            }
            let mut q = remaining.min(maker.remaining);
            if reduce_only {
                let r = self.reducible(ai, mid, is_buy);
                if r == 0 {
                    stop = Some(Error::ReduceOnly);
                    break;
                }
                q = q.min(r);
            }
            if maker.reduce_only {
                let r = self.reducible(maker.account, mid, !is_buy);
                if r == 0 {
                    self.remove_order(maker_id, Some(Error::ReduceOnly));
                    continue;
                }
                q = q.min(r);
            }
            let d: i64 = if is_buy { q as i64 } else { -(q as i64) };
            let (ts, ms) = (self.pos_size(ai, mid), self.pos_size(maker.account, mid));
            let (nts, nms) = (ts + d, ms - d);
            let oi_delta = (nts.max(0) - ts.max(0)) + (nms.max(0) - ms.max(0));
            let m = &self.markets[&mid];
            if oi_delta > 0 && (m.long_oi as i128 + oi_delta as i128) > m.p.oi_cap as i128 {
                stop = Some(Error::OiCap);
                break;
            }
            let taker_fee = if mode == Mode::Liquidation { 0 } else { self.fee_ppm(ai, true) };
            let maker_fee = self.fee_ppm(maker.account, false);
            let m = &self.markets[&mid];
            let only_reduces = (ts > 0 && nts >= 0 && nts < ts) || (ts < 0 && nts <= 0 && nts > ts);
            if mode == Mode::Normal && !only_reduces {
                // S-4.9: per-fill check of the taker's initial requirement after the fill.
                let mark = m.mark as i128;
                let notional = (q as i128) * (price as i128);
                let fee = div_ceil(notional * taker_fee as i128, PPM * N_PER_USDC);
                let e_after = self.equity(ai) + (d as i128) * (mark - price as i128) - fee * N_PER_USDC - N_PER_USDC;
                let (ir, _) = self.account_req(ai, Some((mid, nts, None)));
                if e_after < ir {
                    stop = Some(Error::InsufficientMargin);
                    break;
                }
            }
            self.exec_fill(mid, if mode == Mode::Liquidation { FillKind::Liquidation } else { FillKind::Book }, ai, maker.account, is_buy, price, q, taker_fee, maker_fee, taker_order, maker_id);
            let mo = self.orders.get_mut(&maker_id).unwrap();
            mo.remaining -= q;
            if mo.remaining == 0 {
                self.remove_order(maker_id, None);
            }
            remaining -= q;
        }
        (size - remaining, stop)
    }

    // ------------------------------------------------------------------ inputs

    fn apply_deposit(&mut self, index: u64, account: Addr, amount: u64) -> Result<(), Error> {
        if index != self.next_deposit {
            return Err(Error::DepositOrder);
        }
        let ai = match self.by_addr.get(&account) {
            Some(i) => *i,
            None => {
                let i = self.accounts.len() as u32;
                self.accounts.push(new_account(account));
                self.by_addr.insert(account, i);
                i
            }
        };
        let nb = self.accounts[ai as usize].balance + amount as i128;
        if nb > MAX_BALANCE {
            return Err(Error::Overflow); // not consumed: fail closed (SPEC S-3.2.1)
        }
        self.accounts[ai as usize].balance = nb;
        self.deposits_head = deposit_link(&self.deposits_head, index, &account, amount);
        self.next_deposit += 1;
        self.total_deposits += amount as i128;
        self.events.push(Event::Deposit { index, account, amount });
        Ok(())
    }

    fn receive_forced(&mut self, r: ForcedReq) -> Result<(), Error> {
        if r.index != self.next_forced {
            return Err(Error::ForcedOrder); // not received: the vault index is the single order
        }
        self.next_forced += 1;
        self.forced_queue.push_back(r);
        Ok(())
    }

    /// SPEC section 14 (R395-5): serve the queue from its head, in index order, every block. A request leaves the
    /// queue only when served or rejected for a user-caused reason; a halt-caused failure leaves it at the head
    /// (and everything behind it), so the vault's deadline (L2 block.timestamp) fires escape if it lasts.
    fn process_forced(&mut self) {
        while let Some(r) = self.forced_queue.front().cloned() {
            match self.try_forced(&r) {
                ForcedResult::Pending(e) => {
                    self.events.push(Event::ForcedPending { index: r.index, reason: e });
                    break;
                }
                res => {
                    match res {
                        ForcedResult::Served => self.events.push(Event::ForcedServed { index: r.index }),
                        ForcedResult::Rejected(e) => self.events.push(Event::ForcedRejected { index: r.index, reason: e }),
                        ForcedResult::Pending(_) => unreachable!(),
                    }
                    self.forced_queue.pop_front();
                    self.forced_head = forced_link(&self.forced_head, &r);
                    self.forced_processed += 1;
                }
            }
        }
    }

    fn try_forced(&mut self, r: &ForcedReq) -> ForcedResult {
        let ai = match self.by_addr.get(&r.account) {
            Some(&ai) if ai >= FIRST_USER => ai,
            // the vault lets only depositors request; an uncredited deposit is the sequencer's delay: wait
            Some(_) => return ForcedResult::Rejected(Error::UnknownAccount),
            None => return ForcedResult::Pending(Error::UnknownAccount),
        };
        match r.kind {
            FORCED_CANCEL_ALL | FORCED_CLOSE if r.arg != 0 => ForcedResult::Rejected(Error::Malformed),
            FORCED_CANCEL_ALL => {
                self.cancel_all(ai, 0);
                ForcedResult::Served
            }
            FORCED_CLOSE => {
                self.cancel_all(ai, 0);
                let mids: Vec<u32> = self.accounts[ai as usize].positions.keys().copied().collect();
                let mut pending = None;
                for mid in mids {
                    if let Err(e) = self.forced_close(ai, mid) {
                        pending = Some(e);
                    }
                }
                match pending {
                    Some(e) => ForcedResult::Pending(e),
                    None => ForcedResult::Served,
                }
            }
            FORCED_WITHDRAW => {
                let free = self.free_collateral(ai);
                let halted = self.halted_involved(ai);
                let fee = self.g.g.withdrawal_fee as i128;
                let want = if r.arg == 0 { free } else { r.arg as i128 };
                if want <= fee || want > free {
                    let e = if want <= fee { Error::WithdrawTooSmall } else { Error::InsufficientMargin };
                    // only the halt can make a request that is servable on live prices unservable now
                    return if halted { ForcedResult::Pending(Error::PositionInHaltedMarket) } else { ForcedResult::Rejected(e) };
                }
                match self.withdraw(ai, want as u64, r.account) {
                    Ok(()) => ForcedResult::Served,
                    Err(e) => ForcedResult::Rejected(e),
                }
            }
            _ => ForcedResult::Rejected(Error::Malformed),
        }
    }

    /// Close one position for a forced request. Active market: reduce-only through the book within the slippage
    /// bound, then the backstop (within its limit), then ADL, both at the bound price with the taker fee, so a
    /// forced close is always served and never cheaper than the book. Halted market: book only; the rest waits.
    fn forced_close(&mut self, ai: u32, mid: u32) -> Result<(), Error> {
        let s = self.pos_size(ai, mid);
        if s == 0 {
            return Ok(());
        }
        let is_buy = s < 0;
        if !self.active(mid) {
            // 394 README 6a: in a halt the book is used only inside the exit band, the rest is closed against the
            // opposite positions (ADL ranking) at the settle price with the taker fee: never refused for a halt.
            let Some(x) = self.exit_price(mid) else { return Err(Error::MarketHalted) };
            let limit = if is_buy { x.high.min(MAX_PRICE) } else { x.low };
            let oid = self.next_order_id;
            self.next_order_id += 1;
            self.take(ai, mid, is_buy, limit, abs64(s), true, Mode::Forced, oid);
            let r = abs64(self.pos_size(ai, mid));
            if r > 0 {
                let fee = self.fee_ppm(ai, true);
                self.adl(ai, mid, x.settle.clamp(x.low, x.high), r, is_buy, fee);
            }
            return Ok(());
        }
        let m = &self.markets[&mid];
        let band = (m.mark as u128 * m.p.market_slippage_bps as u128 / 10000) as u64;
        let limit = if is_buy { (m.mark + band).min(MAX_PRICE) } else { m.mark.saturating_sub(band).max(1) };
        if m.mark > 0 {
            let oid = self.next_order_id;
            self.next_order_id += 1;
            self.take(ai, mid, is_buy, limit, abs64(s), true, Mode::Forced, oid);
        }
        let r = abs64(self.pos_size(ai, mid));
        if r == 0 {
            return Ok(());
        }
        let fee = self.fee_ppm(ai, true);
        let q = self.backstop_room(mid, is_buy, r, self.markets[&mid].mark, limit);
        if q > 0 {
            self.exec_fill(mid, FillKind::Backstop, ai, ACCT_BACKSTOP, is_buy, limit, q, fee, 0, 0, 0);
        }
        let r = abs64(self.pos_size(ai, mid));
        if r > 0 {
            self.adl(ai, mid, limit, r, is_buy, fee);
        }
        Ok(())
    }

    /// Size the backstop may take from a closing account (direction `closer_buys`) at `price`: within its
    /// position limit and only while its equity after covers its maintenance requirement (SPEC 6.3 step 3).
    fn backstop_room(&self, mid: u32, closer_buys: bool, r: u64, mark: u64, price: u64) -> u64 {
        if self.accounts[ACCT_BACKSTOP as usize].balance <= 0 {
            return 0;
        }
        let bs = self.pos_size(ACCT_BACKSTOP, mid);
        let limit = ((self.markets[&mid].p.oi_cap as u128) * (self.g.g.backstop_oi_ppm as u128) / 1_000_000) as i128;
        let dir: i128 = if closer_buys { -1 } else { 1 };
        let same = dir * bs as i128;
        let room = (limit - same.max(0)).max(0) + (-same).max(0);
        let q = (r as i128).min(room) as u64;
        if q == 0 {
            return 0;
        }
        let d = (dir * q as i128) as i64;
        let e_after = self.equity(ACCT_BACKSTOP) + (d as i128) * (mark as i128 - price as i128);
        let (_, mr_after) = self.account_req(ACCT_BACKSTOP, Some((mid, bs + d, None)));
        if e_after >= mr_after {
            q
        } else {
            0
        }
    }

    fn authenticate(&mut self, sa: &SignedAction) -> Result<u32, Error> {
        let d = digest(&self.domain_sep, &struct_hash(&sa.action));
        let signer = recover(&d, &sa.signature).ok_or(Error::BadSignature)?;
        let account = sa.action.account();
        let ai = *self.by_addr.get(&account).ok_or(Error::UnknownAccount)?;
        if ai < FIRST_USER {
            return Err(Error::BadSignature);
        }
        let t = self.time;
        if signer != account {
            let ok = self.agent_owner.get(&signer) == Some(&ai)
                && self.accounts[ai as usize].agents.get(&signer).is_some_and(|ag| ag.expires_at > t);
            if !ok {
                return Err(Error::BadSignature);
            }
            if !sa.action.agent_allowed() {
                return Err(Error::AgentNotAllowed);
            }
        }
        // S-2.5
        let x = sa.action.nonce();
        if x.saturating_add(NONCE_PAST_MS) < t || x > t.saturating_add(NONCE_FUTURE_MS) {
            return Err(Error::NonceWindow);
        }
        let set = self.nonces.entry(signer).or_default();
        if set.contains(&x) {
            return Err(Error::NonceUsed);
        }
        if set.len() >= NONCE_SET {
            let min = *set.iter().next().unwrap();
            if x <= min {
                return Err(Error::NonceUsed);
            }
            set.remove(&min);
        }
        set.insert(x);
        Ok(ai)
    }

    fn prune_agents(&mut self, ai: u32) {
        let t = self.time;
        let expired: Vec<Addr> = self.accounts[ai as usize].agents.iter().filter(|(_, a)| a.expires_at <= t).map(|(k, _)| *k).collect();
        for k in expired {
            self.accounts[ai as usize].agents.remove(&k);
            self.agent_owner.remove(&k);
        }
    }

    pub fn apply_action(&mut self, sa: &SignedAction) -> Result<(), Error> {
        let ai = self.authenticate(sa)?;
        self.prune_agents(ai);
        match &sa.action {
            Action::PlaceOrder(p) => self.place(ai, p),
            Action::CancelOrder { market_id, order_id, .. } => {
                let o = self.orders.get(order_id).ok_or(Error::OrderNotFound)?;
                if o.account != ai || o.market != *market_id {
                    return Err(Error::OrderNotFound);
                }
                self.remove_order(*order_id, Some(Error::Cancelled));
                Ok(())
            }
            Action::CancelAll { market_id, .. } => {
                if *market_id != 0 && !self.markets.contains_key(market_id) {
                    return Err(Error::UnknownMarket);
                }
                self.cancel_all(ai, *market_id);
                Ok(())
            }
            Action::UpdateLeverage { market_id, leverage, isolated, .. } => {
                let m = self.markets.get(market_id).ok_or(Error::UnknownMarket)?;
                if *isolated {
                    return Err(Error::IsolatedUnsupported);
                }
                if *leverage == 0 || *leverage > m.p.max_leverage {
                    return Err(Error::Leverage);
                }
                if !self.active(*market_id) {
                    return Err(Error::MarketHalted);
                }
                let old = self.accounts[ai as usize].leverage.insert(*market_id, *leverage);
                let (ir, _) = self.account_req(ai, None);
                if self.equity(ai) < ir {
                    match old {
                        Some(v) => self.accounts[ai as usize].leverage.insert(*market_id, v),
                        None => self.accounts[ai as usize].leverage.remove(market_id),
                    };
                    return Err(Error::InsufficientMargin);
                }
                Ok(())
            }
            Action::UpdateMargin { market_id, .. } => {
                if !self.markets.contains_key(market_id) {
                    return Err(Error::UnknownMarket);
                }
                Err(Error::NotIsolated) // v0.1: no isolated positions exist (SPEC O-6)
            }
            Action::Withdraw { amount, recipient, .. } => self.withdraw(ai, *amount, *recipient),
            Action::ApproveAgent { agent, name, expires_at, .. } => {
                let t = self.time;
                if name.len() > 32 || *expires_at <= t || *expires_at > t.saturating_add(MAX_AGENT_TTL_MS) {
                    return Err(Error::Malformed);
                }
                if *agent == [0u8; 20] || *agent == self.accounts[ai as usize].address || self.by_addr.contains_key(agent) {
                    return Err(Error::AgentInvalid);
                }
                if let Some(owner) = self.agent_owner.get(agent) {
                    if *owner != ai {
                        return Err(Error::AgentInvalid);
                    }
                }
                let a = &mut self.accounts[ai as usize];
                if !a.agents.contains_key(agent) && a.agents.len() >= MAX_AGENTS {
                    return Err(Error::AgentInvalid);
                }
                a.agents.insert(*agent, Agent { name: name.clone(), expires_at: *expires_at });
                self.agent_owner.insert(*agent, ai);
                Ok(())
            }
            Action::RevokeAgent { agent, .. } => {
                if self.accounts[ai as usize].agents.remove(agent).is_none() {
                    return Err(Error::AgentInvalid);
                }
                self.agent_owner.remove(agent);
                Ok(())
            }
        }
    }

    fn cancel_all(&mut self, ai: u32, market_id: u32) {
        let ids: Vec<u64> = self.accounts[ai as usize].orders.iter().copied().filter(|id| market_id == 0 || self.orders[id].market == market_id).collect();
        for id in ids {
            self.remove_order(id, Some(Error::Cancelled));
        }
    }

    fn withdraw(&mut self, ai: u32, amount: u64, recipient: Addr) -> Result<(), Error> {
        if recipient == [0u8; 20] {
            return Err(Error::Malformed);
        }
        if amount <= self.g.g.withdrawal_fee {
            return Err(Error::WithdrawTooSmall);
        }
        // R395-5 (c): a halt never blocks free collateral (S-9.2: free collateral with the halt rule)
        let a = amount as i128;
        if a > self.free_collateral(ai) {
            return Err(Error::InsufficientMargin);
        }
        let fee = self.g.g.withdrawal_fee as i128;
        self.accounts[ai as usize].balance -= a;
        self.accounts[ACCT_PROTOCOL_FEE as usize].balance += fee;
        self.record_withdrawal(ai, recipient, (a - fee) as u64);
        Ok(())
    }

    /// S-9.2 (0.1.3): positions in a halted market count with their losses at the last mark but not their gains,
    /// and their initial requirement twice (a buffer against the move the halted oracle does not show).
    pub fn free_collateral(&self, ai: u32) -> i128 {
        let a = &self.accounts[ai as usize];
        let mut e = a.balance * N_PER_USDC;
        let mut ir = 0i128;
        let mut mids: BTreeSet<u32> = a.positions.keys().copied().collect();
        for oid in &a.orders {
            mids.insert(self.orders[oid].market);
        }
        for mid in mids {
            let size = a.positions.get(&mid).map(|p| p.size).unwrap_or(0);
            // halted: the side of 394's exit band worse for the holder (long at low, short at high), SPEC S-9.2
            let price = if self.active(mid) {
                self.markets[&mid].mark
            } else {
                match self.exit_price(mid) {
                    Some(x) if size > 0 => x.low,
                    Some(x) if size < 0 => x.high,
                    _ => self.markets[&mid].mark,
                }
            };
            let u = a.positions.get(&mid).map(|p| (p.size as i128) * (price as i128) - p.open_notional).unwrap_or(0);
            let (i, _) = self.market_req_at(ai, mid, size, None, price);
            e += u;
            ir += i;
        }
        div_floor(e - ir, N_PER_USDC).min(a.balance).max(0)
    }

    pub fn exit_price(&self, mid: u32) -> Option<crate::oracle::ExitPrice> {
        let m = &self.markets[&mid];
        m.oracle.exit_price(&m.p.oracle, self.time)
    }

    /// X3: a halt older than maxHaltMs settles every position of the market at the settle price (all orders and
    /// triggers of the market cancelled). Pairwise exact: the sizes sum to zero (I-1), so I-2 holds.
    fn settle_due_markets(&mut self) {
        let ids: Vec<u32> = self.markets.keys().copied().collect();
        for mid in ids {
            if self.active(mid) {
                continue;
            }
            let Some(x) = self.exit_price(mid) else { continue };
            if !x.settle_due {
                continue;
            }
            let has = self.accounts.iter().any(|a| a.positions.contains_key(&mid));
            let oids: Vec<u64> = self.orders.iter().filter(|(_, o)| o.market == mid).map(|(k, _)| *k).collect();
            for oid in oids {
                self.remove_order(oid, Some(Error::MarketHalted));
            }
            if !has {
                continue;
            }
            for ai in 0..self.accounts.len() as u32 {
                let s = self.pos_size(ai, mid);
                if s != 0 {
                    let r = self.trade_position(ai, mid, -s, x.settle);
                    self.credit_notional(ai, r);
                }
            }
            self.markets.get_mut(&mid).unwrap().long_oi = 0;
            self.events.push(Event::MarketSettled { market: mid, price: x.settle });
        }
    }

    pub fn halted_involved(&self, ai: u32) -> bool {
        self.accounts[ai as usize].positions.keys().any(|m| !self.active(*m))
    }

    pub fn checkpoint_fields(&self) -> CheckpointFields {
        CheckpointFields {
            deposits_credited: self.next_deposit,
            deposits_head: self.deposits_head,
            forced_processed: self.forced_processed,
            forced_head: self.forced_head,
            withdrawal_count: self.withdrawals.len() as u64,
            withdrawals_total: self.total_withdrawn as u128,
        }
    }

    fn record_withdrawal(&mut self, ai: u32, recipient: Addr, net: u64) {
        let rec = WithdrawalRecord { id: self.withdrawals.len() as u64, account: self.accounts[ai as usize].address, recipient, amount: net, height: self.height };
        self.total_withdrawn += net as i128;
        self.events.push(Event::Withdrawal(rec.clone()));
        self.withdrawals.push(rec);
    }

    fn check_order_fields(&self, p: &PlaceOrder) -> Result<(), Error> {
        let m = self.markets.get(&p.market_id).ok_or(Error::UnknownMarket)?;
        if p.order_type > ORDER_TP_LIMIT || p.time_in_force > TIF_POST_ONLY {
            return Err(Error::Malformed);
        }
        let is_market_type = matches!(p.order_type, ORDER_MARKET | ORDER_STOP_MARKET | ORDER_TP_MARKET);
        let is_trigger = p.order_type >= ORDER_STOP_MARKET;
        if is_market_type && p.time_in_force != TIF_IOC {
            return Err(Error::Malformed);
        }
        if is_trigger && p.time_in_force == TIF_POST_ONLY {
            return Err(Error::Malformed);
        }
        if p.price == 0 || p.price > MAX_PRICE {
            return Err(Error::Malformed);
        }
        if p.price % m.p.tick != 0 {
            return Err(Error::Tick);
        }
        if is_trigger {
            if p.trigger_price == 0 || p.trigger_price > MAX_PRICE {
                return Err(Error::Malformed);
            }
            if p.trigger_price % m.p.tick != 0 {
                return Err(Error::Tick);
            }
        } else if p.trigger_price != 0 {
            return Err(Error::Malformed);
        }
        if p.size % m.p.step != 0 {
            return Err(Error::Step);
        }
        if p.size < m.p.min_size || p.size > m.p.max_size {
            return Err(Error::SizeBounds);
        }
        Ok(())
    }

    /// S-9.4: tier from the 14 complete UTC days before today (known for the whole day, not movable mid-day).
    pub fn fee_ppm(&self, ai: u32, taker: bool) -> u32 {
        let today = self.time / DAY_MS;
        let vol: u128 = self.accounts[ai as usize].volume.range(today.saturating_sub(14)..today).map(|(_, v)| *v).sum();
        let t = self.g.g.fee_tiers.iter().rev().find(|t| vol >= t.min_volume_14d as u128).unwrap_or(&self.g.g.fee_tiers[0]);
        if taker {
            t.taker_fee_ppm
        } else {
            t.maker_fee_ppm
        }
    }

    /// S-4.11: one-way latch, opened once the insurance fund and the backstop hold their genesis minimums.
    fn launch_check(&mut self) -> bool {
        if !self.launched
            && self.accounts[ACCT_INSURANCE as usize].balance >= self.g.g.launch_insurance_min as i128
            && self.accounts[ACCT_BACKSTOP as usize].balance >= self.g.g.launch_backstop_min as i128
        {
            self.launched = true;
            self.events.push(Event::Launched);
        }
        self.launched
    }

    fn place(&mut self, ai: u32, p: &PlaceOrder) -> Result<(), Error> {
        self.check_order_fields(p)?;
        if !self.launch_check() {
            return Err(Error::NotLaunched);
        }
        if !self.active(p.market_id) {
            return Err(Error::MarketHalted);
        }
        let is_trigger = p.order_type >= ORDER_STOP_MARKET;
        let rests = is_trigger || p.time_in_force != TIF_IOC;
        if rests {
            let a = &self.accounts[ai as usize];
            let in_market = a.orders.iter().filter(|id| self.orders[*id].market == p.market_id).count();
            if a.orders.len() >= MAX_ORDERS_TOTAL || in_market >= MAX_ORDERS_PER_MARKET {
                return Err(Error::TooManyOrders);
            }
        }
        if is_trigger {
            let id = self.next_order_id;
            self.next_order_id += 1;
            self.orders.insert(id, Order { id, account: ai, market: p.market_id, is_buy: p.is_buy, price: p.price, remaining: p.size, reduce_only: p.reduce_only, client_order_id: p.client_order_id, trigger: Some((p.order_type, p.trigger_price)), height: self.height });
            self.markets.get_mut(&p.market_id).unwrap().triggers.insert(id);
            self.accounts[ai as usize].orders.insert(id);
            self.events.push(Event::TriggerStored { order: id });
            return Ok(());
        }
        let is_market = p.order_type == ORDER_MARKET;
        self.execute(ai, p.market_id, p.is_buy, is_market, p.time_in_force, p.price, p.size, p.reduce_only, p.client_order_id, None)
    }

    /// Run a limit or market order (from an action or a fired trigger).
    #[allow(clippy::too_many_arguments)]
    fn execute(&mut self, ai: u32, mid: u32, is_buy: bool, is_market: bool, tif: u8, price: u64, size: u64, reduce_only: bool, coid: H256, reuse_id: Option<u64>) -> Result<(), Error> {
        if reduce_only && self.reducible(ai, mid, is_buy) == 0 {
            return Err(Error::ReduceOnly);
        }
        let m = &self.markets[&mid];
        let mut limit = price;
        if is_market {
            let band = (m.mark as u128 * m.p.market_slippage_bps as u128 / 10000) as u64;
            limit = if is_buy { price.min(m.mark.saturating_add(band)) } else { price.max(m.mark.saturating_sub(band)) };
        }
        if tif == TIF_POST_ONLY && m.book.best_maker(is_buy, price).is_some() {
            return Err(Error::WouldCross);
        }
        if tif != TIF_IOC && !reduce_only {
            let size_now = self.pos_size(ai, mid);
            let (ir, _) = self.account_req(ai, Some((mid, size_now, Some((is_buy, price, size)))));
            if self.equity(ai) < ir {
                return Err(Error::InsufficientMargin);
            }
        }
        let id = reuse_id.unwrap_or_else(|| {
            let i = self.next_order_id;
            self.next_order_id += 1;
            i
        });
        let (filled, stop) = if tif == TIF_POST_ONLY { (0, None) } else { self.take(ai, mid, is_buy, limit, size, reduce_only, Mode::Normal, id) };
        let rem = size - filled;
        if rem > 0 {
            if tif != TIF_IOC && stop.is_none() {
                self.orders.insert(id, Order { id, account: ai, market: mid, is_buy, price, remaining: rem, reduce_only, client_order_id: coid, trigger: None, height: self.height });
                self.markets.get_mut(&mid).unwrap().book.insert(is_buy, price, id);
                self.accounts[ai as usize].orders.insert(id);
                self.events.push(Event::Rested { order: id });
            } else {
                self.events.push(Event::RemainderCancelled { order: id, size: rem, reason: stop.unwrap_or(Error::NoLiquidity) });
            }
        }
        Ok(())
    }

    // ------------------------------------------------------------------ system passes

    fn trigger_pass(&mut self) -> bool {
        let mut fired = Vec::new();
        for (mid, m) in &self.markets {
            if !(m.oracle.status == Status::Active && m.mark > 0) {
                continue;
            }
            for id in &m.triggers {
                let o = &self.orders[id];
                let (ty, tp) = o.trigger.unwrap();
                let stop = ty == ORDER_STOP_MARKET || ty == ORDER_STOP_LIMIT;
                let up = (stop && o.is_buy) || (!stop && !o.is_buy); // fires when mark >= trigger
                let hit = if up { m.mark >= tp } else { m.mark <= tp };
                if hit {
                    fired.push((*mid, *id));
                }
            }
        }
        for (mid, id) in &fired {
            let o = self.orders[id].clone();
            self.markets.get_mut(mid).unwrap().triggers.remove(id);
            self.orders.remove(id);
            self.accounts[o.account as usize].orders.remove(id);
            self.events.push(Event::Triggered { order: *id });
            let (ty, _) = o.trigger.unwrap();
            let is_market = ty == ORDER_STOP_MARKET || ty == ORDER_TP_MARKET;
            let tif = if is_market { TIF_IOC } else { TIF_GTC };
            if let Err(e) = self.execute(o.account, *mid, o.is_buy, is_market, tif, o.price, o.remaining, o.reduce_only, o.client_order_id, Some(*id)) {
                self.events.push(Event::Cancelled { order: *id, reason: e });
            }
        }
        !fired.is_empty()
    }

    fn liquidation_pass(&mut self) -> bool {
        let mut any = false;
        for ai in FIRST_USER..self.accounts.len() as u32 {
            let a = &self.accounts[ai as usize];
            if a.positions.is_empty() {
                continue;
            }
            if a.positions.keys().any(|m| !self.active(*m)) {
                continue; // SPEC S-4.10: no liquidation while a market of the account is halted
            }
            let (_, mr) = self.account_req(ai, None);
            if self.equity(ai) < mr {
                self.liquidate(ai);
                any = true;
            }
        }
        any
    }

    /// Price at which E would be 0 for position `mid`, the others held at mark (SPEC 6.3 step 2).
    fn bankruptcy_price(&self, ai: u32, mid: u32) -> u64 {
        let a = &self.accounts[ai as usize];
        let pos = &a.positions[&mid];
        let s = pos.size as i128;
        let rest: i128 = a.balance * N_PER_USDC
            + a.positions.iter().filter(|(k, _)| **k != mid).map(|(k, p)| (p.size as i128) * (self.markets[k].mark as i128) - p.open_notional).sum::<i128>();
        let num = pos.open_notional - rest;
        let bp = if s > 0 { div_ceil(num, s) } else { div_floor(-num, -s) };
        bp.clamp(1, MAX_PRICE as i128) as u64
    }

    fn liquidate(&mut self, ai: u32) {
        self.events.push(Event::Liquidated { account: ai });
        self.cancel_all(ai, 0);
        let mut gap = false;
        let e = self.equity(ai);
        if e < 0 {
            let need = div_ceil(-e, N_PER_USDC);
            // R395-31: insurance first, as far as it goes; only what it cannot cover is deleveraged
            let ins = self.accounts[ACCT_INSURANCE as usize].balance.max(0);
            let pay = need.min(ins);
            if pay > 0 {
                self.transfer(ACCT_INSURANCE, ai, pay);
                self.events.push(Event::InsuranceCover { account: ai, amount: pay });
            }
            gap = pay < need;
        }
        let mids: Vec<u32> = self.accounts[ai as usize].positions.keys().copied().collect();
        for mid in mids {
            let s0 = self.pos_size(ai, mid);
            if s0 == 0 {
                continue;
            }
            let mark = self.markets[&mid].mark;
            let bp = self.bankruptcy_price(ai, mid);
            let is_buy = s0 < 0;
            // R395-13: liquidation price bound = mark -/+ half the tier-0 maintenance fraction, never past bankruptcy;
            // the book, the backstop and ADL all fill at or inside it, so a liquidated account loses at most
            // mmf0/2 of the notional beyond mark plus the liquidation fee (the rest of its equity stays its own).
            let half_mmf = self.markets[&mid].p.tiers[0].mmf_ppm as u128 / 2;
            let off = ((mark as u128) * half_mmf / 1_000_000) as u64;
            let lp = if is_buy { bp.min(mark.saturating_add(off).min(MAX_PRICE)) } else { bp.max(mark.saturating_sub(off).max(1)) };
            if !gap {
                let oid = self.next_order_id;
                self.next_order_id += 1;
                self.take(ai, mid, is_buy, lp, abs64(s0), true, Mode::Liquidation, oid);
            }
            let r = abs64(self.pos_size(ai, mid));
            if r > 0 && !gap {
                let q = self.backstop_room(mid, is_buy, r, mark, lp);
                if q > 0 {
                    self.exec_fill(mid, FillKind::Backstop, ai, ACCT_BACKSTOP, is_buy, lp, q, 0, 0, 0, 0);
                }
            }
            let r = abs64(self.pos_size(ai, mid));
            if r > 0 {
                self.adl(ai, mid, if gap { bp } else { lp }, r, is_buy, 0);
            }
            // liquidation fee to the insurance fund, never below zero equity
            let notional = (abs64(s0) as i128) * (mark as i128);
            let fee = div_ceil(notional * self.markets[&mid].p.liquidation_fee_ppm as i128, PPM * N_PER_USDC);
            let avail = div_floor(self.equity(ai).max(0), N_PER_USDC).min(self.accounts[ai as usize].balance.max(0));
            let f = fee.min(avail);
            if f > 0 {
                self.transfer(ai, ACCT_INSURANCE, f);
                self.events.push(Event::LiquidationFee { account: ai, amount: f });
            }
        }
    }

    /// SPEC 6.4: opposite positions ranked by U/E (profit ratio times leverage), ties by account index.
    fn adl(&mut self, ai: u32, mid: u32, bp: u64, mut r: u64, liq_is_buy: bool, closer_fee_ppm: u32) {
        let mark = self.markets[&mid].mark as i128;
        let mut cands: Vec<(i128, u32)> = Vec::new();
        for (bi, a) in self.accounts.iter().enumerate() {
            let bi = bi as u32;
            if bi == ai || bi == ACCT_PROTOCOL_FEE || bi == ACCT_INSURANCE {
                continue;
            }
            if let Some(p) = a.positions.get(&mid) {
                // the liquidated account buys (closing a short) -> counterparties are longs, and vice versa
                if (liq_is_buy && p.size > 0) || (!liq_is_buy && p.size < 0) {
                    let u = (p.size as i128) * mark - p.open_notional;
                    let e = self.equity(bi);
                    // R395-14: the backstop (no key, cannot trade) is the FIRST counterparty of every ADL on the side
                    // opposite to its position: the only way its book is reduced in 0.1.5
                    let score = if bi == ACCT_BACKSTOP { i128::MAX } else if e > 0 { u.saturating_mul(PPM) / e } else { i128::MIN };
                    cands.push((score, bi));
                }
            }
        }
        cands.sort_by(|x, y| y.0.cmp(&x.0).then(x.1.cmp(&y.1)));
        for (_, bi) in cands {
            if r == 0 {
                break;
            }
            let q = r.min(abs64(self.pos_size(bi, mid)));
            if q == 0 {
                continue;
            }
            self.exec_fill(mid, FillKind::Adl, ai, bi, liq_is_buy, bp, q, closer_fee_ppm, 0, 0, 0);
            r -= q;
        }
        debug_assert_eq!(r, 0, "I-1 guarantees enough opposite size");
    }

    /// Accounts without positions and with a negative balance are covered by the insurance fund as far as it
    /// goes; the rest stays recorded as bad debt (never hidden, I-2 still exact).
    fn settle_bad_debt(&mut self) {
        for ai in FIRST_USER..self.accounts.len() as u32 {
            let a = &self.accounts[ai as usize];
            if a.balance < 0 && a.positions.is_empty() {
                let need = -a.balance;
                let ins = self.accounts[ACCT_INSURANCE as usize].balance.max(0);
                let pay = need.min(ins);
                if pay > 0 {
                    self.transfer(ACCT_INSURANCE, ai, pay);
                    self.events.push(Event::InsuranceCover { account: ai, amount: pay });
                }
                if need > pay {
                    self.events.push(Event::BadDebt { account: ai, amount: need - pay });
                }
            }
        }
    }

    fn sweep_fees(&mut self) {
        let bal = self.accounts[ACCT_PROTOCOL_FEE as usize].balance;
        if bal >= self.g.g.min_fee_sweep as i128 && bal > 0 {
            self.accounts[ACCT_PROTOCOL_FEE as usize].balance = 0;
            let fr = self.g.fee_recipient;
            self.record_withdrawal(ACCT_PROTOCOL_FEE, fr, bal as u64);
        }
    }

    fn sweep_dust(&mut self) {
        if self.dust >= N_PER_USDC {
            let u = self.dust / N_PER_USDC;
            self.dust -= u * N_PER_USDC;
            self.accounts[ACCT_INSURANCE as usize].balance += u;
        }
    }

    // ------------------------------------------------------------------ commitments (SPEC 11)

    /// SPEC 11 (0.1.5, R395-23): what each account is owed if every position closes at mark, floored at 0, with the
    /// total negative equity of all other accounts charged to `INSURANCE` first (the order of 6.3), so that the sum
    /// never exceeds custody while the insurance fund covers the deficits; only the remainder is the vault's
    /// pro-rata haircut.
    pub fn escape_values(&self) -> Vec<u64> {
        let mut v = Vec::with_capacity(self.accounts.len());
        let mut deficit: i128 = 0;
        for ai in 0..self.accounts.len() as u32 {
            let e = self.equity(ai);
            if e <= 0 {
                deficit += div_ceil(-e, N_PER_USDC);
                v.push(0);
            } else {
                v.push((e / N_PER_USDC).min(u64::MAX as i128) as u64);
            }
        }
        let ins = ACCT_INSURANCE as usize;
        v[ins] = (v[ins] as i128 - deficit).max(0) as u64;
        v
    }

    pub fn escape_value(&self, ai: u32) -> u64 {
        self.escape_values()[ai as usize]
    }

    pub fn account_leaf_data(&self, ai: u32) -> Vec<u8> {
        self.account_leaf_data_with(ai, self.escape_value(ai))
    }

    fn account_leaf_data_with(&self, ai: u32, escape: u64) -> Vec<u8> {
        let a = &self.accounts[ai as usize];
        let mut ph = Vec::new();
        ph.extend_from_slice(&(a.positions.len() as u32).to_be_bytes());
        for (mid, p) in &a.positions {
            ph.extend_from_slice(&mid.to_be_bytes());
            ph.extend_from_slice(&p.size.to_be_bytes());
            ph.extend_from_slice(&p.open_notional.to_be_bytes());
            ph.extend_from_slice(&0u64.to_be_bytes()); // isoMargin (v0.1: none)
            ph.push(0); // isolated
        }
        let mut d = Vec::with_capacity(8 + 20 + 4 + 16 + 8 + 32);
        d.extend_from_slice(b"PPv1acct");
        d.extend_from_slice(&a.address);
        d.extend_from_slice(&ai.to_be_bytes());
        d.extend_from_slice(&a.balance.to_be_bytes());
        d.extend_from_slice(&escape.to_be_bytes());
        d.extend_from_slice(&keccak(&ph));
        d
    }

    pub fn withdrawal_leaf_data(r: &WithdrawalRecord) -> Vec<u8> {
        let mut d = Vec::with_capacity(8 + 8 + 20 + 20 + 8 + 8);
        d.extend_from_slice(b"PPv1wdrl");
        d.extend_from_slice(&r.id.to_be_bytes());
        d.extend_from_slice(&r.account);
        d.extend_from_slice(&r.recipient);
        d.extend_from_slice(&r.amount.to_be_bytes());
        d.extend_from_slice(&r.height.to_be_bytes());
        d
    }

    fn market_leaf_data(mid: u32, m: &Market) -> Vec<u8> {
        let mut d = Vec::new();
        d.extend_from_slice(b"PPv1mrkt");
        d.extend_from_slice(&mid.to_be_bytes());
        d.extend_from_slice(&m.oracle.index.to_be_bytes());
        d.extend_from_slice(&m.mark.to_be_bytes());
        d.extend_from_slice(&m.long_oi.to_be_bytes());
        d.extend_from_slice(&m.cum_funding.to_be_bytes());
        d.push(match m.oracle.status {
            Status::Active => 0,
            Status::Halted(HaltReason::Stale) => 1,
            Status::Halted(HaltReason::Jump) => 2,
            Status::Halted(HaltReason::Arithmetic) => 3,
        });
        d
    }

    pub fn account_leaves(&self) -> Vec<H256> {
        let esc = self.escape_values();
        (0..self.accounts.len() as u32).map(|i| merkle::leaf_hash(&self.account_leaf_data_with(i, esc[i as usize]))).collect()
    }

    pub fn withdrawal_leaves(&self) -> Vec<H256> {
        self.withdrawals.iter().map(|r| merkle::leaf_hash(&Self::withdrawal_leaf_data(r))).collect()
    }

    pub fn markets_root(&self) -> H256 {
        let max = *self.markets.keys().next_back().unwrap() as usize;
        let mut leaves = vec![[0u8; 32]; max + 1];
        for (mid, m) in &self.markets {
            leaves[*mid as usize] = merkle::leaf_hash(&Self::market_leaf_data(*mid, m));
        }
        merkle::root(&leaves, MARKET_DEPTH)
    }

    /// Everything not in the three trees (orders, triggers, agents, nonces, leverage, oracle internals,
    /// premium accumulators), hashed in a defined order so that validators also agree on it (S-0.1).
    pub fn aux_hash(&self) -> H256 {
        let mut b = Vec::new();
        for (id, o) in &self.orders {
            b.extend_from_slice(&id.to_be_bytes());
            b.extend_from_slice(&o.account.to_be_bytes());
            b.extend_from_slice(&o.market.to_be_bytes());
            b.push(o.is_buy as u8);
            b.extend_from_slice(&o.price.to_be_bytes());
            b.extend_from_slice(&o.remaining.to_be_bytes());
            b.push(o.reduce_only as u8);
            b.extend_from_slice(&o.client_order_id);
            let (ty, tp) = o.trigger.unwrap_or((255, 0));
            b.push(ty);
            b.extend_from_slice(&tp.to_be_bytes());
            b.extend_from_slice(&o.height.to_be_bytes());
        }
        for (mid, m) in &self.markets {
            b.extend_from_slice(&mid.to_be_bytes());
            for q in m.book.bids.values().chain(m.book.asks.values()) {
                for id in q {
                    b.extend_from_slice(&id.to_be_bytes());
                }
            }
            b.extend_from_slice(&m.oracle.index_time.to_be_bytes());
            b.extend_from_slice(&m.oracle.halt_low.to_be_bytes());
            b.extend_from_slice(&m.oracle.halt_high.to_be_bytes());
            match m.oracle.run {
                Some((l, c, t)) => {
                    b.push(1);
                    b.extend_from_slice(&l.to_be_bytes());
                    b.extend_from_slice(&c.to_be_bytes());
                    b.extend_from_slice(&t.to_be_bytes());
                }
                None => b.push(0),
            }
            match m.last_trade {
                Some((p, t)) => {
                    b.push(1);
                    b.extend_from_slice(&p.to_be_bytes());
                    b.extend_from_slice(&t.to_be_bytes());
                }
                None => b.push(0),
            }
            b.extend_from_slice(&(m.premium_samples.len() as u32).to_be_bytes());
            for x in &m.premium_samples {
                b.extend_from_slice(&x.to_be_bytes());
            }
        }
        for (i, a) in self.accounts.iter().enumerate() {
            b.extend_from_slice(&(i as u32).to_be_bytes());
            for (mid, l) in &a.leverage {
                b.extend_from_slice(&mid.to_be_bytes());
                b.extend_from_slice(&l.to_be_bytes());
            }
            b.push(0xfe);
            for (ag, x) in &a.agents {
                b.extend_from_slice(ag);
                b.extend_from_slice(&(x.name.len() as u16).to_be_bytes());
                b.extend_from_slice(x.name.as_bytes());
                b.extend_from_slice(&x.expires_at.to_be_bytes());
            }
            b.push(0xff);
            for (d, v) in &a.volume {
                b.extend_from_slice(&d.to_be_bytes());
                b.extend_from_slice(&v.to_be_bytes());
            }
            b.push(0xfd);
        }
        b.push(self.launched as u8);
        b.extend_from_slice(&self.next_forced.to_be_bytes());
        b.extend_from_slice(&self.deposits_head);
        b.extend_from_slice(&self.forced_head);
        for r in &self.forced_queue {
            b.extend_from_slice(&r.index.to_be_bytes());
            b.extend_from_slice(&r.account);
            b.push(r.kind);
            b.extend_from_slice(&r.deadline.to_be_bytes());
            b.extend_from_slice(&r.arg.to_be_bytes());
        }
        for (s, set) in &self.nonces {
            b.extend_from_slice(s);
            b.extend_from_slice(&(set.len() as u32).to_be_bytes());
            for n in set {
                b.extend_from_slice(&n.to_be_bytes());
            }
        }
        match self.last_oracle_round {
            Some(r) => {
                b.push(1);
                b.extend_from_slice(&r.to_be_bytes());
            }
            None => b.push(0),
        }
        keccak(&b)
    }

    pub fn system_hash(&self) -> H256 {
        let total_escape: u128 = self.escape_values().iter().map(|x| *x as u128).sum();
        system_hash_of(&SystemFields {
            next_deposit_index: self.next_deposit,
            next_forced_index: self.forced_processed,
            next_order_id: self.next_order_id,
            total_escape,
            dust: self.dust as u128,
            aux_hash: self.aux_hash(),
            withdrawal_count: self.withdrawals.len() as u64,
            withdrawals_total: u64::try_from(self.total_withdrawn).expect("withdrawn total fits u64 (custody bound)"),
        })
    }

    pub fn state_root(&self) -> H256 {
        let ar = merkle::root(&self.account_leaves(), ACCOUNT_DEPTH);
        let wr = merkle::root(&self.withdrawal_leaves(), WITHDRAWAL_DEPTH);
        state_root_of(self.height, self.time, &ar, &self.markets_root(), &wr, &self.system_hash())
    }

    pub fn account_proof(&self, addr: &Addr) -> Option<(Vec<u8>, u32, Vec<H256>)> {
        let ai = *self.by_addr.get(addr)?;
        let leaves = self.account_leaves();
        Some((self.account_leaf_data(ai), ai, merkle::proof(&leaves, ACCOUNT_DEPTH, ai as usize)))
    }

    pub fn withdrawal_proof(&self, id: u64) -> Option<(Vec<u8>, Vec<H256>)> {
        let r = self.withdrawals.get(id as usize)?;
        Some((Self::withdrawal_leaf_data(r), merkle::proof(&self.withdrawal_leaves(), WITHDRAWAL_DEPTH, id as usize)))
    }
}


// Copyright (c) 2026 Bloch Inc.
//! `engine-vectors`: end-to-end vectors for `PerpsVault` PRODUCED BY THE ENGINE. UNAUDITED PROTOTYPE.
//!
//!   engine-vectors --story <normal|insurance|backstop|shortfall> --vault 0x.. [--chain-id 700770] [--t0 <L2 seconds>]
//!
//! It drives the real engine library (`engine/`, SPEC.md) through a story of L2 events (deposits, forced requests:
//! the bytes the vault would emit), signed user actions and oracle rounds, builds each checkpoint exactly as the
//! node does (`node/src/engine/real.rs::state_parts` + `Engine::checkpoint_fields`), checks the SPEC 11 state-root
//! equation, and signs it with the node's own `checkpoint::sign_checkpoint` under the vault's EIP-712 domain with a
//! THROWAWAY 5-of-7 committee (keys derived from public labels; never a real key, never written anywhere).
//! Withdrawal and account proofs are the engine's `withdrawal_proof` / `account_proof`.
//!
//! Output (stdout): one JSON document, a list of steps the vault must accept or refuse in order, each with the
//! effect expected of the vault. `contracts/test/EngineVectors.t.sol` (forge) and `contracts/script/e2e_anvil.py`
//! (a private anvil) replay it against the real contract. The expected payouts are computed here by an
//! independent restatement of VAULT.md sections 6 and 7 (not by calling the contract).
use k256::ecdsa::SigningKey;
use perps_node::checkpoint::{sign_checkpoint, CheckpointBody};
use perps_node::dev::sorted_keys;
use perps_node::eip712::Domain;
use perps_node::sig::key_address;
use postern_perps_engine as pe;
use postern_perps_engine::engine::{system_address, Event, ACCT_BACKSTOP, ACCT_INSURANCE, ACCT_PROTOCOL_FEE};
use postern_perps_engine::hash::{hex0x, parse_addr, Addr};
use postern_perps_engine::testkit::*;
use postern_perps_engine::*;
use serde_json::{json, Value};
use std::collections::BTreeMap;

const USDC: u64 = 1_000_000;
const P_BTC: u64 = 6_725_000_000_000;
const P_ETH: u64 = 348_000_000_000;

// The proposed launch limits (contracts/deploy/vault.config.json); the replay deploys the vault with exactly these.
const CAP: u64 = 1_000_000 * USDC;
const MIN_DEPOSIT: u64 = 10 * USDC;
const WINDOW_LIMIT: u64 = 250_000 * USDC;
const WINDOW_SECONDS: u64 = 86_400;
const LARGE_THRESHOLD: u64 = 50_000 * USDC;
const LARGE_DELAY: u64 = 86_400;
const FORCED_DELAY: u64 = 172_800;
const ESCAPE_DELAY: u64 = 604_800;
const PAUSE_DURATION: u64 = 172_800;
const PAUSE_COOLDOWN: u64 = 172_800;
const SURPLUS_DELAY: u64 = 90 * 86_400;

fn die(msg: &str) -> ! {
    eprintln!("engine-vectors: {msg}");
    std::process::exit(2)
}

fn h(b: &[u8]) -> String {
    hex0x(b)
}

/// The vault as VAULT.md describes it, restated independently (only what the stories need): what each step must
/// do to token balances. A step the model cannot predict is a bug in the story, so it panics.
#[derive(Default)]
struct VaultModel {
    deposits: Vec<(Addr, u64, Addr)>, // account, amount, payer
    deposited_total: u64,
    committed_out: u64,
    seq: u64,
    credited: u64,
    forced_count: u64,
    forced_processed: u64,
    forced_deadline: Vec<u64>,
    w_total: u64,
    w_count: u64,
    withdrawn_total: u64,
    registered: BTreeMap<u64, bool>,
    total_escape: u128,
    escape_at: u64,
    pool: u64,
    payable: u64,
    senior: u128,
    claimed: u64,
    refunded: BTreeMap<u64, bool>,
    refunded_total: u64,
    balances: BTreeMap<Addr, u64>, // token received from the vault, per recipient
    vault_balance: u64,
    bucket: u64,
    bucket_at: u64,
}

impl VaultModel {
    fn custody(&self) -> u64 {
        self.deposited_total - self.committed_out
    }
    fn uncredited(&self) -> u64 {
        self.deposits[self.credited as usize..].iter().map(|d| d.1).sum()
    }
    fn pay(&mut self, to: Addr, amount: u64, now: u64) {
        // token bucket (VAULT.md 6): the stories keep every payment inside it; a story that would not is refused here
        let refilled = (self.bucket as u128 + (now - self.bucket_at) as u128 * WINDOW_LIMIT as u128 / WINDOW_SECONDS as u128).min(WINDOW_LIMIT as u128) as u64;
        assert!(amount <= refilled, "story exceeds the release bucket");
        self.bucket = refilled - amount;
        self.bucket_at = now;
        *self.balances.entry(to).or_insert(0) += amount;
        self.vault_balance -= amount;
    }
}

struct Gen {
    e: Engine,
    /// The engine state at the last checkpoint the vault ACCEPTED: every proof is taken from it.
    snap: Engine,
    system_remainder: u64,
    /// Escape payouts at or above the large threshold: registered, payable `LARGE_DELAY` later.
    delayed: Vec<(Addr, Addr, u64, u64)>,
    nonce_bump: std::cell::Cell<u64>,
    ok: Vec<SigningKey>,
    vkeys: Vec<SigningKey>,
    dom: Domain,
    vault: Addr,
    chain_id: u64,
    height: u64,
    t_ms: u64,
    l2: u64,
    queued: Vec<Input>,
    prices: Vec<(u32, u64)>,
    steps: Vec<Value>,
    m: VaultModel,
    fee_recipient: Addr,
    notes: Vec<String>,
}

impl Gen {
    fn new(vault: Addr, chain_id: u64, t0: u64, ins_min: u64, back_min: u64) -> Gen {
        let mut g = genesis_raw(7, vec![market(1, "BTC-USD"), market(2, "ETH-USD")]);
        g.verifying_contract = h(&vault);
        g.chain_id = chain_id;
        g.launch_insurance_min = ins_min;
        g.launch_backstop_min = back_min;
        let fee_recipient = parse_addr(&g.fee_recipient).unwrap_or_else(|e| die(&e));
        let checked = g.check().unwrap_or_else(|e| die(&format!("genesis: {e:?}")));
        let e = Engine::new(checked);
        let vkeys = sorted_keys("contracts/engine-vectors/validator", 7);
        let dom = Domain { name: "PosternPerpsVault".into(), version: "1".into(), chain_id, verifying_contract: Some(vault) };
        let mut m = VaultModel::default();
        m.bucket = WINDOW_LIMIT;
        m.bucket_at = t0;
        Gen { snap: e.clone(), system_remainder: 0, delayed: vec![], nonce_bump: std::cell::Cell::new(0), e, ok: oracle_keys(7), vkeys, dom, vault, chain_id, height: 0, t_ms: t0 * 1000, l2: t0, queued: vec![], prices: vec![(1, P_BTC), (2, P_ETH)], steps: vec![], m, fee_recipient, notes: vec![] }
    }

    fn step(&mut self, kind: &str, mut v: Value) {
        v["kind"] = json!(kind);
        v["l2Time"] = json!(self.l2.to_string());
        v["n"] = json!(self.steps.len());
        self.steps.push(v);
    }

    /// L2 time moves with the engine's clock (seconds), never backwards; every L2 transaction is one second later.
    fn l2_tick(&mut self) {
        self.l2 = (self.l2 + 1).max(self.t_ms / 1000);
    }

    // ------------------------------------------------------------------ L2 side (what users do on the vault)
    fn deposit(&mut self, from: Addr, account: Addr, amount: u64) {
        self.l2_tick();
        assert!(amount >= MIN_DEPOSIT && self.m.custody() + amount <= CAP);
        let index = self.m.deposits.len() as u64;
        self.m.deposits.push((account, amount, from));
        self.m.deposited_total += amount;
        self.m.vault_balance += amount;
        self.queued.push(Input::Deposit { index, account, amount });
        self.step("deposit", json!({"from": h(&from), "account": h(&account), "amount": amount.to_string(), "expectIndex": index}));
    }

    /// `deliver = false`: the sequencer censors the request (the engine never receives it).
    fn forced(&mut self, from: Addr, kind: u8, arg: u64, deliver: bool) -> u64 {
        self.l2_tick();
        let index = self.m.forced_count;
        let deadline = self.l2 + FORCED_DELAY;
        self.m.forced_count += 1;
        self.m.forced_deadline.push(deadline);
        if deliver {
            self.queued.push(Input::Forced { index, account: from, kind, deadline, arg });
        }
        self.step("forced", json!({"from": h(&from), "forcedKind": kind, "arg": arg.to_string(), "expectIndex": index, "expectDeadline": deadline.to_string(), "delivered": deliver}));
        index
    }

    // ------------------------------------------------------------------ engine side
    /// One engine block `dt_ms` later with a fresh oracle round, the queued L2 inputs (in vault order) and `actions`.
    fn block(&mut self, dt_ms: u64, oracle: bool, actions: Vec<Input>) -> BlockOutcome {
        self.height += 1;
        self.t_ms += dt_ms;
        let mut inputs = vec![];
        if oracle {
            inputs.push(oracle_round(&self.e, &self.ok, self.t_ms, &self.prices));
        }
        inputs.append(&mut self.queued);
        inputs.extend(actions);
        let out = self.e.apply_block(&Block { height: self.height, time: self.t_ms, inputs }).unwrap_or_else(|x| die(&format!("block {} refused: {x:?}", self.height)));
        if let Err(msg) = invariants::check(&self.e) {
            die(&format!("engine invariant broken at {}: {msg}", self.height));
        }
        out
    }

    /// A fresh nonce inside the engine's window (S-2.5): the next block's time plus a counter.
    fn now_nonce(&self) -> u64 {
        let n = self.nonce_bump.get();
        self.nonce_bump.set(n + 1);
        self.t_ms + 3000 + n % 1000
    }

    /// The checkpoint of the engine's current state, built as the node builds it, signed by 5 of the 7.
    fn checkpoint(&mut self, expect: &str) -> CheckpointBody {
        let f = self.e.checkpoint_fields();
        let total_escape: u128 = self.e.escape_values().iter().map(|x| *x as u128).sum();
        let body = CheckpointBody {
            height: self.e.height,
            time: self.e.time,
            log_hash: self.e.log_hash,
            accounts_root: pe::merkle::root(&self.e.account_leaves(), pe::engine::ACCOUNT_DEPTH),
            markets_root: self.e.markets_root(),
            withdrawals_root: pe::merkle::root(&self.e.withdrawal_leaves(), pe::engine::WITHDRAWAL_DEPTH),
            next_deposit_index: f.deposits_credited,
            next_forced_index: f.forced_processed,
            next_order_id: self.e.next_order_id,
            total_escape,
            dust: self.e.dust as u128,
            aux_hash: self.e.aux_hash(),
            deposits_head: f.deposits_head,
            forced_head: f.forced_head,
            withdrawal_count: f.withdrawal_count,
            withdrawals_total: u64::try_from(f.withdrawals_total).unwrap(),
        };
        assert_eq!(body.state_root(), self.e.state_root(), "node checkpoint body does not reproduce the engine's state root");
        // signers 0,1,2,4,6 of the address-sorted committee: ascending, not simply the first five
        let sigs: Vec<String> = [0usize, 1, 2, 4, 6].iter().map(|i| h(&sign_checkpoint(&self.vkeys[*i], &body, &self.dom))).collect();
        self.l2_tick();
        // VAULT.md 5.3 acceptance, restated
        let overdue_left = body.next_forced_index < self.m.forced_count && self.l2 > self.m.forced_deadline[body.next_forced_index as usize];
        let uncredited_after: u64 = self.m.deposits[body.next_deposit_index as usize..].iter().map(|d| d.1).sum();
        let solvent = (body.withdrawals_total - self.m.withdrawn_total) + uncredited_after <= self.m.custody();
        let accept = !overdue_left && solvent && self.m.escape_at == 0;
        let want = match expect {
            "accepted" => true,
            "InvalidCheckpoint" => false,
            _ => die("checkpoint expectation"),
        };
        assert_eq!(accept, want, "the story's expectation differs from the restated acceptance rules (overdue {overdue_left}, solvent {solvent})");
        if accept {
            self.m.seq += 1;
            self.m.credited = body.next_deposit_index;
            self.m.forced_processed = body.next_forced_index;
            self.m.w_total = body.withdrawals_total;
            self.m.w_count = body.withdrawal_count;
            self.m.total_escape = total_escape;
            self.snap = self.e.clone();
        }
        let mut cp = body.to_json();
        // every number as a decimal string (uniform for the replay)
        for k in ["height", "time", "nextDepositIndex", "nextForcedIndex", "withdrawalCount"] {
            cp[k] = json!(cp[k].as_u64().unwrap().to_string());
        }
        self.step("checkpoint", json!({"cp": cp, "digest": h(&body.digest(&self.dom)), "signatures": sigs, "expect": expect, "expectSeq": self.m.seq}));
        body
    }

    /// Registers and pays withdrawal record `id` against the latest accepted checkpoint, with the engine's proof.
    fn withdraw(&mut self, id: u64) {
        let (data, siblings) = self.snap.withdrawal_proof(id).unwrap_or_else(|| die("no such withdrawal record"));
        let r = self.snap.withdrawals[id as usize].clone();
        assert_eq!(data.len(), 8 + 8 + 20 + 20 + 8 + 8);
        assert!(id < self.m.w_count && !self.m.registered.contains_key(&id), "record not certified or already registered");
        self.l2_tick();
        let to = r.recipient;
        assert!(r.amount < LARGE_THRESHOLD, "story keeps records below the large threshold");
        self.m.registered.insert(id, true);
        self.m.withdrawn_total += r.amount;
        self.m.committed_out += r.amount;
        let now = self.l2;
        self.m.pay(to, r.amount, now);
        let sib: Vec<String> = siblings.iter().map(|s| h(s)).collect();
        self.step(
            "withdraw",
            json!({"seq": self.m.seq, "w": {"withdrawalId": r.id.to_string(), "account": h(&r.account), "recipient": h(&r.recipient), "amount": r.amount.to_string(), "height": r.height.to_string()},
                   "leafData": h(&data), "siblings": sib, "expectPaid": r.amount.to_string(), "expectRecipient": h(&to)}),
        );
    }

    fn warp(&mut self, seconds: u64, why: &str) {
        self.l2 += seconds;
        self.step("warp", json!({"why": why}));
    }

    // ------------------------------------------------------------------ escape (vault side, engine data)
    fn trigger_escape(&mut self, reason: u8) {
        self.l2_tick();
        let overdue = self.m.forced_processed < self.m.forced_count && self.l2 > self.m.forced_deadline[self.m.forced_processed as usize];
        assert!(if reason == 2 { overdue } else { !overdue }, "escape reason");
        self.m.escape_at = self.l2;
        self.m.pool = self.m.custody() - (self.m.w_total - self.m.withdrawn_total) - self.m.uncredited();
        self.step("triggerEscape", json!({"expectReason": reason, "expectPool": self.m.pool.to_string(), "expectTotalEscape": self.m.total_escape.to_string()}));
    }

    fn account_json(&self, a: &Addr) -> (Value, u64) {
        let (data, index, siblings) = self.snap.account_proof(a).unwrap_or_else(|| die("no such account"));
        assert_eq!(data.len(), 8 + 20 + 4 + 16 + 8 + 32);
        let balance = i128::from_be_bytes(data[32..48].try_into().unwrap());
        let escape = u64::from_be_bytes(data[48..56].try_into().unwrap());
        let sib: Vec<String> = siblings.iter().map(|s| h(s)).collect();
        (json!({"a": {"account": h(a), "index": index, "balance": balance.to_string(), "escapeValue": escape.to_string(), "positionsHash": h(&data[56..88])}, "leafData": h(&data), "siblings": sib}), escape)
    }

    fn prove_system_shares(&mut self) {
        self.l2_tick();
        let (ins, ie) = self.account_json(&system_address(ACCT_INSURANCE));
        let (back, be) = self.account_json(&system_address(ACCT_BACKSTOP));
        let sub = ie as u128 + be as u128;
        self.m.senior = self.m.total_escape - sub;
        self.m.payable = (self.m.senior.min(self.m.pool as u128)) as u64;
        let rest = self.m.pool - self.m.payable;
        let sub_paid = (sub.min(rest as u128)) as u64;
        self.m.committed_out += sub_paid;
        self.system_remainder = sub_paid;
        self.step("proveSystemShares", json!({"insurance": ins, "backstop": back, "expectPayable": self.m.payable.to_string(), "expectSubordinated": sub.to_string(), "expectSystemRemainder": sub_paid.to_string()}));
    }

    fn claim(&mut self, a: Addr) -> u64 {
        self.l2_tick();
        let (acct, escape) = self.account_json(&a);
        let amount = if self.m.senior == 0 { 0 } else { (escape as u128 * self.m.payable as u128 / self.m.senior) as u64 };
        if amount == 0 {
            // an account the engine values at zero (negative equity, charged to INSURANCE) has nothing to claim
            self.step("claimEscape", json!({"proof": acct, "expect": "InvalidWithdrawal", "expectAmount": "0", "escapeValue": escape.to_string()}));
            return 0;
        }
        let to = if a == system_address(ACCT_PROTOCOL_FEE) { self.fee_recipient } else { a };
        self.m.claimed += amount;
        self.m.committed_out += amount;
        let now = self.l2;
        let large = amount >= LARGE_THRESHOLD;
        if large {
            self.delayed.push((a, to, amount, now + LARGE_DELAY));
        } else {
            self.m.pay(to, amount, now);
        }
        self.step("claimEscape", json!({"proof": acct, "expect": "registered", "expectAmount": amount.to_string(), "expectPaidNow": if large { "0".to_string() } else { amount.to_string() },
                                        "expectRecipient": h(&to), "escapeValue": escape.to_string()}));
        amount
    }

    /// Pays the escape payouts that had to wait the large-amount delay.
    fn release_delayed(&mut self) {
        let list = std::mem::take(&mut self.delayed);
        if list.is_empty() {
            return;
        }
        let ready = list.iter().map(|d| d.3).max().unwrap();
        if self.l2 < ready {
            let dt = ready - self.l2;
            self.warp(dt, "large-amount delay of the escape payouts");
        }
        for (a, to, amount, _) in list {
            self.l2_tick();
            let now = self.l2;
            self.m.pay(to, amount, now);
            self.step("releaseEscape", json!({"account": h(&a), "expectPaid": amount.to_string(), "expectRecipient": h(&to)}));
        }
    }

    fn refund(&mut self, index: u64) {
        self.l2_tick();
        assert!(index >= self.m.credited && !self.m.refunded.contains_key(&index));
        let (account, amount, payer) = self.m.deposits[index as usize];
        // a system deposit goes back to its payer, any other to the credited account (vault v0.3)
        let to = if account[..18] == [0u8; 18] { payer } else { account };
        self.m.refunded.insert(index, true);
        self.m.refunded_total += amount;
        self.m.committed_out += amount;
        *self.m.balances.entry(to).or_insert(0) += amount;
        self.m.vault_balance -= amount;
        self.step("refund", json!({"index": index, "account": h(&account), "amount": amount.to_string(), "expectRecipient": h(&to)}));
    }

    /// Pays the registered system remainder (INSURANCE + BACKSTOP, to the fee address) once its 90 days passed.
    fn release_system(&mut self) {
        let amount = self.system_remainder;
        self.l2_tick();
        assert!(self.l2 >= self.m.escape_at + SURPLUS_DELAY);
        let now = self.l2;
        // above the window limit it is paid in parts; the stories keep it inside one bucket
        let fr = self.fee_recipient;
        self.m.pay(fr, amount, now);
        self.step("releaseSystem", json!({"expectPaid": amount.to_string(), "expectRecipient": h(&fr)}));
    }

    /// 90 days after escape the surplus (balance beyond everything still claimable) goes to the fee address, once.
    /// The vault counts `escapePayable - escapeClaimedTotal` as still claimable, so the rounding residue of pro-rata
    /// claims (less than one base unit per claimant) is NOT surplus: it stays in the vault for good. The stories make
    /// no donation, so there is nothing to sweep and the call is refused.
    fn sweep(&mut self) {
        self.l2_tick();
        let unclaimed = self.m.payable - self.m.claimed;
        let amount = self.m.vault_balance - unclaimed;
        if amount == 0 {
            self.step("sweep", json!({"expect": "InvalidSurplus", "expectAmount": "0"}));
            return;
        }
        let fr = self.fee_recipient;
        *self.m.balances.entry(fr).or_insert(0) += amount;
        self.m.vault_balance -= amount;
        self.m.committed_out += amount;
        self.step("sweep", json!({"expect": "swept", "expectAmount": amount.to_string(), "expectRecipient": h(&fr)}));
    }

    fn finish(self, story: &str) -> Value {
        let balances: Vec<Value> = self.m.balances.iter().map(|(a, v)| json!({"account": h(a), "received": v.to_string()})).collect();
        json!({
            "schema": "postern-perps/engine-vault-vectors/1",
            "status": "PROTOTYPE, UNAUDITED. Produced by contracts/tools/engine-vectors from the real engine; throwaway keys",
            "story": story,
            "chainId": self.chain_id,
            "vault": h(&self.vault),
            "feeRecipient": h(&self.fee_recipient),
            "validators": self.vkeys.iter().map(|k| h(&key_address(k))).collect::<Vec<_>>(),
            "guardians": sorted_keys("contracts/engine-vectors/guardian", 3).iter().map(|k| h(&key_address(k))).collect::<Vec<_>>(),
            "limits": {"threshold": 5, "unpauseThreshold": 6, "cap": CAP.to_string(), "minDeposit": MIN_DEPOSIT.to_string(), "windowLimit": WINDOW_LIMIT.to_string(), "windowSeconds": WINDOW_SECONDS.to_string(),
                       "largeThreshold": LARGE_THRESHOLD.to_string(), "largeDelay": LARGE_DELAY.to_string(), "forcedDelay": FORCED_DELAY.to_string(), "escapeDelay": ESCAPE_DELAY.to_string(),
                       "pauseDuration": PAUSE_DURATION.to_string(), "pauseCooldown": PAUSE_COOLDOWN.to_string()},
            "engine": {"specVersion": self.e.g.g.spec_version, "finalHeight": self.e.height, "finalStateRoot": h(&self.e.state_root())},
            "notes": self.notes,
            "stepCount": self.steps.len(),
            "steps": self.steps,
            "expect": {"vaultBalance": self.m.vault_balance.to_string(), "depositedTotal": self.m.deposited_total.to_string(), "withdrawnTotal": self.m.withdrawn_total.to_string(),
                       "escapeClaimedTotal": self.m.claimed.to_string(), "refundedTotal": self.m.refunded_total.to_string(), "receivedCount": balances.len(), "received": balances},
        })
    }

    fn all_ok(&self, out: &BlockOutcome, what: &str) {
        if !out.outcomes.iter().all(|o| o.is_ok()) {
            die(&format!("{what}: an input was refused: {:?}", out.outcomes));
        }
    }

    fn ai(&self, a: &Addr) -> u32 {
        self.e.by_addr[a]
    }
}

fn buy(g: &Gen, k: &SigningKey, market: u32, price: u64, size: u64) -> Input {
    sign_action(&g.e, k, place(addr(k), g.now_nonce(), market, true, ORDER_MARKET, TIF_IOC, price, size, false))
}

fn ask(g: &Gen, k: &SigningKey, market: u32, price: u64, size: u64) -> Input {
    sign_action(&g.e, k, place(addr(k), g.now_nonce(), market, false, ORDER_LIMIT, TIF_GTC, price, size, false))
}

/// Story "normal": launch seeds through the vault, deposit -> credited -> trade -> withdraw -> paid; a forced
/// withdrawal served; then a forced request the sequencer ignores -> overdue -> checkpoint refused -> escape ->
/// every claim, refund and certified record paid; the vault ends holding only what nobody can claim.
fn story_normal(vault: Addr, chain_id: u64, t0: u64) -> Value {
    let mut g = Gen::new(vault, chain_id, t0, 60_000 * USDC, 25_000 * USDC);
    let (owner, ka, kb, kc, kd) = (addr(&key(900)), key(1), key(2), key(3), key(4));
    let (alice, bob, carol, dave) = (addr(&ka), addr(&kb), addr(&kc), addr(&kd));
    let (ins, back) = (system_address(ACCT_INSURANCE), system_address(ACCT_BACKSTOP));

    // 1. users deposit before the launch seeds exist: credited, but the launch gate keeps trading closed
    g.deposit(alice, alice, 10_000 * USDC);
    g.deposit(bob, bob, 40_000 * USDC);
    let out = g.block(3000, true, vec![]);
    g.all_ok(&out, "first deposits");
    let a1 = ask(&g, &kb, 1, P_BTC, 10_000_000);
    let out = g.block(3000, true, vec![a1]);
    assert_eq!(out.outcomes.last().unwrap(), &Err(Error::NotLaunched), "launch gate closed before the seeds");
    g.notes.push("before the INSURANCE/BACKSTOP seeds are deposited through the vault an order is refused NOT_LAUNCHED (SPEC 4.11)".into());

    // 2. the owner seeds INSURANCE and BACKSTOP through the vault (vault v0.3); the gate opens
    g.deposit(owner, ins, 60_000 * USDC);
    g.deposit(owner, back, 25_000 * USDC);
    let out = g.block(3000, true, vec![]);
    g.all_ok(&out, "seeds");
    g.checkpoint("accepted");

    // 3. trading: alice long 0.1 BTC from bob; fees accrue to PROTOCOL_FEE
    let a1 = ask(&g, &kb, 1, P_BTC, 10_000_000);
    let b1 = buy(&g, &ka, 1, P_BTC + 1_000_000_000, 10_000_000);
    let out = g.block(3000, true, vec![a1, b1]);
    g.all_ok(&out, "trade");
    assert!(out.events.iter().any(|e| matches!(e, Event::Fill { .. })));
    assert!(out.events.iter().any(|e| matches!(e, Event::Launched)), "the launch latch opens at the first order after the seeds (S-4.11)");

    // 4. alice withdraws 1,000 gross by a signed action; the record is paid against the next checkpoint
    let w = sign_action(&g.e, &ka, Action::Withdraw { account: alice, nonce: g.now_nonce(), amount: 1_000 * USDC, recipient: alice });
    let out = g.block(3000, true, vec![w]);
    g.all_ok(&out, "withdraw action");
    g.checkpoint("accepted");
    g.withdraw(0);

    // 5. bob's forced withdrawal (kind 2, 5,000 gross) posted on the L2, served by the engine
    g.forced(bob, 2, 5_000 * USDC, true);
    let out = g.block(3000, true, vec![]);
    assert!(out.events.iter().any(|e| matches!(e, Event::ForcedServed { index: 0 })), "forced withdrawal served: {:?}", out.events);
    g.checkpoint("accepted");
    g.withdraw(1);

    // 5b. the UTC day rolls over: the engine sweeps PROTOCOL_FEE into a withdrawal record for the owner's address
    let before = g.e.withdrawals.len();
    g.warp(17 * 3600, "past 00:00 UTC");
    g.t_ms = g.l2 * 1000;
    let out = g.block(3000, true, vec![]);
    g.all_ok(&out, "day rollover");
    let out = g.block(3000, true, vec![]);
    g.all_ok(&out, "after the rollover");
    if g.e.withdrawals.len() > before {
        let r = g.e.withdrawals[before].clone();
        assert_eq!((r.account, r.recipient), (system_address(ACCT_PROTOCOL_FEE), g.fee_recipient), "fee sweep record");
        g.notes.push(format!("fee sweep record {} of {} base units to the fee recipient", r.id, r.amount));
        g.checkpoint("accepted");
        g.withdraw(r.id);
    } else {
        die("expected a fee sweep record at the day rollover");
    }

    // 6. carol deposits and is credited; then alice's forced CloseAll is censored by the sequencer
    g.deposit(carol, carol, 2_000 * USDC);
    let out = g.block(3000, true, vec![]);
    g.all_ok(&out, "carol");
    g.checkpoint("accepted");
    g.forced(alice, 1, 0, false);
    // the exchange keeps running without it: a checkpoint before the deadline is still accepted
    let out = g.block(3000, true, vec![]);
    g.all_ok(&out, "quiet block");
    g.checkpoint("accepted");
    // dave deposits after the last checkpoint the vault will accept: refunded in escape
    g.deposit(dave, dave, 500 * USDC);

    // 7. the deadline passes; the engine (still ignoring the request) produces a checkpoint the vault refuses
    g.warp(FORCED_DELAY + 1, "forced deadline of request 1 passes");
    g.t_ms = g.l2 * 1000;
    g.queued.clear(); // the censoring sequencer also never credits dave
    let out = g.block(3000, true, vec![]);
    g.all_ok(&out, "block after the deadline");
    g.checkpoint("InvalidCheckpoint");

    // 8. escape by the overdue request, against the last ACCEPTED checkpoint: proofs must come from that state, so
    //    the generator rebuilds it (the engine state at the refused checkpoint is not what the vault holds)
    finish_escape(g, "normal", 2, &[alice, bob, carol], &[5])
}

/// The escape half. Proofs come from the engine state of the last ACCEPTED checkpoint (`snap`).
fn finish_escape(mut g: Gen, story: &str, reason: u8, users: &[Addr], refunds: &[u64]) -> Value {
    g.trigger_escape(reason);
    g.prove_system_shares();
    for u in users {
        g.claim(*u);
    }
    if g.snap.escape_value(ACCT_PROTOCOL_FEE) > 0 {
        g.claim(system_address(ACCT_PROTOCOL_FEE));
    }
    for i in refunds {
        g.refund(*i);
    }
    g.release_delayed();
    // every certified record was registered before escape in these stories
    assert_eq!(g.m.w_total, g.m.withdrawn_total);
    let wait = (g.m.escape_at + SURPLUS_DELAY).saturating_sub(g.l2);
    g.warp(wait, "90 days after escape: the system remainder and the surplus become payable to the fee address");
    if g.system_remainder > 0 {
        g.release_system();
    }
    g.sweep();
    // conservation, restated: everything deposited left the vault to someone
    let out: u64 = g.m.balances.values().sum();
    assert_eq!(out + g.m.vault_balance, g.m.deposited_total, "conservation in the model");
    g.finish(story)
}

/// Stories "insurance" and "shortfall": an account goes below zero equity while one of its markets is halted (no
/// liquidation, SPEC S-4.10), the validators then stop, and users exit by escape after `escapeDelay` of silence.
///   insurance (seeds 60,000 / 25,000): the engine's escape values charge the deficit to INSURANCE first; every
///     user is paid in full and the vault's claims sum exactly to custody.
///   backstop (seeds 5,000 / 2,000): the deficit exceeds the insurance fund; the engine's values sum above custody,
///     and the vault's subordination (R395-33) pays every user in full out of the BACKSTOP share.
///   shortfall (seeds 5,000 / 1,000): the deficit exceeds both; users share the rest pro rata and the system
///     accounts receive nothing.
fn story_gap(vault: Addr, chain_id: u64, t0: u64, name: &str, ins_seed: u64, back_seed: u64) -> Value {
    let mut g = Gen::new(vault, chain_id, t0, ins_seed * USDC, back_seed * USDC);
    let (owner, kb, km, kc) = (addr(&key(900)), key(2), key(5), key(3));
    let (bob, mallory, carol) = (addr(&kb), addr(&km), addr(&kc));
    let (ins, back) = (system_address(ACCT_INSURANCE), system_address(ACCT_BACKSTOP));
    g.deposit(owner, ins, ins_seed * USDC);
    g.deposit(owner, back, back_seed * USDC);
    g.deposit(bob, bob, 40_000 * USDC);
    g.deposit(mallory, mallory, 8_000 * USDC);
    g.deposit(carol, carol, 3_000 * USDC);
    let out = g.block(3000, true, vec![]);
    g.all_ok(&out, "deposits");
    // mallory: long 1 BTC at the default 10x (6,725 of her 8,000 as margin) and a small ETH position, against bob
    let a1 = ask(&g, &kb, 1, P_BTC, 100_000_000);
    let a2 = ask(&g, &kb, 2, P_ETH, 1_000_000);
    let b1 = buy(&g, &km, 1, P_BTC + 1_000_000_000, 100_000_000);
    let b2 = buy(&g, &km, 2, P_ETH + 100_000_000, 1_000_000);
    let out = g.block(3000, true, vec![a1, a2, b1, b2]);
    g.all_ok(&out, "positions");
    assert_eq!(g.e.accounts[g.ai(&mallory) as usize].positions.len(), 2, "mallory holds BTC and ETH");
    g.checkpoint("accepted");
    // the ETH oracle goes silent: after maxStaleness the ETH market halts; BTC keeps trading and falls 4% a round
    g.prices = vec![(1, P_BTC)];
    for _ in 0..4 {
        let out = g.block(3000, true, vec![]);
        g.all_ok(&out, "ETH going stale");
    }
    assert!(g.e.halted_involved(g.ai(&mallory)), "ETH halted: mallory cannot be liquidated");
    let mut p = P_BTC;
    for _ in 0..6 {
        p = p * 96 / 100 / 10_000_000 * 10_000_000;
        g.prices = vec![(1, p)];
        let out = g.block(3000, true, vec![]);
        g.all_ok(&out, "BTC falling");
        assert!(!out.events.iter().any(|e| matches!(e, Event::Liquidated { .. })), "no liquidation while a market of the account is halted");
    }
    let me = g.e.equity(g.ai(&mallory));
    if std::env::var("VECTORS_DEBUG").is_ok() {
        let ai = g.ai(&mallory) as usize;
        eprintln!("mallory balance {} positions {:?} equity {me}", g.e.accounts[ai].balance, g.e.accounts[ai].positions);
        for (id, m) in &g.e.markets {
            eprintln!("market {id}: mark {} index {} status {:?}", m.mark, m.oracle.index, m.oracle.status);
        }
    }
    assert!(me < 0, "mallory is below zero equity at the checkpoint");
    let values = g.e.escape_values();
    let custody = g.m.custody() as u128;
    let total: u128 = values.iter().map(|x| *x as u128).sum();
    g.notes.push(format!("at the last checkpoint: mallory equity {} (1e-8 notional units), INSURANCE balance {} -> escape value {}, sum of escape values {} vs custody {}",
        me, g.e.accounts[ACCT_INSURANCE as usize].balance, values[ACCT_INSURANCE as usize], total, custody));
    let sub = values[ACCT_INSURANCE as usize] as u128 + values[ACCT_BACKSTOP as usize] as u128;
    match name {
        "insurance" => assert!(values[ACCT_INSURANCE as usize] > 0 && total <= custody, "insurance covers the deficit: no haircut"),
        "backstop" => assert!(values[ACCT_INSURANCE as usize] == 0 && total > custody && total - sub <= custody, "deficit beyond insurance, inside the backstop share"),
        _ => assert!(values[ACCT_INSURANCE as usize] == 0 && total - sub > custody, "deficit beyond both system shares: users share the rest pro rata"),
    }
    g.checkpoint("accepted");
    // the validators stop. Nothing is submitted for escapeDelay; anyone triggers escape (reason 1).
    g.warp(ESCAPE_DELAY, "no checkpoint for escapeDelay");
    finish_escape(g, name, 1, &[bob, mallory, carol], &[])
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let get = |k: &str| args.iter().position(|a| a == k).and_then(|i| args.get(i + 1)).cloned();
    let story = get("--story").unwrap_or_else(|| die("usage: engine-vectors --story <normal|insurance|backstop|shortfall> --vault 0x.. [--chain-id 700770] [--t0 seconds]"));
    let vault = parse_addr(&get("--vault").unwrap_or_else(|| die("--vault required"))).unwrap_or_else(|e| die(&e));
    let chain_id: u64 = get("--chain-id").map(|v| v.parse().unwrap_or_else(|_| die("--chain-id"))).unwrap_or(700770);
    let t0: u64 = get("--t0").map(|v| v.parse().unwrap_or_else(|_| die("--t0"))).unwrap_or(1_800_000_000);
    let out = match story.as_str() {
        "normal" => story_normal(vault, chain_id, t0),
        "insurance" => story_gap(vault, chain_id, t0, "insurance", 60_000, 25_000),
        "backstop" => story_gap(vault, chain_id, t0, "backstop", 5_000, 2_000),
        "shortfall" => story_gap(vault, chain_id, t0, "shortfall", 5_000, 1_000),
        _ => die("unknown story"),
    };
    println!("{}", serde_json::to_string_pretty(&out).unwrap());
}

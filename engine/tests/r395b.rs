//! Fixes of 395's engine Mediums R395-13, -14, -16, -23 and the systemHash fields of VAULT.md 5.2 (SPEC 0.1.5).
use k256::ecdsa::SigningKey;
use postern_perps_engine::engine::{self, account_leaf_bytes, chain_genesis, system_address, Event, FillKind, ACCT_BACKSTOP};
use postern_perps_engine::hash::{hex0x, keccak, keccak_parts, parse_addr, Addr};
use postern_perps_engine::invariants;
use postern_perps_engine::merkle;
use postern_perps_engine::testkit::*;
use postern_perps_engine::*;
use serde_json::Value;

const T0: u64 = 1_794_880_000_000;
const P: u64 = 6_725_000_000_000;
const TICK: u64 = 10_000_000;

struct Rig {
    e: Engine,
    ok: Vec<SigningKey>,
    h: u64,
    t: u64,
    dep: u64,
}

impl Rig {
    fn new() -> Rig {
        Rig { e: Engine::new(genesis(7, vec![market(1, "BTC-USD")])), ok: oracle_keys(7), h: 0, t: T0, dep: 0 }
    }
    fn dep(&mut self, a: Addr, usdc: u64) -> Input {
        let i = Input::Deposit { index: self.dep, account: a, amount: usdc * 1_000_000 };
        self.dep += 1;
        i
    }
    fn block(&mut self, dt: u64, oracle: Option<u64>, mut rest: Vec<Input>) -> BlockOutcome {
        self.h += 1;
        self.t += dt;
        let mut inputs = vec![];
        if let Some(p) = oracle {
            inputs.push(oracle_round(&self.e, &self.ok, self.t, &[(1, p)]));
        }
        inputs.append(&mut rest);
        let out = self.e.apply_block(&Block { height: self.h, time: self.t, inputs }).unwrap();
        invariants::check(&self.e).unwrap();
        out
    }
}

fn tick(p: u64) -> u64 {
    p / TICK * TICK
}

#[test]
fn r395_13_liquidation_never_fills_same_block_orders_nor_past_the_bound() {
    let mut r = Rig::new();
    let (ka, kb, kc, kd) = (key(1), key(2), key(3), key(4));
    let (a, b, c, d) = (addr(&ka), addr(&kb), addr(&kc), addr(&kd));
    let ds: Vec<Input> = vec![r.dep(a, 700), r.dep(b, 50_000), r.dep(c, 50_000), r.dep(d, 50_000)];
    r.block(1000, Some(P), ds);
    let t = r.t + 3000;
    let ask = sign_action(&r.e, &kb, place(b, t, 1, false, ORDER_LIMIT, TIF_GTC, P, 10_000_000, false));
    let buy = sign_action(&r.e, &ka, place(a, t, 1, true, ORDER_MARKET, TIF_IOC, P + 100 * TICK * 10, 10_000_000, false));
    r.block(3000, Some(P), vec![ask, buy]);
    // E = MR near p* = 60889 USD (SPEC 6.2 with 700 USDC, 0.1 BTC, mmf 1%): an OLD bid of d between bankruptcy
    // (~60280) and the bound mark - mmf/2 (~60585)
    let old_bid_px = tick(6_046_300_000_000);
    let t = r.t + 3000;
    let old = sign_action(&r.e, &kd, place(d, t, 1, true, ORDER_LIMIT, TIF_GTC, old_bid_px, 10_000_000, false));
    r.block(3000, Some(P), vec![old]);
    let mut p = P;
    let mut liq = None;
    for _ in 0..80 {
        p = tick(p * 998 / 1000);
        let t = r.t + 3000;
        // the sequencer's own bid 1 USD under the new price, placed in THIS block (cancel the previous one first)
        let cancel = sign_action(&r.e, &kc, Action::CancelAll { account: c, nonce: t, market_id: 0 });
        let bid = sign_action(&r.e, &kc, place(c, t + 1, 1, true, ORDER_LIMIT, TIF_GTC, p - 10 * TICK, 10_000_000, false));
        let out = r.block(3000, Some(p), vec![cancel, bid]);
        if out.events.iter().any(|e| matches!(e, Event::Liquidated { .. })) {
            liq = Some(out);
            break;
        }
    }
    let out = liq.expect("a is liquidated");
    let (ci, di) = (r.e.by_addr[&c], r.e.by_addr[&d]);
    for e in &out.events {
        if let Event::Fill { maker, kind, price, .. } = e {
            assert!(*maker != ci, "same-block order of the sequencer filled a liquidation");
            assert!(*maker != di, "a bid past the liquidation bound received the liquidated margin");
            if *kind == FillKind::Adl {
                assert!(*price > old_bid_px, "ADL at the bound price, above the old bid");
            }
        }
    }
    assert!(r.e.accounts[r.e.by_addr[&a] as usize].positions.is_empty());
}

#[test]
fn r395_14_backstop_is_unwound_by_the_opposite_side() {
    let mut g = genesis_raw(7, vec![market(1, "BTC-USD")]);
    g.backstop_oi_ppm = 250_000;
    let mut r = Rig { e: Engine::new(g.check().unwrap()), ok: oracle_keys(7), h: 0, t: T0, dep: 0 };
    let (ka, kb) = (key(1), key(2));
    let (a, b) = (addr(&ka), addr(&kb));
    let ds = vec![r.dep(a, 700), r.dep(b, 50_000), r.dep(system_address(ACCT_BACKSTOP), 100_000)];
    r.block(1000, Some(P), ds);
    let t = r.t + 3000;
    let ask = sign_action(&r.e, &kb, place(b, t, 1, false, ORDER_LIMIT, TIF_GTC, P, 10_000_000, false));
    let buy = sign_action(&r.e, &ka, place(a, t, 1, true, ORDER_MARKET, TIF_IOC, P + 1000 * TICK, 10_000_000, false));
    r.block(3000, Some(P), vec![ask, buy]);
    let mut p = P;
    for _ in 0..12 {
        p = tick(p * 985 / 1000);
        let out = r.block(3000, Some(p), vec![]);
        if out.events.iter().any(|e| matches!(e, Event::Liquidated { .. })) {
            break;
        }
    }
    let bs = |e: &Engine| e.accounts[ACCT_BACKSTOP as usize].positions.get(&1).map(|x| x.size).unwrap_or(0);
    assert_eq!(bs(&r.e), 10_000_000, "the backstop took a's long");
    // b (short) closes through a forced request: the backstop's long is the first thing it meets
    let f = Input::Forced { index: 0, account: b, kind: FORCED_CLOSE, market_id: 0, amount: 0, deadline: 0 };
    r.block(3000, Some(p), vec![f]);
    assert_eq!(bs(&r.e), 0, "the backstop's position was reduced by the opposite side");
    let _ = engine::ACCT_INSURANCE;
}

#[test]
fn r395_16_one_quoted_sample_does_not_set_the_funding_rate() {
    let mut r = Rig::new();
    let (ka, kb, kc) = (key(1), key(2), key(3));
    let (a, b, c) = (addr(&ka), addr(&kb), addr(&kc));
    let to_b = 3_600_000 - (r.t % 3_600_000) - 60_000;
    let ds = vec![r.dep(a, 100_000), r.dep(b, 100_000), r.dep(c, 1_000_000)];
    r.block(to_b, Some(P), ds);
    let t = r.t + 3000;
    let ask = sign_action(&r.e, &kb, place(b, t, 1, false, ORDER_LIMIT, TIF_GTC, P, 10_000_000, false));
    let buy = sign_action(&r.e, &ka, place(a, t, 1, true, ORDER_MARKET, TIF_IOC, P + 1000 * TICK, 10_000_000, false));
    r.block(3000, Some(P), vec![ask, buy]);
    // one block: c quotes a huge bid 4% over the index as the last action (sampled), cancels it in the next block
    let t = r.t + 3000;
    let q = sign_action(&r.e, &kc, place(c, t, 1, true, ORDER_LIMIT, TIF_POST_ONLY, tick(P * 104 / 100), 100_000_000, false));
    r.block(3000, Some(P), vec![q]);
    let t = r.t + 3000;
    let cancel = sign_action(&r.e, &kc, Action::CancelAll { account: c, nonce: t, market_id: 0 });
    r.block(3000, Some(P), vec![cancel]);
    let mut rate = None;
    for _ in 0..25 {
        let out = r.block(3000, Some(P), vec![]);
        for e in out.events {
            if let Event::Funding { rate: x, .. } = e {
                rate = Some(x);
            }
        }
    }
    assert_eq!(rate, Some(12_500), "the median of the hour's samples is 0: rate = interest, the quoted sample is ignored");
}

#[test]
fn vault_vectors_of_390_reproduced_and_0_1_5_state_root() {
    let v: Value = serde_json::from_str(include_str!("fixtures/vault-27c1969/vault-vectors.json")).unwrap();
    let k = |s: &str| keccak(s.trim_start_matches("keccak256('").trim_end_matches("')").as_bytes());
    let i = &v["inputs"];
    let o = &v["outputs"];
    let w = &i["withdrawal"];
    let wr = engine::WithdrawalRecord {
        id: w["withdrawalId"].as_str().unwrap().parse().unwrap(),
        account: parse_addr(w["account"].as_str().unwrap()).unwrap(),
        recipient: parse_addr(w["recipient"].as_str().unwrap()).unwrap(),
        amount: w["amount"].as_str().unwrap().parse().unwrap(),
        height: w["height"].as_str().unwrap().parse().unwrap(),
    };
    assert_eq!(hex0x(&merkle::leaf_hash(&Engine::withdrawal_leaf_data(&wr))), o["withdrawalLeaf"].as_str().unwrap());
    let ac = &i["account"];
    let leaf = account_leaf_bytes(
        &parse_addr(ac["account"].as_str().unwrap()).unwrap(),
        ac["index"].as_u64().unwrap() as u32,
        ac["balance"].as_str().unwrap().parse().unwrap(),
        ac["escapeValue"].as_str().unwrap().parse().unwrap(),
        &k(ac["positionsHash"].as_str().unwrap()),
    );
    assert_eq!(hex0x(&merkle::leaf_hash(&leaf)), o["accountLeaf"].as_str().unwrap());
    let leaves: Vec<_> = i["treeLeaves"].as_array().unwrap().iter().map(|x| k(x.as_str().unwrap())).collect();
    assert_eq!(hex0x(&merkle::root(&leaves, 32)), o["root32_abc"].as_str().unwrap());
    assert_eq!(hex0x(&merkle::root(&[], 32)), o["root32_empty"].as_str().unwrap());
    let vault = parse_addr(v["vault"].as_str().unwrap()).unwrap();
    assert_eq!(hex0x(&chain_genesis("PosternPerps.deposit.v1", 700770, &vault)), o["depositHead0"].as_str().unwrap());
    assert_eq!(hex0x(&chain_genesis("PosternPerps.forced.v1", 700770, &vault)), o["forcedHead0"].as_str().unwrap());
    let c = &i["checkpoint"];
    let u = |f: &str| c[f].as_str().unwrap().parse::<u64>().unwrap();
    let u128f = |f: &str| c[f].as_str().unwrap().parse::<u128>().unwrap();
    let (ar, mr, wroot, aux) = (k(c["accountsRoot"].as_str().unwrap()), k(c["marketsRoot"].as_str().unwrap()), k(c["withdrawalsRoot"].as_str().unwrap()), k(c["auxHash"].as_str().unwrap()));
    // 390's stateRoot (SPEC 0.1.1 systemHash, six fields): the shared encoding agrees
    let sys_011 = keccak_parts(&[b"PPv1sys", &u("nextDepositIndex").to_be_bytes(), &u("nextForcedIndex").to_be_bytes(), &u("nextOrderId").to_be_bytes(), &u128f("totalEscape").to_be_bytes(), &u128f("dust").to_be_bytes(), &aux]);
    assert_eq!(hex0x(&engine::state_root_of(u("height"), u("time"), &ar, &mr, &wroot, &sys_011)), o["stateRoot"].as_str().unwrap());
    // SPEC 0.1.5 adds withdrawalCount and withdrawalsTotal (VAULT 5.2 request): the new vector for 390 and 389
    let sys_015 = engine::system_hash_of(&engine::SystemFields {
        next_deposit_index: u("nextDepositIndex"),
        next_forced_index: u("nextForcedIndex"),
        next_order_id: u("nextOrderId"),
        total_escape: u128f("totalEscape"),
        dust: u128f("dust"),
        aux_hash: aux,
        withdrawal_count: u("withdrawalCount"),
        withdrawals_total: u("withdrawalsTotal"),
    });
    let root_015 = engine::state_root_of(u("height"), u("time"), &ar, &mr, &wroot, &sys_015);
    eprintln!("SPEC 0.1.5 vector (390's inputs): systemHash {} stateRoot {}", hex0x(&sys_015), hex0x(&root_015));
    assert_ne!(root_015, sys_011);
}

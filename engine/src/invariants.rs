//! Invariants of SPEC section 8, checked independently of the code paths that maintain them.
use crate::engine::Engine;
use crate::num::N_PER_USDC;
use std::collections::BTreeMap;

pub fn check(e: &Engine) -> Result<(), String> {
    // I-1: sum of sizes per market is zero; long OI equals the sum of long sizes.
    let mut sum: BTreeMap<u32, i128> = BTreeMap::new();
    let mut longs: BTreeMap<u32, i128> = BTreeMap::new();
    let mut notional: i128 = 0;
    let mut balances: i128 = 0;
    for a in &e.accounts {
        balances += a.balance;
        for (mid, p) in &a.positions {
            if p.size == 0 {
                return Err(format!("zero-size position stored in market {mid}"));
            }
            *sum.entry(*mid).or_default() += p.size as i128;
            if p.size > 0 {
                *longs.entry(*mid).or_default() += p.size as i128;
            }
            notional += p.open_notional;
        }
    }
    for (mid, s) in &sum {
        if *s != 0 {
            return Err(format!("I-1 violated in market {mid}: sum of sizes {s}"));
        }
    }
    for (mid, m) in &e.markets {
        let l = longs.get(mid).copied().unwrap_or(0);
        if l != m.long_oi as i128 {
            return Err(format!("long OI mismatch in market {mid}: {} vs {l}", m.long_oi));
        }
        if l > m.p.oi_cap as i128 {
            return Err(format!("OI cap exceeded in market {mid}"));
        }
        // I-3: book not crossed, every resting order consistent.
        if m.book.is_crossed() {
            return Err(format!("I-3: book crossed in market {mid}"));
        }
        for (side, levels) in [(true, &m.book.bids), (false, &m.book.asks)] {
            for (p, q) in levels.iter() {
                if q.is_empty() {
                    return Err("empty level kept".into());
                }
                for id in q {
                    let o = e.orders.get(id).ok_or("book references a missing order")?;
                    if o.price != *p || o.is_buy != side || o.market != *mid || o.trigger.is_some() || o.remaining == 0 {
                        return Err(format!("I-3: order {id} inconsistent with its level"));
                    }
                    if o.price % m.p.tick != 0 || o.remaining % m.p.step != 0 {
                        return Err(format!("I-3: order {id} off tick/step"));
                    }
                    if !e.accounts[o.account as usize].orders.contains(id) {
                        return Err(format!("order {id} missing from its account"));
                    }
                }
            }
        }
    }
    // every order is either on its book or in its market's triggers, and in its account
    for (id, o) in &e.orders {
        let m = &e.markets[&o.market];
        let on_book = if o.is_buy { m.book.bids.get(&o.price) } else { m.book.asks.get(&o.price) }.is_some_and(|q| q.contains(id));
        let in_trig = m.triggers.contains(id);
        if on_book == in_trig {
            return Err(format!("order {id}: on_book={on_book} in_triggers={in_trig}"));
        }
    }
    for a in &e.accounts {
        for id in &a.orders {
            if !e.orders.contains_key(id) {
                return Err(format!("account lists missing order {id}"));
            }
        }
    }
    // I-2: exact conservation.
    let lhs = N_PER_USDC * balances + e.dust - notional;
    let rhs = N_PER_USDC * (e.total_deposits - e.total_withdrawn);
    if lhs != rhs {
        return Err(format!("I-2 violated: lhs {lhs} rhs {rhs} (diff {})", lhs - rhs));
    }
    if e.dust < 0 {
        return Err("dust negative".into());
    }
    // system accounts never negative
    for i in 0..3 {
        if e.accounts[i].balance < 0 {
            return Err(format!("system account {i} negative"));
        }
    }
    let wsum: i128 = e.withdrawals.iter().map(|w| w.amount as i128).sum();
    if wsum != e.total_withdrawn {
        return Err("withdrawal records do not sum to total_withdrawn".into());
    }
    Ok(())
}

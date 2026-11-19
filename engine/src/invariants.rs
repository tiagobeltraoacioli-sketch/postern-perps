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
    // PROTOCOL_FEE and INSURANCE are never negative (R395-17: the insurance fund pays as far as its balance goes,
    // never further). Restated in 0.2 (R430-1): BACKSTOP is NOT in this list. The old assertion "system accounts
    // 0..3 never negative" was false for a reachable state (test `r430_1_...`): the backstop holds positions, is
    // never liquidated, and realises a loss larger than its balance when ADL / a forced close / X3 closes them.
    // What holds instead, and is checked: a negative backstop balance takes no new position (`backstop_room`), is
    // counted in the escape deficit (below), and is paid by INSURANCE as bad debt in the same block as far as the
    // fund goes (0.2.3: whether or not the backstop still holds positions; the rest is a BadDebt event every block).
    for i in [crate::engine::ACCT_PROTOCOL_FEE, crate::engine::ACCT_INSURANCE] {
        if e.accounts[i as usize].balance < 0 {
            return Err(format!("system account {i} negative"));
        }
    }
    for i in [crate::engine::ACCT_PROTOCOL_FEE, crate::engine::ACCT_INSURANCE] {
        if !e.accounts[i as usize].positions.is_empty() || !e.accounts[i as usize].orders.is_empty() {
            return Err(format!("system account {i} holds a position or an order"));
        }
    }
    {
        let b = &e.accounts[crate::engine::ACCT_BACKSTOP as usize];
        if !b.orders.is_empty() {
            return Err("BACKSTOP holds an order".into());
        }
        let ins = e.accounts[crate::engine::ACCT_INSURANCE as usize].balance;
        if b.balance < 0 && ins > 0 {
            return Err("BACKSTOP with a negative balance while INSURANCE holds funds (realised loss not settled)".into());
        }
    }
    // SPEC 6.5 (0.2.5, I-6): after a block whose step 9 ran to completion the backstop holds no position in an
    // active market unless its equity is at or above its stop level (floor and own maintenance requirement)
    if e.last_passes.1 {
        let b = &e.accounts[crate::engine::ACCT_BACKSTOP as usize];
        let in_active = b.positions.keys().any(|m| e.markets[m].oracle.status == crate::oracle::Status::Active && e.markets[m].mark > 0);
        if in_active && e.backstop_equity() < e.backstop_stop_level() {
            return Err(format!("I-6: BACKSTOP holds a position in an active market with index-valued equity {} below its stop level {}", e.backstop_equity(), e.backstop_stop_level()));
        }
    }
    // the same for users: bad debt of a flat account is paid by the insurance fund as far as it goes
    {
        let ins = e.accounts[crate::engine::ACCT_INSURANCE as usize].balance;
        for (i, a) in e.accounts.iter().enumerate().skip(crate::engine::FIRST_USER as usize) {
            if a.balance < 0 && a.positions.is_empty() && ins > 0 {
                return Err(format!("account {i} flat with a negative balance while INSURANCE holds funds"));
            }
        }
    }
    if e.forced_processed + e.forced_queue.len() as u64 != e.next_forced {
        return Err("forced queue: processed + pending != received".into());
    }
    for (k, r) in e.forced_queue.iter().enumerate() {
        if r.index != e.forced_processed + k as u64 {
            return Err("forced queue out of index order".into());
        }
    }
    // R395-23: escape claims never exceed custody while the insurance fund covers the negative equity
    let esc = e.escape_values();
    let total: i128 = esc.iter().map(|x| *x as i128).sum();
    let deficit: i128 = (0..e.accounts.len() as u32).map(|i| e.equity(i)).filter(|x| *x < 0).map(|x| crate::num::div_ceil(-x, N_PER_USDC)).sum();
    let ins_e = e.equity(crate::engine::ACCT_INSURANCE).max(0) / N_PER_USDC;
    if ins_e >= deficit && total > e.total_deposits - e.total_withdrawn {
        return Err(format!("R395-23: escape total {total} > custody {} with insurance {ins_e} >= deficit {deficit}", e.total_deposits - e.total_withdrawn));
    }
    // 0.2, unconditional form of the same bound (SPEC 11): from I-1 and I-2 the signed equities sum to custody
    // minus dust, so the escape total can exceed custody by at most the deficit the insurance fund could not absorb.
    let ins_floor = (e.equity(crate::engine::ACCT_INSURANCE).max(0) / N_PER_USDC).min(u64::MAX as i128);
    let uncovered = (deficit - ins_floor).max(0);
    if total > e.total_deposits - e.total_withdrawn + uncovered {
        return Err(format!("escape total {total} > custody {} + uncovered deficit {uncovered}", e.total_deposits - e.total_withdrawn));
    }
    // 0.2: the indexes are exactly what the primary data implies (a snapshot rebuilds them from it)
    if e.by_addr.len() != e.accounts.len() {
        return Err("by_addr size".into());
    }
    let mut owners = 0usize;
    for (i, a) in e.accounts.iter().enumerate() {
        if e.by_addr.get(&a.address) != Some(&(i as u32)) {
            return Err(format!("by_addr does not map account {i}"));
        }
        if a.agents.len() > crate::engine::MAX_AGENTS {
            return Err(format!("account {i} has more than MAX_AGENTS agents"));
        }
        for ag in a.agents.keys() {
            if e.agent_owner.get(ag) != Some(&(i as u32)) {
                return Err(format!("agent_owner does not map an agent of account {i}"));
            }
            owners += 1;
        }
        for id in &a.orders {
            if e.orders[id].account != i as u32 {
                return Err(format!("order {id} listed by account {i} belongs to another"));
            }
        }
    }
    if owners != e.agent_owner.len() {
        return Err("agent_owner holds an agent no account lists".into());
    }
    for (mid, m) in &e.markets {
        for id in &m.triggers {
            match e.orders.get(id) {
                Some(o) if o.trigger.is_some() && o.market == *mid => {}
                _ => return Err(format!("trigger set of market {mid} lists {id} wrongly")),
            }
        }
    }
    for (i, a) in e.accounts.iter().enumerate() {
        if a.orders.len() > crate::engine::MAX_ORDERS_TOTAL {
            return Err(format!("account {i} holds more than MAX_ORDERS_TOTAL orders"));
        }
        let mut per_market: BTreeMap<u32, usize> = BTreeMap::new();
        for id in &a.orders {
            *per_market.entry(e.orders[id].market).or_default() += 1;
        }
        if per_market.values().any(|n| *n > crate::engine::MAX_ORDERS_PER_MARKET) {
            return Err(format!("account {i} holds more than MAX_ORDERS_PER_MARKET orders in a market"));
        }
    }
    for set in e.nonces.values() {
        if set.is_empty() || set.len() > crate::engine::NONCE_SET {
            return Err("nonce set size".into());
        }
    }
    for (k, w) in e.withdrawals.iter().enumerate() {
        if w.id != k as u64 {
            return Err("withdrawal ids are not their positions".into());
        }
    }
    let wsum: i128 = e.withdrawals.iter().map(|w| w.amount as i128).sum();
    if wsum != e.total_withdrawn {
        return Err("withdrawal records do not sum to total_withdrawn".into());
    }
    Ok(())
}

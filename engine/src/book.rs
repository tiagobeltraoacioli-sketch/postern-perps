//! Price-time priority book (SPEC section 4). Only ids live in the levels; orders live in the engine's order map.
use std::collections::{BTreeMap, VecDeque};

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Book {
    pub bids: BTreeMap<u64, VecDeque<u64>>,
    pub asks: BTreeMap<u64, VecDeque<u64>>,
}

impl Book {
    pub fn best_bid(&self) -> Option<u64> {
        self.bids.keys().next_back().copied()
    }
    pub fn best_ask(&self) -> Option<u64> {
        self.asks.keys().next().copied()
    }
    pub fn insert(&mut self, is_buy: bool, price: u64, id: u64) {
        let side = if is_buy { &mut self.bids } else { &mut self.asks };
        side.entry(price).or_default().push_back(id);
    }
    pub fn remove(&mut self, is_buy: bool, price: u64, id: u64) -> bool {
        let side = if is_buy { &mut self.bids } else { &mut self.asks };
        let mut found = false;
        if let Some(q) = side.get_mut(&price) {
            if let Some(pos) = q.iter().position(|x| *x == id) {
                q.remove(pos);
                found = true;
            }
            if q.is_empty() {
                side.remove(&price);
            }
        }
        found
    }
    /// Front order of the best opposite level that a taker with `limit` may hit.
    pub fn best_maker(&self, taker_is_buy: bool, limit: u64) -> Option<(u64, u64)> {
        if taker_is_buy {
            let (p, q) = self.asks.iter().next()?;
            if *p <= limit {
                return Some((*p, *q.front()?));
            }
        } else {
            let (p, q) = self.bids.iter().next_back()?;
            if *p >= limit {
                return Some((*p, *q.front()?));
            }
        }
        None
    }
    pub fn is_crossed(&self) -> bool {
        matches!((self.best_bid(), self.best_ask()), (Some(b), Some(a)) if b >= a)
    }
}

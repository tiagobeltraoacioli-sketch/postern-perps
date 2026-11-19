//! What the sequencer's choice of observations can do to the committee median (README section 5a, R395-9), and the
//! check a validator can run against it. Pure functions; nothing here changes the acceptance rule C1 to C5.
//!
//! The sequencer puts an `OracleUpdate` in a block and decides which of the valid observations it holds go in
//! (at least `q`). The engine cannot see what was left out. [`selection_range`] is the exact set of medians that
//! choice can reach for a market, given every observation that existed; [`omitted`] is the evidence a validator
//! has when its own acknowledged observation is not in the block.

use crate::types::{OracleUpdate, SignedObservation};

/// The median of rule C3: `sort(v)[len / 2]` (the upper median for an even count). `None` for an empty slice.
pub fn median(values: &[u64]) -> Option<u64> {
    let mut v = values.to_vec();
    v.sort_unstable();
    v.get(v.len() / 2).copied()
}

/// The reach of the sequencer's choice for one market in one round.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SelectionRange {
    /// The lowest median any subset of at least `q` values gives.
    pub low: u64,
    /// The median of all the values (what a sequencer that includes everything gets).
    pub full: u64,
    /// The highest median any subset of at least `q` values gives.
    pub high: u64,
}

/// For `n = all.len()` valid values of one market and quorum `q`: every subset `S` with `|S| >= q` has its C3
/// median between the order statistics `x[q/2]` and `x[n - q + q/2]` of the sorted values (0-based), and both ends
/// are reached (drop the `n - q` highest, or the `n - q` lowest). With `n = 7`, `q = 5`: between the third and the
/// fifth lowest of the seven, the honest median being the fourth. `None` when `q == 0` or `n < q`.
///
/// Proof sketch (the test `selection_range_is_exact` checks it against every subset): a subset of size `s` has
/// median `S[s/2]`, which has at least `s/2` subset values below or equal, so it is at least `x[s/2] >= x[q/2]`;
/// and at least `s - s/2 - 1` subset values above or equal, so it is at most `x[n - 1 - (s - s/2 - 1)] =
/// x[n - (s - s/2)] <= x[n - (q - q/2)] = x[n - q + q/2]`.
pub fn selection_range(all: &[u64], q: usize) -> Option<SelectionRange> {
    let n = all.len();
    if q == 0 || n < q {
        return None;
    }
    let mut x = all.to_vec();
    x.sort_unstable();
    Some(SelectionRange {
        low: x[q / 2],
        full: x[n / 2],
        high: x[n - q + q / 2],
    })
}

/// The signers among `held` (observations of the update's round that a validator knows the sequencer received in
/// time: its own, acknowledged by the sequencer) that the update leaves out or carries with other content.
/// Non-empty = the sequencer selected. Observations of other rounds are ignored.
pub fn omitted(update: &OracleUpdate, round_of_held: u64, held: &[SignedObservation]) -> Vec<u8> {
    if round_of_held != update.round {
        return Vec::new();
    }
    let mut out: Vec<u8> = held
        .iter()
        .filter(|h| !update.observations.iter().any(|o| o == *h))
        .map(|h| h.signer)
        .collect();
    out.sort_unstable();
    out.dedup();
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::keccak256;
    use crate::types::OraclePrice;

    /// Deterministic pseudo-random values (no randomness in this crate): keccak of a counter.
    fn values(seed: u32, n: usize, spread: u64) -> Vec<u64> {
        (0..n)
            .map(|i| {
                let h = keccak256(&[seed.to_be_bytes(), (i as u32).to_be_bytes()].concat());
                1_000_000 + u64::from_be_bytes(h[..8].try_into().unwrap()) % spread
            })
            .collect()
    }

    /// Exhaustive: for every committee size up to 9, every strict-majority quorum and 40 value sets each (ties
    /// included: a spread of 4 forces equal values), the lowest and highest median over ALL subsets of at least
    /// `q` values are exactly `selection_range`'s, and the full median lies between them.
    #[test]
    fn selection_range_is_exact() {
        let mut subsets_checked = 0u64;
        for n in 1..=9usize {
            for q in (n / 2 + 1)..=n {
                for seed in 0..40u32 {
                    let spread = if seed % 4 == 0 { 4 } else { 10_000 };
                    let all = values(seed * 100 + n as u32 * 10 + q as u32, n, spread);
                    let r = selection_range(&all, q).unwrap();
                    let (mut lo, mut hi) = (u64::MAX, 0u64);
                    for mask in 1u32..(1 << n) {
                        if (mask.count_ones() as usize) < q {
                            continue;
                        }
                        let s: Vec<u64> = (0..n)
                            .filter(|i| mask >> i & 1 == 1)
                            .map(|i| all[i])
                            .collect();
                        let m = median(&s).unwrap();
                        lo = lo.min(m);
                        hi = hi.max(m);
                        subsets_checked += 1;
                    }
                    assert_eq!(
                        (lo, hi),
                        (r.low, r.high),
                        "n {n} q {q} seed {seed}: {all:?}"
                    );
                    assert!(r.low <= r.full && r.full <= r.high);
                    assert_eq!(r.full, median(&all).unwrap());
                }
            }
        }
        assert!(subsets_checked > 20_000);
    }

    #[test]
    fn default_committee_reach_is_one_order_statistic_each_way() {
        // n = 7, q = 5: between the third and the fifth lowest; including everything gives the fourth.
        let r = selection_range(&[70, 10, 50, 30, 60, 20, 40], 5).unwrap();
        assert_eq!((r.low, r.full, r.high), (30, 40, 50));
        // q = n: no choice at all.
        let r = selection_range(&[70, 10, 50, 30, 60, 20, 40], 7).unwrap();
        assert_eq!((r.low, r.full, r.high), (40, 40, 40));
        // q = 6 of 7: the upper median of six leaves [fourth, fifth].
        let r = selection_range(&[70, 10, 50, 30, 60, 20, 40], 6).unwrap();
        assert_eq!((r.low, r.full, r.high), (40, 40, 50));
        assert_eq!(selection_range(&[1, 2, 3, 4], 5), None);
        assert_eq!(selection_range(&[1, 2, 3, 4], 0), None);
        assert_eq!(median(&[]), None);
    }

    fn obs(signer: u8, price: u64) -> SignedObservation {
        SignedObservation {
            signer,
            time_ms: 30_001,
            prices: vec![OraclePrice {
                market_id: 1,
                price,
                sources: 5,
            }],
            signature: [signer; 65],
        }
    }

    #[test]
    fn omitted_names_what_the_sequencer_left_out_or_altered() {
        let u = OracleUpdate {
            round: 10,
            observations: (0..5).map(|i| obs(i, 100)).collect(),
        };
        // Included as held: nothing to report.
        assert_eq!(omitted(&u, 10, &[obs(2, 100)]), Vec::<u8>::new());
        // Held and acknowledged, not in the block.
        assert_eq!(
            omitted(&u, 10, &[obs(6, 100), obs(2, 100), obs(5, 100)]),
            vec![5, 6]
        );
        // Same signer, other content (cannot happen with a valid signature unless the signer signed twice).
        assert_eq!(omitted(&u, 10, &[obs(3, 101)]), vec![3]);
        // Another round's observation says nothing about this update.
        assert_eq!(omitted(&u, 11, &[obs(6, 100)]), Vec::<u8>::new());
    }
}

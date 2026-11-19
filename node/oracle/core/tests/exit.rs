//! R395-5 (High, reviewer 395; coordinator decision 04:59 UTC): a halt the sequencer can cause alone (by
//! withholding oracle inputs) must not leave exits without a price. The oracle gives every market that ever had an
//! index a defined exit price path in every state (README section 6a, X1 to X3). These tests were written first
//! and failed against the stub `exit_price` (returning `None`) of the previous commit.

use k256::ecdsa::SigningKey;
use perps_oracle::committee::*;
use perps_oracle::types::*;
use perps_oracle::{eip712, keccak256, sig, ROUND_MS};

const VAULT: [u8; 20] = [0xdb; 20];

fn key(i: u8) -> SigningKey {
    SigningKey::from_bytes(
        &keccak256(format!("postern-perps-oracle-test-signer-{i}").as_bytes()).into(),
    )
    .unwrap()
}

fn params() -> CommitteeParams {
    CommitteeParams {
        verifying_contract: VAULT,
        signers: (0..7)
            .map(|i| sig::address_of(key(i).verifying_key()))
            .collect(),
        quorum: 5,
        max_future_ms: 1000,
        max_age_ms: 6000,
        markets: vec![MarketParams {
            market_id: 1,
            min_sources: 3,
            agree_bps: 100,
            jump_bps: 500,
            max_staleness_ms: 10_000,
            confirm_rounds: 3,
            exit_haircut_bps: 500,
            exit_haircut_per_min_bps: 100,
            exit_max_haircut_bps: 5000,
            max_halt_ms: 3_600_000,
        }],
    }
}

const R0: u64 = 598_296_000;

fn upd(round: u64, p: u64) -> OracleUpdate {
    let dom = eip712::domain_separator(&VAULT);
    OracleUpdate {
        round,
        observations: (0..7u8)
            .map(|i| {
                let o = Observation {
                    round,
                    time_ms: round * ROUND_MS + 100,
                    prices: vec![OraclePrice {
                        market_id: 1,
                        price: p,
                        sources: 5,
                    }],
                };
                SignedObservation {
                    signer: i,
                    time_ms: o.time_ms,
                    prices: o.prices.clone(),
                    signature: sig::sign(&key(i), &eip712::digest(&dom, &o)),
                }
            })
            .collect(),
    }
}

fn t_of(r: u64) -> u64 {
    r * ROUND_MS + 400
}

/// Active at `p` after three rounds; returns the next round.
fn active(p: u64) -> (CommitteeParams, OracleState, u64) {
    let prm = params();
    let mut st = OracleState::genesis(&prm).unwrap();
    for k in 0..3 {
        begin_block(&prm, &mut st, t_of(R0 + k));
        apply_update(&prm, &mut st, &upd(R0 + k, p), t_of(R0 + k)).unwrap();
    }
    (prm, st, R0 + 3)
}

#[test]
fn genesis_has_no_exit_price_because_nothing_can_have_traded() {
    let prm = params();
    let st = OracleState::genesis(&prm).unwrap();
    assert_eq!(exit_price(&prm, &st, 1, t_of(R0)), None);
    assert_eq!(exit_price(&prm, &st, 2, t_of(R0)), None, "unknown market");
}

#[test]
fn active_market_exits_at_the_index() {
    let (prm, st, r) = active(650_000);
    let x = exit_price(&prm, &st, 1, t_of(r)).unwrap();
    assert_eq!(
        (x.source, x.low, x.high, x.reference, x.settle),
        (ExitSource::Index, 650_000, 650_000, 650_000, 650_000)
    );
    assert!(!x.settle_due);
}

/// The R395-5 scenario: the sequencer stops including oracle inputs. Before: `safe_index` is `None` for ever and
/// no exit has a price. Now: a band around the last good index, widening with time, and settlement due after
/// `max_halt_ms`.
#[test]
fn withheld_oracle_inputs_leave_a_widening_band_and_a_settlement_deadline() {
    let (prm, mut st, _) = active(650_000);
    let it = st.market(1).unwrap().index_time_ms;
    begin_block(&prm, &mut st, it + 10_001);
    assert_eq!(st.safe_index(1), None);
    let x = exit_price(&prm, &st, 1, it + 10_001).unwrap();
    assert_eq!(x.source, ExitSource::HaltBand);
    // 5% half-width in the first minute: low rounded down, high rounded up.
    assert_eq!(
        (x.low, x.high, x.reference, x.settle),
        (617_500, 682_500, 650_000, 650_000)
    );
    assert_eq!(x.age_ms, 10_001);
    // 10 whole minutes after the last good index: 5% + 10 * 1% = 15%.
    let x = exit_price(&prm, &st, 1, it + 600_000).unwrap();
    assert_eq!((x.low, x.high), (552_500, 747_500));
    // Capped at 50%.
    let x = exit_price(&prm, &st, 1, it + 3_599_999).unwrap();
    assert_eq!((x.low, x.high), (325_000, 975_000));
    assert!(!x.settle_due);
    let x = exit_price(&prm, &st, 1, it + 3_600_000).unwrap();
    assert!(
        x.settle_due,
        "one hour without a good index: settlement due"
    );
}

#[test]
fn rounding_is_outward() {
    let (prm, mut st, _) = active(650_001);
    let it = st.market(1).unwrap().index_time_ms;
    begin_block(&prm, &mut st, it + 10_001);
    let x = exit_price(&prm, &st, 1, it + 10_001).unwrap();
    // 650001 * 0.95 = 617500.95 -> 617500; 650001 * 1.05 = 682501.05 -> 682502.
    assert_eq!((x.low, x.high), (617_500, 682_502));
}

/// A crash the committee stands behind (a jump halt): the band covers the crash price, so a long is never valued
/// above what the committee has signed since the halt; settlement uses the latest committee price.
#[test]
fn a_jump_halt_band_covers_every_committee_price_since_the_halt() {
    let (prm, mut st, r) = active(650_000);
    let out = apply_update(&prm, &mut st, &upd(r, 520_000), t_of(r)).unwrap();
    assert_eq!(
        out.outcomes[0].1,
        MarketOutcome::HaltedJump { price: 520_000 }
    );
    let x = exit_price(&prm, &st, 1, t_of(r)).unwrap();
    assert_eq!(
        (x.low, x.high, x.reference, x.settle),
        (494_000, 682_500, 650_000, 520_000)
    );
    // Next round the committee sees 700000 (an oscillation): the band now covers both extremes.
    begin_block(&prm, &mut st, t_of(r + 1));
    apply_update(&prm, &mut st, &upd(r + 1, 700_000), t_of(r + 1)).unwrap();
    let x = exit_price(&prm, &st, 1, t_of(r + 1)).unwrap();
    assert_eq!((x.low, x.high, x.settle), (494_000, 735_000, 700_000));
    // The sequencer cannot narrow the band by including only near-index rounds later: it only widens.
    begin_block(&prm, &mut st, t_of(r + 5));
    apply_update(&prm, &mut st, &upd(r + 5, 600_000), t_of(r + 5)).unwrap();
    let x = exit_price(&prm, &st, 1, t_of(r + 5)).unwrap();
    assert!(x.low <= 494_000 && x.high >= 735_000, "{x:?}");
    // Resume (confirmed) clears the halt extremes: active again, exits at the index.
    for k in 6..8 {
        begin_block(&prm, &mut st, t_of(r + k));
        apply_update(&prm, &mut st, &upd(r + k, 600_000), t_of(r + k)).unwrap();
    }
    assert_eq!(st.safe_index(1), Some(600_000));
    let m = st.market(1).unwrap();
    assert_eq!((m.halt_low, m.halt_high), (0, 0));
    assert_eq!(
        exit_price(&prm, &st, 1, t_of(r + 7)).unwrap().source,
        ExitSource::Index
    );
}

#[test]
fn halt_extremes_are_part_of_the_state_bytes() {
    let (prm, mut st, r) = active(650_000);
    let a = st.clone();
    apply_update(&prm, &mut st, &upd(r, 520_000), t_of(r)).unwrap();
    let mut b = a.clone();
    apply_update(&prm, &mut b, &upd(r, 510_000), t_of(r)).unwrap();
    assert_ne!(st.encode(), b.encode());
    assert_eq!(st.market(1).unwrap().halt_low, 520_000);
}

/// R436-10 (High, reviewer 436, round 2). Round 1's README 6a proposed that a forced close in a halted market
/// settles at `settle`. With no committee price since the halt, `settle` is the frozen index: a sequencer that
/// stops including oracle inputs when the market turns against its position, and then serves its own forced
/// close, leaves at the pre-move price. The price of ONE account's exit is the side of the band worse for it;
/// `settle` is for X3 only, and says whether it is a committee price at all.
#[test]
fn r436_10_one_accounts_exit_never_gets_the_frozen_index() {
    let (prm, mut st, _) = active(650_000);
    let it = st.market(1).unwrap().index_time_ms;
    // The sequencer goes dark; 10.001 s later the market is STALE. The real price is now 4% higher (676_000):
    // nobody in the engine knows.
    begin_block(&prm, &mut st, it + 10_001);
    let x = exit_price(&prm, &st, 1, it + 10_001).unwrap();
    // The option the round-1 text gave away: `settle` is still the pre-move index, and no committee price backs it.
    assert_eq!((x.settle, x.settle_committee_time_ms), (650_000, None));
    // A short leaving alone buys back at `high`: at least one permitted step (jumpBps = 5%) above the frozen
    // index, so above the real price it is running from; a long sells at `low`.
    assert_eq!(x.close_price(false), 682_500);
    assert_eq!(x.close_price(true), 617_500);
    assert!(x.close_price(false) > 676_000 && x.close_price(false) > x.settle);
    assert!(x.close_price(true) < x.settle);
    // Waiting does not help the one who waits: the band only widens with the age of the index.
    let later = exit_price(&prm, &st, 1, it + 1_800_000).unwrap();
    assert!(later.close_price(false) > x.close_price(false));
    assert!(later.close_price(true) < x.close_price(true));
    // At the X3 deadline with nothing signed since the halt, `settle` is still not a committee price.
    let due = exit_price(&prm, &st, 1, it + 3_600_000).unwrap();
    assert!(due.settle_due);
    assert_eq!((due.settle, due.settle_committee_time_ms), (650_000, None));
    // While active there is one price and no band.
    let (prm, st, r) = active(650_000);
    let a = exit_price(&prm, &st, 1, t_of(r)).unwrap();
    assert_eq!(
        (
            a.close_price(true),
            a.close_price(false),
            a.settle_committee_time_ms
        ),
        (650_000, 650_000, None)
    );
}

/// When the committee HAS signed since the halt, `settle` is that price and carries its time; one account's exit
/// is still priced at the band, which covers it.
#[test]
fn r436_10_settle_names_the_committee_price_and_its_time() {
    let (prm, mut st, r) = active(650_000);
    apply_update(&prm, &mut st, &upd(r, 520_000), t_of(r)).unwrap();
    let x = exit_price(&prm, &st, 1, t_of(r)).unwrap();
    assert_eq!(
        (x.settle, x.settle_committee_time_ms),
        (520_000, Some(r * ROUND_MS))
    );
    assert!(x.close_price(true) <= 520_000 && x.close_price(false) >= 650_000);
    // A later committee price replaces it.
    begin_block(&prm, &mut st, t_of(r + 1));
    apply_update(&prm, &mut st, &upd(r + 1, 700_000), t_of(r + 1)).unwrap();
    let x = exit_price(&prm, &st, 1, t_of(r + 1)).unwrap();
    assert_eq!(
        (x.settle, x.settle_committee_time_ms),
        (700_000, Some((r + 1) * ROUND_MS))
    );
    assert!(x.close_price(true) <= 520_000 && x.close_price(false) >= 700_000);
}

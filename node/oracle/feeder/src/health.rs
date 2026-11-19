//! The drop rule for a failing venue (README section 7a, D1 to D3). Pure: rounds in, decisions out; no clock.
//!
//! A venue that fails is not a venue that lies: a wrong price stays in the round and is handled by F4/F5 (outlier,
//! strict majority) where everybody can see it. This rule is only about a venue that does not answer, answers with
//! an error, or answers something its adapter refuses: asking it again every 3 s gains nothing and, when the venue
//! is rate-limiting us, makes it worse.
//!
//! - D1 a fetch unit (one URL: one pair of one venue, or one batched request) FAILS in a round when the request
//!   fails (timeout, TLS, any HTTP status other than 200) or every source reading that body is refused by its
//!   adapter. It is REFUSED when the status is 429, 418 or 403 (the venue is telling us to stop).
//! - D2 after `DROP_AFTER_FAILS` consecutive failed rounds, or at once when refused, the unit is DROPPED: not
//!   requested for the cooldown, `COOLDOWN_ROUNDS` the first time, doubling at each new drop without a success in
//!   between, at most `MAX_COOLDOWN_ROUNDS`. After the cooldown one probe is sent; a failed probe drops again.
//! - D3 one success resets the failure count and the cooldown.
//!
//! A dropped unit's sources count as `dropped` readings (not valid) in every round of the cooldown: `minSources`
//! and the strict-majority rule F5 are unchanged, so dropping can only make the feeder abstain, never lower its bar.

pub const DROP_AFTER_FAILS: u32 = 5;
pub const COOLDOWN_ROUNDS: u64 = 20;
pub const MAX_COOLDOWN_ROUNDS: u64 = 1200;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Outcome {
    Ok,
    Failed,
    /// HTTP 429, 418 or 403.
    Refused,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Health {
    consecutive: u32,
    cooldown: u64,
    /// First round in which the unit is requested again.
    resume_round: u64,
    pub drops: u64,
    pub failures: u64,
    pub successes: u64,
}

impl Default for Health {
    fn default() -> Self {
        Health {
            consecutive: 0,
            cooldown: COOLDOWN_ROUNDS,
            resume_round: 0,
            drops: 0,
            failures: 0,
            successes: 0,
        }
    }
}

impl Health {
    /// D2: is the unit requested in `round`?
    pub fn should_fetch(&self, round: u64) -> bool {
        round >= self.resume_round
    }

    /// Records the outcome of the request made in `round`. Returns the round at which the unit resumes when this
    /// outcome dropped it.
    pub fn record(&mut self, round: u64, outcome: Outcome) -> Option<u64> {
        match outcome {
            Outcome::Ok => {
                self.successes += 1;
                self.consecutive = 0;
                self.cooldown = COOLDOWN_ROUNDS;
                None
            }
            Outcome::Failed | Outcome::Refused => {
                self.failures += 1;
                self.consecutive = self.consecutive.saturating_add(1);
                if outcome == Outcome::Refused || self.consecutive >= DROP_AFTER_FAILS {
                    self.drops += 1;
                    self.resume_round = round.saturating_add(1).saturating_add(self.cooldown);
                    self.cooldown = self.cooldown.saturating_mul(2).min(MAX_COOLDOWN_ROUNDS);
                    // The probe after the cooldown is one strike from the next drop.
                    self.consecutive = DROP_AFTER_FAILS - 1;
                    Some(self.resume_round)
                } else {
                    None
                }
            }
        }
    }
}

/// D1: the outcome of a fetch unit from its HTTP status (0 = no response) and whether any source accepted the body.
pub fn classify(status: u16, any_source_parsed: bool) -> Outcome {
    match status {
        429 | 418 | 403 => Outcome::Refused,
        200 if any_source_parsed => Outcome::Ok,
        _ => Outcome::Failed,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn drops_after_five_failures_and_backs_off() {
        let mut h = Health::default();
        for r in 0..4 {
            assert!(h.should_fetch(r));
            assert_eq!(h.record(r, Outcome::Failed), None);
        }
        // D2: the fifth consecutive failure drops for 20 rounds.
        assert_eq!(h.record(4, Outcome::Failed), Some(25));
        for r in 5..25 {
            assert!(!h.should_fetch(r), "round {r}");
        }
        assert!(h.should_fetch(25));
        // A failed probe drops again at once, for twice as long.
        assert_eq!(h.record(25, Outcome::Failed), Some(66));
        assert!(!h.should_fetch(65));
        assert!(h.should_fetch(66));
        // The cooldown is capped.
        let mut r = 66;
        for _ in 0..12 {
            r = h.record(r, Outcome::Failed).unwrap();
        }
        assert_eq!(
            h.record(r, Outcome::Failed),
            Some(r + 1 + MAX_COOLDOWN_ROUNDS)
        );
    }

    #[test]
    fn success_resets_and_refusal_drops_at_once() {
        let mut h = Health::default();
        for r in 0..4 {
            h.record(r, Outcome::Failed);
        }
        // D3: a success before the fifth failure clears the count.
        assert_eq!(h.record(4, Outcome::Ok), None);
        for r in 5..9 {
            assert_eq!(h.record(r, Outcome::Failed), None);
        }
        assert_eq!(h.record(9, Outcome::Ok), None);
        // Refused (rate limit): dropped on the first one.
        assert_eq!(h.record(10, Outcome::Refused), Some(31));
        // The probe succeeds: cooldown back to the base.
        assert_eq!(h.record(31, Outcome::Ok), None);
        assert_eq!(h.record(32, Outcome::Refused), Some(53));
        assert_eq!((h.drops, h.failures, h.successes), (2, 10, 3));
    }

    #[test]
    fn classification() {
        assert_eq!(classify(200, true), Outcome::Ok);
        // A 200 whose body no adapter accepts is a failure (the venue changed its format, or sent an error body).
        assert_eq!(classify(200, false), Outcome::Failed);
        assert_eq!(classify(0, false), Outcome::Failed);
        assert_eq!(classify(500, false), Outcome::Failed);
        for s in [429, 418, 403] {
            assert_eq!(classify(s, false), Outcome::Refused);
        }
    }
}

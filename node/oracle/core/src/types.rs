//! The oracle's data types (README sections 3 and 4).

/// One market's price in one observation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct OraclePrice {
    pub market_id: u32,
    /// USD per one whole base unit, in the market's price scale; never 0.
    pub price: u64,
    /// Venues that survived the feeder's rule F5.
    pub sources: u8,
}

/// What one oracle signer signs (EIP-712 primary type `OracleObservation`).
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct Observation {
    pub round: u64,
    pub time_ms: u64,
    pub prices: Vec<OraclePrice>,
}

/// An observation as carried inside an [`OracleUpdate`]: the round is the update's, the signer is an index into the
/// oracle committee fixed at genesis.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct SignedObservation {
    pub signer: u8,
    pub time_ms: u64,
    pub prices: Vec<OraclePrice>,
    pub signature: [u8; 65],
}

impl SignedObservation {
    /// The message this entry claims to be signed: `OracleObservation{round, timeMs, prices}`.
    pub fn message(&self, round: u64) -> Observation {
        Observation {
            round,
            time_ms: self.time_ms,
            prices: self.prices.clone(),
        }
    }
}

/// The block input of kind `oracle`.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct OracleUpdate {
    pub round: u64,
    pub observations: Vec<SignedObservation>,
}

/// The round that contains a millisecond time.
pub fn round_of(time_ms: u64) -> u64 {
    time_ms / crate::ROUND_MS
}

//! Canonical bytes of the oracle block input (README section 4).
//!
//! ```text
//! OracleUpdate      = round:u64be  count:u8  SignedObservation{count}
//! SignedObservation = signer:u8  timeMs:u64be  priceCount:u16be  OraclePrice{priceCount}  signature:65 bytes
//! OraclePrice       = marketId:u32be  price:u64be  sources:u8
//! ```
//!
//! The decoder is strict and bounded: it refuses a truncated input, trailing bytes, a `priceCount` above
//! [`crate::MAX_PRICES`], and allocates nothing before the length is known to be present. It checks structure
//! only; the semantic rules (ordering, quorum, signatures, time) are [`crate::committee::apply_update`]'s, so a
//! structurally valid but semantically wrong input reaches the engine and is rejected there with a named error.
//! `encode(decode(b)) == b` for every accepted `b` (tested), so the bytes the log hashes are unique per value.

use crate::types::{OraclePrice, OracleUpdate, SignedObservation};
use crate::MAX_PRICES;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DecodeError {
    Truncated,
    TrailingBytes,
    TooManyPrices,
}

/// Bytes of one `OraclePrice`.
pub const PRICE_LEN: usize = 4 + 8 + 1;
/// Fixed bytes of one `SignedObservation` without its prices.
pub const OBS_FIXED_LEN: usize = 1 + 8 + 2 + 65;

pub fn encode(u: &OracleUpdate) -> Vec<u8> {
    assert!(u.observations.len() <= 255, "encode: more than 255 observations");
    let mut out = Vec::with_capacity(9 + u.observations.iter().map(|o| OBS_FIXED_LEN + o.prices.len() * PRICE_LEN).sum::<usize>());
    out.extend_from_slice(&u.round.to_be_bytes());
    out.push(u.observations.len() as u8);
    for o in &u.observations {
        assert!(o.prices.len() <= MAX_PRICES, "encode: more than MAX_PRICES prices");
        out.push(o.signer);
        out.extend_from_slice(&o.time_ms.to_be_bytes());
        out.extend_from_slice(&(o.prices.len() as u16).to_be_bytes());
        for p in &o.prices {
            out.extend_from_slice(&p.market_id.to_be_bytes());
            out.extend_from_slice(&p.price.to_be_bytes());
            out.push(p.sources);
        }
        out.extend_from_slice(&o.signature);
    }
    out
}

struct Reader<'a> {
    b: &'a [u8],
    at: usize,
}

impl<'a> Reader<'a> {
    fn take(&mut self, n: usize) -> Result<&'a [u8], DecodeError> {
        let end = self.at.checked_add(n).ok_or(DecodeError::Truncated)?;
        if end > self.b.len() {
            return Err(DecodeError::Truncated);
        }
        let s = &self.b[self.at..end];
        self.at = end;
        Ok(s)
    }
    fn u8(&mut self) -> Result<u8, DecodeError> {
        Ok(self.take(1)?[0])
    }
    fn u16(&mut self) -> Result<u16, DecodeError> {
        Ok(u16::from_be_bytes(self.take(2)?.try_into().unwrap()))
    }
    fn u32(&mut self) -> Result<u32, DecodeError> {
        Ok(u32::from_be_bytes(self.take(4)?.try_into().unwrap()))
    }
    fn u64(&mut self) -> Result<u64, DecodeError> {
        Ok(u64::from_be_bytes(self.take(8)?.try_into().unwrap()))
    }
}

pub fn decode(b: &[u8]) -> Result<OracleUpdate, DecodeError> {
    let mut r = Reader { b, at: 0 };
    let round = r.u64()?;
    let count = r.u8()?;
    let mut observations = Vec::with_capacity(usize::from(count));
    for _ in 0..count {
        let signer = r.u8()?;
        let time_ms = r.u64()?;
        let n = usize::from(r.u16()?);
        if n > MAX_PRICES {
            return Err(DecodeError::TooManyPrices);
        }
        // Check the bytes exist before allocating for them.
        let price_bytes = r.take(n * PRICE_LEN)?;
        let mut pr = Reader { b: price_bytes, at: 0 };
        let mut prices = Vec::with_capacity(n);
        for _ in 0..n {
            prices.push(OraclePrice { market_id: pr.u32()?, price: pr.u64()?, sources: pr.u8()? });
        }
        let signature: [u8; 65] = r.take(65)?.try_into().unwrap();
        observations.push(SignedObservation { signer, time_ms, prices, signature });
    }
    if r.at != b.len() {
        return Err(DecodeError::TrailingBytes);
    }
    Ok(OracleUpdate { round, observations })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample() -> OracleUpdate {
        OracleUpdate {
            round: 598_296_123,
            observations: vec![
                SignedObservation {
                    signer: 0,
                    time_ms: 1_794_888_369_100,
                    prices: vec![
                        OraclePrice { market_id: 1, price: 648_703, sources: 7 },
                        OraclePrice { market_id: 3, price: 72_660, sources: 5 },
                    ],
                    signature: [0x11; 65],
                },
                SignedObservation { signer: 4, time_ms: 1_794_888_369_900, prices: vec![], signature: [0x22; 65] },
            ],
        }
    }

    #[test]
    fn round_trip_and_layout() {
        let u = sample();
        let b = encode(&u);
        assert_eq!(b.len(), 9 + (OBS_FIXED_LEN + 2 * PRICE_LEN) + OBS_FIXED_LEN);
        assert_eq!(&b[..8], &598_296_123u64.to_be_bytes());
        assert_eq!(b[8], 2);
        assert_eq!(decode(&b).unwrap(), u);
        assert_eq!(encode(&decode(&b).unwrap()), b);
    }

    #[test]
    fn every_truncation_and_extension_is_refused() {
        let b = encode(&sample());
        for cut in 0..b.len() {
            assert_eq!(decode(&b[..cut]), Err(DecodeError::Truncated), "cut at {cut}");
        }
        let mut longer = b.clone();
        longer.push(0);
        assert_eq!(decode(&longer), Err(DecodeError::TrailingBytes));
    }

    #[test]
    fn price_count_is_bounded_before_allocation() {
        let mut b = Vec::new();
        b.extend_from_slice(&1u64.to_be_bytes());
        b.push(1);
        b.push(0);
        b.extend_from_slice(&0u64.to_be_bytes());
        b.extend_from_slice(&(MAX_PRICES as u16 + 1).to_be_bytes());
        assert_eq!(decode(&b), Err(DecodeError::TooManyPrices));
        // 256 claimed prices with no bytes behind them: truncated, refused before any allocation for them.
        let n = b.len();
        b[n - 2..].copy_from_slice(&256u16.to_be_bytes());
        assert_eq!(decode(&b), Err(DecodeError::Truncated));
    }
}

//! Fixed scales and rounding (SPEC section 1, S-0.2, S-0.3).

/// 1 price unit = 1e-8 USD per whole base unit.
pub const PRICE_SCALE: u64 = 100_000_000;
/// 1 size unit = 1e-8 base unit.
pub const SIZE_SCALE: u64 = 100_000_000;
/// 1 collateral unit = 1e-6 USDC.
pub const USDC_SCALE: u64 = 1_000_000;
/// Notional units (price x size, 1e-16 USD) per collateral unit (1e-6 USDC).
pub const N_PER_USDC: i128 = 10_000_000_000;
pub const MAX_PRICE: u64 = 1_000_000_000_000_000;
pub const MAX_SIZE: u64 = 1_000_000_000_000_000;
pub const PPM: i128 = 1_000_000;
pub const RATE_SCALE: i128 = 1_000_000_000;
/// Bound on balances (S-1 table).
pub const MAX_BALANCE: i128 = 1 << 62;

/// Floor division (toward negative infinity). `b > 0`.
pub fn div_floor(a: i128, b: i128) -> i128 {
    debug_assert!(b > 0);
    let q = a / b;
    if a % b != 0 && a < 0 {
        q - 1
    } else {
        q
    }
}

/// Ceiling division (toward positive infinity). `b > 0`.
pub fn div_ceil(a: i128, b: i128) -> i128 {
    -div_floor(-a, b)
}

/// S-0.3: convert a signed notional amount owed TO the account (negative = owed BY it) into collateral units,
/// rounding toward the protocol (floor in both directions: a gain is rounded down, a loss rounded up in size).
/// Returns `(usdc, dust)` with `usdc * N_PER_USDC + dust == x` and `0 <= dust < N_PER_USDC`.
pub fn to_usdc_toward_protocol(x: i128) -> (i128, i128) {
    let u = div_floor(x, N_PER_USDC);
    (u, x - u * N_PER_USDC)
}

/// Median of three.
pub fn median3(a: u64, b: u64, c: u64) -> u64 {
    let mut v = [a, b, c];
    v.sort_unstable();
    v[1]
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn floor_ceil() {
        assert_eq!(div_floor(7, 2), 3);
        assert_eq!(div_floor(-7, 2), -4);
        assert_eq!(div_ceil(7, 2), 4);
        assert_eq!(div_ceil(-7, 2), -3);
        assert_eq!(div_floor(-8, 2), -4);
    }
    #[test]
    fn toward_protocol() {
        // gain 1.5 units -> 1, dust 0.5
        let (u, d) = to_usdc_toward_protocol(15_000_000_000);
        assert_eq!((u, d), (1, 5_000_000_000));
        // loss 1.5 units -> -2 (pays 2), dust 0.5
        let (u, d) = to_usdc_toward_protocol(-15_000_000_000);
        assert_eq!((u, d), (-2, 5_000_000_000));
        let (u, d) = to_usdc_toward_protocol(-20_000_000_000);
        assert_eq!((u, d), (-2, 0));
    }
}

//! Exact decimal text to fixed point at 18 decimals, and the rounding to a market's price scale.
//!
//! Venue prices arrive as decimal text ("64870.25", "0.99904"). They are parsed exactly (no floats) into
//! `u128` units of 1e-18 USD. Anything that is not plain decimal text is refused, so a venue that changes its
//! number format makes its reading invalid (fail closed) instead of silently mis-scaled.

/// 10^18.
pub const D18: u128 = 1_000_000_000_000_000_000;

/// Largest accepted value: 10^12 whole units (one trillion USD per base unit) at 18 decimals. It keeps every
/// product in this crate (value * 10000, value * 10^18 / 10^18 by halves) far below `u128::MAX` (3.4e38).
pub const MAX_D18: u128 = 1_000_000_000_000 * D18;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DecimalError {
    Empty,
    BadChar,
    TooPrecise,
    TooLarge,
}

/// Parses `digits[.digits]` into units of 1e-18. Refused: empty text, a sign, an exponent, whitespace, a leading
/// or trailing dot, more than 18 fraction digits, a value above [`MAX_D18`].
pub fn parse_d18(s: &str) -> Result<u128, DecimalError> {
    if s.is_empty() {
        return Err(DecimalError::Empty);
    }
    let (int_part, frac_part) = match s.find('.') {
        Some(i) => (&s[..i], &s[i + 1..]),
        None => (s, ""),
    };
    if int_part.is_empty() || (s.contains('.') && frac_part.is_empty()) {
        return Err(DecimalError::BadChar);
    }
    if !int_part.bytes().all(|b| b.is_ascii_digit())
        || !frac_part.bytes().all(|b| b.is_ascii_digit())
    {
        return Err(DecimalError::BadChar);
    }
    if frac_part.len() > 18 {
        return Err(DecimalError::TooPrecise);
    }
    // Leading zeros are fine; a very long integer part is refused before it can overflow.
    let int_trim = int_part.trim_start_matches('0');
    if int_trim.len() > 13 {
        return Err(DecimalError::TooLarge);
    }
    let mut int_v: u128 = 0;
    for b in int_trim.bytes() {
        int_v = int_v * 10 + u128::from(b - b'0');
    }
    let mut frac_v: u128 = 0;
    for b in frac_part.bytes() {
        frac_v = frac_v * 10 + u128::from(b - b'0');
    }
    frac_v *= 10u128.pow(18 - frac_part.len() as u32);
    let v = int_v * D18 + frac_v;
    if v > MAX_D18 {
        return Err(DecimalError::TooLarge);
    }
    Ok(v)
}

/// Rounds a 1e-18 value to `decimals` decimals, half up (README section 3: "rounded half up from the exact
/// value"). Returns `None` when `decimals > 18`, when the result does not fit `u64`, or when it is 0 (a price is
/// never 0).
pub fn to_scale_half_up(v18: u128, decimals: u8) -> Option<u64> {
    if decimals > 18 {
        return None;
    }
    let div = 10u128.pow(18 - u32::from(decimals));
    let q = (v18 + div / 2) / div;
    if q == 0 {
        return None;
    }
    u64::try_from(q).ok()
}

/// Formats a scaled integer as decimal text with exactly `decimals` fraction digits (for reports and JSON).
pub fn format_scaled(v: u64, decimals: u8) -> String {
    if decimals == 0 {
        return v.to_string();
    }
    let p = 10u64.pow(u32::from(decimals));
    format!("{}.{:0width$}", v / p, v % p, width = decimals as usize)
}

/// `a * b / 10^18` rounded down (toward zero), for `a <= MAX_D18` and `b <= 2 * D18` (the only use: a USDT price
/// times a USDT/USD rate that the depeg bound keeps near 1). Splits `a` by 10^18 so every partial product stays
/// below 2^128: `(a / d) * b <= 1e12 * 2e18`, `(a % d) * b < 1e18 * 2e18`. Returns `None` outside the bounds.
pub fn mul_div_floor_d18(a: u128, b: u128) -> Option<u128> {
    if a > MAX_D18 || b > 2 * D18 {
        return None;
    }
    let d = D18;
    Some((a / d) * b + (a % d) * b / d)
}

/// `|a - b| * 10000 <= bps * reference`, in u128. Inputs are bounded by [`MAX_D18`] (or are u64), so the products
/// cannot overflow (1e30 * 1e4 = 1e34 < 3.4e38).
pub fn within_bps(a: u128, b: u128, bps: u32, reference: u128) -> bool {
    let diff = a.abs_diff(b);
    diff * 10_000 <= u128::from(bps) * reference
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_exactly() {
        assert_eq!(
            parse_d18("64870.25").unwrap(),
            64_870_250_000_000_000_000_000
        );
        assert_eq!(parse_d18("0.99904").unwrap(), 999_040_000_000_000_000);
        assert_eq!(parse_d18("72").unwrap(), 72 * D18);
        assert_eq!(parse_d18("000072.5").unwrap(), 72_500_000_000_000_000_000);
        assert_eq!(parse_d18("0.000000000000000001").unwrap(), 1);
        assert_eq!(parse_d18("1000000000000").unwrap(), MAX_D18);
    }

    #[test]
    fn refuses_everything_else() {
        for s in [
            "", ".5", "5.", "-1", "+1", "1e5", "1E-5", " 1", "1 ", "1,5", "0x10", "1.2.3", "NaN",
            "inf",
        ] {
            assert!(parse_d18(s).is_err(), "{s:?} must be refused");
        }
        assert_eq!(
            parse_d18("0.0000000000000000001"),
            Err(DecimalError::TooPrecise)
        );
        assert_eq!(
            parse_d18("1000000000000.000000000000000001"),
            Err(DecimalError::TooLarge)
        );
        assert_eq!(
            parse_d18("99999999999999999999"),
            Err(DecimalError::TooLarge)
        );
    }

    #[test]
    fn rounds_half_up() {
        // 64870.25 at 1 decimal: 64870.3 (half up), at 0: 64870.
        let v = parse_d18("64870.25").unwrap();
        assert_eq!(to_scale_half_up(v, 1), Some(648_703));
        assert_eq!(to_scale_half_up(v, 0), Some(64_870));
        assert_eq!(
            to_scale_half_up(parse_d18("64870.24999").unwrap(), 1),
            Some(648_702)
        );
        assert_eq!(
            to_scale_half_up(parse_d18("0.4").unwrap(), 0),
            None,
            "0 is never a price"
        );
        assert_eq!(to_scale_half_up(parse_d18("0.5").unwrap(), 0), Some(1));
        assert_eq!(to_scale_half_up(v, 19), None);
        assert_eq!(to_scale_half_up(MAX_D18, 18), None, "does not fit u64");
        assert_eq!(
            to_scale_half_up(MAX_D18, 6),
            Some(1_000_000_000_000_000_000)
        );
    }

    #[test]
    fn mul_div_matches_wide_arithmetic() {
        let a = parse_d18("64939.07").unwrap();
        let rate = parse_d18("0.99903").unwrap();
        // 64939.07 * 0.99903 = 64876.0791021 exactly.
        assert_eq!(
            mul_div_floor_d18(a, rate),
            Some(parse_d18("64876.0791021").unwrap())
        );
        assert_eq!(mul_div_floor_d18(MAX_D18, 2 * D18), Some(2 * MAX_D18));
        assert_eq!(mul_div_floor_d18(MAX_D18 + 1, D18), None);
        assert_eq!(mul_div_floor_d18(D18, 2 * D18 + 1), None);
        // Rounded down: 1e-18 * 0.5 = 0.5e-18 -> 0.
        assert_eq!(mul_div_floor_d18(1, D18 / 2), Some(0));
    }

    #[test]
    fn formats() {
        assert_eq!(format_scaled(648_703, 1), "64870.3");
        assert_eq!(format_scaled(72_660, 3), "72.660");
        assert_eq!(format_scaled(5, 2), "0.05");
        assert_eq!(format_scaled(7, 0), "7");
    }
}

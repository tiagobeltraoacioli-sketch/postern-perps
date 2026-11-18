//! The replay tape: a deterministic, authorable script of oracle rounds (README section 7b). UNAUDITED PROTOTYPE.
//!
//! One JSON object per line; blank lines are skipped. Prices are USD decimal strings keyed by market id, exact at
//! the engine's price scale (`PRICE_SCALE = 10^8`, SPEC section 1): more than 8 fraction digits is refused, never
//! rounded. Row forms (unknown fields refused):
//!
//! - `{"p":{"1":"67250.5","2":"3480.12"}}`: one round. A market not named HOLDS its last price; `"absent":[3]`
//!   removes market 3 from the observations until a later row prices it again (the signers abstain).
//! - `"repeat":N` on a `p` row: N rounds with these prices (default 1).
//! - `{"ramp":{"1":"59180"},"rounds":N}`: N rounds moving market 1 in a straight line from its current price to
//!   the target: round k of N has `from + (to - from) * k / N`, the division truncated toward `from`, so round N
//!   is exactly the target. Other markets hold.
//! - `{"silent":N}`: N rounds in which nothing is submitted (the node's markets go STALE: rule H1).
//! - on `p` and `ramp` rows: `"src":{"1":5}` (the `sources` count signed for a market, default 5),
//!   `"signers":[0,1,2,3,4]` (only these committee indices sign; default all), `"override":{"6":{"1":"70000"}}`
//!   (signer 6 signs another price for market 1: a lying or diverging signer), `"note":"free text"`.
//!
//! [`parse`] expands a tape to one [`Step`] per round. The same tape text gives the same steps on every machine.

use std::collections::BTreeMap;

use perps_oracle::decimal::{format_scaled, parse_d18};
use serde_json::{Map, Value};

/// Fraction digits of the engine's price scale (SPEC section 1, `PRICE_SCALE = 10^8`).
pub const PRICE_DECIMALS: u8 = 8;
pub const DEFAULT_SOURCES: u8 = 5;
pub const MAX_STEPS: usize = 1_000_000;

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Step {
    /// Nothing is submitted this round.
    pub silent: bool,
    /// market id -> (price at 1e-8 USD, sources).
    pub prices: BTreeMap<u32, (u64, u8)>,
    /// Committee indices that sign; `None` = all.
    pub signers: Option<Vec<u8>>,
    /// signer -> market id -> price that signer signs instead.
    pub overrides: BTreeMap<u8, BTreeMap<u32, u64>>,
}

/// An exact price at the engine scale: positive, at most 8 fraction digits, at most `MAX_PRICE = 10^15` units.
pub fn parse_price(s: &str) -> Result<u64, String> {
    let v = parse_d18(s).map_err(|e| format!("price {s:?}: {e:?}"))?;
    let unit = 10u128.pow(18 - u32::from(PRICE_DECIMALS));
    if v % unit != 0 {
        return Err(format!(
            "price {s:?}: more than {PRICE_DECIMALS} fraction digits"
        ));
    }
    let p = v / unit;
    if p == 0 || p > 1_000_000_000_000_000 {
        return Err(format!("price {s:?}: outside 1e-8 .. 10,000,000 USD"));
    }
    Ok(p as u64)
}

/// The tape text of a price: decimal, trailing zeros removed.
pub fn format_price(p: u64) -> String {
    let s = format_scaled(p, PRICE_DECIMALS);
    let s = s.trim_end_matches('0');
    s.strip_suffix('.').unwrap_or(s).to_string()
}

fn market_id(k: &str) -> Result<u32, String> {
    if k.is_empty() || k.len() > 10 || !k.bytes().all(|b| b.is_ascii_digit()) || k.starts_with('0')
    {
        return Err(format!("market id {k:?}"));
    }
    k.parse().map_err(|_| format!("market id {k:?}"))
}

fn price_map(v: &Value, what: &str) -> Result<BTreeMap<u32, u64>, String> {
    let o = v.as_object().ok_or(format!("{what}: not an object"))?;
    let mut m = BTreeMap::new();
    for (k, p) in o {
        let p = p
            .as_str()
            .ok_or(format!("{what}: price of {k} is not a decimal string"))?;
        m.insert(market_id(k)?, parse_price(p)?);
    }
    Ok(m)
}

fn count(o: &Map<String, Value>, k: &str, default: Option<u64>) -> Result<u64, String> {
    match o.get(k) {
        None => default.ok_or(format!("missing {k}")),
        Some(v) => v
            .as_u64()
            .filter(|n| *n >= 1)
            .ok_or(format!("{k}: a count of at least 1")),
    }
}

struct Common {
    src: BTreeMap<u32, u8>,
    signers: Option<Vec<u8>>,
    overrides: BTreeMap<u8, BTreeMap<u32, u64>>,
}

fn common(o: &Map<String, Value>) -> Result<Common, String> {
    let mut src = BTreeMap::new();
    if let Some(v) = o.get("src") {
        for (k, n) in v.as_object().ok_or("src: not an object")? {
            let n = n
                .as_u64()
                .and_then(|n| u8::try_from(n).ok())
                .ok_or("src: a count 0..255")?;
            src.insert(market_id(k)?, n);
        }
    }
    let signers = match o.get("signers") {
        None => None,
        Some(v) => {
            let a = v.as_array().ok_or("signers: not an array")?;
            let mut s = a
                .iter()
                .map(|x| {
                    x.as_u64()
                        .and_then(|x| u8::try_from(x).ok())
                        .ok_or("signers: indices 0..255")
                })
                .collect::<Result<Vec<u8>, _>>()?;
            s.sort_unstable();
            if s.windows(2).any(|w| w[0] == w[1]) {
                return Err("signers: an index twice".into());
            }
            Some(s)
        }
    };
    let mut overrides = BTreeMap::new();
    if let Some(v) = o.get("override") {
        for (k, m) in v.as_object().ok_or("override: not an object")? {
            let signer: u8 = k.parse().map_err(|_| format!("override: signer {k:?}"))?;
            overrides.insert(signer, price_map(m, "override")?);
        }
    }
    Ok(Common {
        src,
        signers,
        overrides,
    })
}

fn only(o: &Map<String, Value>, allowed: &[&str]) -> Result<(), String> {
    match o.keys().find(|k| !allowed.contains(&k.as_str())) {
        Some(k) => Err(format!("unknown field {k:?}")),
        None => Ok(()),
    }
}

/// Expands a tape to one step per round.
pub fn parse(text: &str) -> Result<Vec<Step>, String> {
    let mut steps: Vec<Step> = Vec::new();
    // market id -> (price, sources) currently in force.
    let mut cur: BTreeMap<u32, (u64, u8)> = BTreeMap::new();
    for (ln, line) in text.lines().enumerate() {
        if line.trim().is_empty() {
            continue;
        }
        let at = |e: String| format!("tape line {}: {e}", ln + 1);
        let v: Value = serde_json::from_str(line).map_err(|e| at(e.to_string()))?;
        let o = v.as_object().ok_or_else(|| at("not an object".into()))?;
        let push = |s: Step, steps: &mut Vec<Step>| -> Result<(), String> {
            if steps.len() >= MAX_STEPS {
                return Err(format!("more than {MAX_STEPS} rounds"));
            }
            steps.push(s);
            Ok(())
        };
        if o.contains_key("silent") {
            only(o, &["silent", "note"]).map_err(at)?;
            for _ in 0..count(o, "silent", None).map_err(at)? {
                push(
                    Step {
                        silent: true,
                        ..Step::default()
                    },
                    &mut steps,
                )
                .map_err(at)?;
            }
            continue;
        }
        let c = common(o).map_err(at)?;
        let apply_src = |cur: &mut BTreeMap<u32, (u64, u8)>| -> Result<(), String> {
            for (m, n) in &c.src {
                cur.get_mut(m)
                    .ok_or(format!("src: market {m} has no price"))?
                    .1 = *n;
            }
            Ok(())
        };
        let step = |cur: &BTreeMap<u32, (u64, u8)>| Step {
            silent: false,
            prices: cur.clone(),
            signers: c.signers.clone(),
            overrides: c.overrides.clone(),
        };
        if o.contains_key("ramp") {
            only(o, &["ramp", "rounds", "src", "signers", "override", "note"]).map_err(at)?;
            let to = price_map(&o["ramp"], "ramp").map_err(at)?;
            let n = count(o, "rounds", None).map_err(at)?;
            let mut from = BTreeMap::new();
            for m in to.keys() {
                from.insert(
                    *m,
                    cur.get(m)
                        .ok_or_else(|| at(format!("ramp: market {m} has no price to start from")))?
                        .0,
                );
            }
            apply_src(&mut cur).map_err(at)?;
            for k in 1..=n {
                for (m, t) in &to {
                    let f = i128::from(from[m]);
                    let p = f + (i128::from(*t) - f) * i128::from(k) / i128::from(n);
                    cur.get_mut(m).unwrap().0 = p as u64;
                }
                push(step(&cur), &mut steps).map_err(at)?;
            }
        } else if o.contains_key("p") {
            only(
                o,
                &[
                    "p", "absent", "repeat", "src", "signers", "override", "note",
                ],
            )
            .map_err(at)?;
            for (m, p) in price_map(&o["p"], "p").map_err(at)? {
                let s = cur.get(&m).map_or(DEFAULT_SOURCES, |x| x.1);
                cur.insert(m, (p, s));
            }
            if let Some(a) = o.get("absent") {
                for m in a
                    .as_array()
                    .ok_or_else(|| at("absent: not an array".into()))?
                {
                    let m = m
                        .as_u64()
                        .and_then(|m| u32::try_from(m).ok())
                        .ok_or_else(|| at("absent: market ids".into()))?;
                    cur.remove(&m);
                }
            }
            apply_src(&mut cur).map_err(at)?;
            for _ in 0..count(o, "repeat", Some(1)).map_err(at)? {
                push(step(&cur), &mut steps).map_err(at)?;
            }
        } else {
            return Err(at("a row needs \"p\", \"ramp\" or \"silent\"".into()));
        }
    }
    if steps.is_empty() {
        return Err("tape: no rounds".into());
    }
    Ok(steps)
}

/// A `p` row for one round (the form `perps-oracle tape` writes from recorded venue rounds).
pub fn row(prices: &BTreeMap<u32, (u64, u8)>, absent: &[u32], note: &str) -> String {
    let p: Map<String, Value> = prices
        .iter()
        .map(|(m, (p, _))| (m.to_string(), Value::String(format_price(*p))))
        .collect();
    let src: Map<String, Value> = prices
        .iter()
        .map(|(m, (_, s))| (m.to_string(), Value::from(*s)))
        .collect();
    let mut o = Map::new();
    o.insert("p".into(), Value::Object(p));
    o.insert("src".into(), Value::Object(src));
    if !absent.is_empty() {
        o.insert("absent".into(), Value::from(absent.to_vec()));
    }
    o.insert("note".into(), Value::String(note.into()));
    Value::Object(o).to_string()
}

/// The order in which steps are played: every `speed`-th step; with `looped`, forward then backward for ever
/// (ping-pong, so the price path has no jump where the tape wraps). `None` = the tape has ended.
pub fn index_at(len: usize, speed: usize, looped: bool, k: u64) -> Option<usize> {
    let speed = speed.max(1);
    let n = len.div_ceil(speed) as u64; // steps played per pass
    if !looped {
        return (k < n).then(|| k as usize * speed);
    }
    if n == 1 {
        return Some(0);
    }
    let period = 2 * (n - 1);
    let j = k % period;
    let i = if j < n { j } else { period - j };
    Some(i as usize * speed)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn prices_are_exact_or_refused() {
        assert_eq!(parse_price("67250.5"), Ok(6_725_050_000_000));
        assert_eq!(parse_price("0.00000001"), Ok(1));
        assert_eq!(parse_price("10000000"), Ok(1_000_000_000_000_000));
        for bad in [
            "0",
            "0.000000001",
            "67250.123456789",
            "10000000.00000001",
            "-1",
            "1e5",
            "",
            "1,5",
        ] {
            assert!(parse_price(bad).is_err(), "{bad}");
        }
        for p in [1u64, 6_725_050_000_000, 100_000_000, 1_000_000_000_000_000] {
            assert_eq!(parse_price(&format_price(p)), Ok(p));
        }
        assert_eq!(format_price(6_725_050_000_000), "67250.5");
        assert_eq!(format_price(100_000_000), "1");
    }

    #[test]
    fn rows_expand_deterministically() {
        let t = r#"{"p":{"1":"100","2":"50"},"repeat":2}

{"ramp":{"1":"88"},"rounds":5,"signers":[4,0,2,1,3]}
{"silent":2}
{"p":{"2":"51"},"absent":[1],"src":{"2":7},"override":{"6":{"2":"60"}}}
{"p":{"1":"90"}}"#;
        let s = parse(t).unwrap();
        assert_eq!(s.len(), 2 + 5 + 2 + 1 + 1);
        let u = 100_000_000u64;
        assert_eq!(
            s[0].prices,
            BTreeMap::from([(1, (100 * u, 5)), (2, (50 * u, 5))])
        );
        assert_eq!(s[1], s[0]);
        // 12% down over 5 rounds: 100 -> 97.6, 95.2, 92.8, 90.4, 88 exactly; market 2 holds.
        let ramp: Vec<u64> = s[2..7].iter().map(|x| x.prices[&1].0).collect();
        assert_eq!(
            ramp,
            [
                9_760_000_000,
                9_520_000_000,
                9_280_000_000,
                9_040_000_000,
                8_800_000_000
            ]
        );
        assert!(s[2..7].iter().all(|x| x.prices[&2].0 == 50 * u));
        assert_eq!(s[2].signers, Some(vec![0, 1, 2, 3, 4]));
        assert!(s[7].silent && s[8].silent && s[7].prices.is_empty());
        assert_eq!(s[9].prices, BTreeMap::from([(2, (51 * u, 7))]));
        assert_eq!(s[9].overrides[&6][&2], 60 * u);
        // Market 1 comes back with the default sources; market 2 keeps its 7.
        assert_eq!(
            s[10].prices,
            BTreeMap::from([(1, (90 * u, 5)), (2, (51 * u, 7))])
        );
        assert_eq!(parse(t).unwrap(), s);
    }

    #[test]
    fn ramp_truncates_toward_the_start_and_ends_on_the_target() {
        let s =
            parse("{\"p\":{\"1\":\"0.00000010\"}}\n{\"ramp\":{\"1\":\"0.00000003\"},\"rounds\":3}")
                .unwrap();
        // 10 -> 3 in 3 rounds: 10 - 7/3 = 8 (7.67 truncated toward 10), 10 - 14/3 = 6, 3.
        assert_eq!(
            s.iter().map(|x| x.prices[&1].0).collect::<Vec<_>>(),
            [10, 8, 6, 3]
        );
        let s =
            parse("{\"p\":{\"1\":\"0.00000003\"}}\n{\"ramp\":{\"1\":\"0.0000001\"},\"rounds\":3}")
                .unwrap();
        assert_eq!(
            s.iter().map(|x| x.prices[&1].0).collect::<Vec<_>>(),
            [3, 5, 7, 10]
        );
    }

    #[test]
    fn malformed_tapes_are_refused() {
        for bad in [
            "",
            "[]",
            "{}",
            "{\"p\":{\"1\":67250.5}}",
            "{\"p\":{\"01\":\"1\"}}",
            "{\"p\":{\"1\":\"1\"},\"repeat\":0}",
            "{\"p\":{\"1\":\"1\"},\"extra\":1}",
            "{\"ramp\":{\"1\":\"2\"},\"rounds\":3}",
            "{\"p\":{\"1\":\"1\"}}\n{\"ramp\":{\"1\":\"2\"}}",
            "{\"p\":{\"1\":\"1\"},\"src\":{\"2\":5}}",
            "{\"p\":{\"1\":\"1\"},\"signers\":[1,1]}",
            "{\"silent\":0}",
            "{\"silent\":1,\"p\":{\"1\":\"1\"}}",
            "{\"p\":{\"1\":\"1\"},\"ramp\":{\"1\":\"2\"},\"rounds\":1,\"repeat\":2}",
        ] {
            assert!(parse(bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn play_order() {
        let seq = |len, speed, looped, n: u64| -> Vec<Option<usize>> {
            (0..n).map(|k| index_at(len, speed, looped, k)).collect()
        };
        assert_eq!(seq(3, 1, false, 4), [Some(0), Some(1), Some(2), None]);
        assert_eq!(seq(7, 3, false, 4), [Some(0), Some(3), Some(6), None]);
        // Ping-pong: neighbours in play order are neighbours on the tape (no jump at the wrap).
        assert_eq!(
            seq(4, 1, true, 8),
            [
                Some(0),
                Some(1),
                Some(2),
                Some(3),
                Some(2),
                Some(1),
                Some(0),
                Some(1)
            ]
        );
        assert_eq!(seq(1, 1, true, 3), [Some(0), Some(0), Some(0)]);
        assert_eq!(
            seq(5, 2, true, 5),
            [Some(0), Some(2), Some(4), Some(2), Some(0)]
        );
    }
}

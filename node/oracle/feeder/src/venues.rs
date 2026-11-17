//! Venue adapters: pure parsers from a recorded or live HTTP response body to a [`Reading`].
//!
//! Each adapter refuses anything it does not recognise exactly (missing field, a number in an unexpected
//! format, an error flag, a response for another pair when the venue names the pair): a venue that changes its API
//! becomes an invalid reading (fail closed), never a mis-parsed price. Endpoints, terms of use and the reason for
//! each venue are in `SOURCES.md`.

use perps_oracle::decimal::parse_d18;
use perps_oracle::feeder_rule::{Quote, Reading, VenueTime};
use serde_json::Value;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Adapter {
    Coinbase,
    Kraken,
    Bitstamp,
    Gemini,
    Bitfinex,
    Okx,
    Binance,
}

/// One configured source: a venue's public ticker for one pair.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Source {
    /// Stable name, `<venue>-<BASE>-<QUOTE>`, used in fixtures and in the node's `OracleStatus`.
    pub name: String,
    /// The operator, for the one-reading-per-operator rule (F1).
    pub operator: &'static str,
    pub adapter: Adapter,
    pub base: &'static str,
    pub quote: Quote,
    /// The venue's own pair code.
    pub pair: String,
    pub url: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ParseError {
    Json,
    VenueError(String),
    Missing(&'static str),
    Number(&'static str),
    WrongPair(String),
    Time,
}

impl std::fmt::Display for ParseError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ParseError::Json => write!(f, "not json"),
            ParseError::VenueError(e) => write!(f, "venue error: {e}"),
            ParseError::Missing(k) => write!(f, "missing field {k}"),
            ParseError::Number(k) => write!(f, "bad number in {k}"),
            ParseError::WrongPair(p) => write!(f, "response for another pair: {p}"),
            ParseError::Time => write!(f, "bad time"),
        }
    }
}

fn base_code(adapter: Adapter, base: &str) -> String {
    match (adapter, base) {
        (Adapter::Kraken, "BTC") => "XBT".into(),
        (Adapter::Bitfinex, "USDT") => "UST".into(),
        _ => base.into(),
    }
}

impl Source {
    pub fn new(adapter: Adapter, base: &'static str, quote: Quote) -> Self {
        let q = match quote {
            Quote::Usd => "USD",
            Quote::Usdt => "USDT",
        };
        let b = base_code(adapter, base);
        let (operator, pair, url) = match adapter {
            Adapter::Coinbase => {
                let p = format!("{b}-{q}");
                (
                    "coinbase",
                    p.clone(),
                    format!("https://api.exchange.coinbase.com/products/{p}/ticker"),
                )
            }
            Adapter::Kraken => {
                let p = format!("{b}{q}");
                (
                    "kraken",
                    p.clone(),
                    format!("https://api.kraken.com/0/public/Ticker?pair={p}"),
                )
            }
            Adapter::Bitstamp => {
                let p = format!("{b}{q}").to_lowercase();
                (
                    "bitstamp",
                    p.clone(),
                    format!("https://www.bitstamp.net/api/v2/ticker/{p}/"),
                )
            }
            Adapter::Gemini => {
                let p = format!("{b}{q}").to_lowercase();
                (
                    "gemini",
                    p.clone(),
                    format!("https://api.gemini.com/v1/pubticker/{p}"),
                )
            }
            Adapter::Bitfinex => {
                let p = format!("t{b}{q}");
                (
                    "bitfinex",
                    p.clone(),
                    format!("https://api-pub.bitfinex.com/v2/ticker/{p}"),
                )
            }
            Adapter::Okx => {
                let p = format!("{b}-{q}");
                (
                    "okx",
                    p.clone(),
                    format!("https://www.okx.com/api/v5/market/ticker?instId={p}"),
                )
            }
            Adapter::Binance => {
                let p = format!("{b}{q}");
                (
                    "binance",
                    p.clone(),
                    format!("https://api.binance.com/api/v3/ticker/bookTicker?symbol={p}"),
                )
            }
        };
        Source {
            name: format!("{operator}-{base}-{q}"),
            operator,
            adapter,
            base,
            quote,
            pair,
            url,
        }
    }

    /// Parses a response body received at `received_ms`.
    pub fn parse(&self, body: &str, received_ms: u64) -> Result<Reading, ParseError> {
        let v: Value = serde_json::from_str(body).map_err(|_| ParseError::Json)?;
        let (bid, ask, last, venue_time) = match self.adapter {
            Adapter::Coinbase => {
                if let Some(m) = v.get("message") {
                    return Err(ParseError::VenueError(m.to_string()));
                }
                let t = str_field(&v, "time")?;
                (
                    num(&v, "bid")?,
                    num(&v, "ask")?,
                    Some(num(&v, "price")?),
                    Some(VenueTime::LastTrade(parse_rfc3339_ms(t)?)),
                )
            }
            Adapter::Kraken => {
                let err = v
                    .get("error")
                    .and_then(Value::as_array)
                    .ok_or(ParseError::Missing("error"))?;
                if !err.is_empty() {
                    return Err(ParseError::VenueError(
                        Value::Array(err.clone()).to_string(),
                    ));
                }
                let res = v
                    .get("result")
                    .and_then(Value::as_object)
                    .ok_or(ParseError::Missing("result"))?;
                if res.len() != 1 {
                    return Err(ParseError::WrongPair(format!("{} result keys", res.len())));
                }
                let (k, t) = res.iter().next().unwrap();
                if !kraken_key_matches(&self.pair, k) {
                    return Err(ParseError::WrongPair(k.clone()));
                }
                (arr0(t, "b")?, arr0(t, "a")?, Some(arr0(t, "c")?), None)
            }
            Adapter::Bitstamp => {
                let ts = str_field(&v, "timestamp")?;
                let secs: u64 = digits(ts).ok_or(ParseError::Time)?;
                (
                    num(&v, "bid")?,
                    num(&v, "ask")?,
                    Some(num(&v, "last")?),
                    Some(VenueTime::Snapshot(
                        secs.checked_mul(1000).ok_or(ParseError::Time)?,
                    )),
                )
            }
            Adapter::Gemini => {
                if v.get("result").and_then(Value::as_str) == Some("error") {
                    return Err(ParseError::VenueError(v.to_string()));
                }
                // `volume.timestamp` is the minute of the volume figures, not the quote time: not used.
                (
                    num(&v, "bid")?,
                    num(&v, "ask")?,
                    Some(num(&v, "last")?),
                    None,
                )
            }
            Adapter::Bitfinex => {
                // [BID, BID_SIZE, ASK, ASK_SIZE, DAILY_CHANGE, DAILY_CHANGE_RELATIVE, LAST_PRICE, VOLUME, HIGH, LOW, ...]
                let a = v.as_array().ok_or(ParseError::Missing("array"))?;
                if a.first().and_then(Value::as_str) == Some("error") {
                    return Err(ParseError::VenueError(v.to_string()));
                }
                if a.len() < 10 {
                    return Err(ParseError::Missing("ticker fields"));
                }
                (
                    json_num(&a[0], "BID")?,
                    json_num(&a[2], "ASK")?,
                    Some(json_num(&a[6], "LAST_PRICE")?),
                    None,
                )
            }
            Adapter::Okx => {
                if v.get("code").and_then(Value::as_str) != Some("0") {
                    return Err(ParseError::VenueError(
                        v.get("msg").map(|m| m.to_string()).unwrap_or_default(),
                    ));
                }
                let d = v
                    .get("data")
                    .and_then(Value::as_array)
                    .ok_or(ParseError::Missing("data"))?;
                if d.len() != 1 {
                    return Err(ParseError::Missing("data[0]"));
                }
                let t = &d[0];
                let inst = str_field(t, "instId")?;
                if inst != self.pair {
                    return Err(ParseError::WrongPair(inst.into()));
                }
                let ts: u64 = digits(str_field(t, "ts")?).ok_or(ParseError::Time)?;
                (
                    num(t, "bidPx")?,
                    num(t, "askPx")?,
                    Some(num(t, "last")?),
                    Some(VenueTime::Snapshot(ts)),
                )
            }
            Adapter::Binance => {
                if v.get("code").is_some() {
                    return Err(ParseError::VenueError(v.to_string()));
                }
                let sym = str_field(&v, "symbol")?;
                if sym != self.pair {
                    return Err(ParseError::WrongPair(sym.into()));
                }
                (num(&v, "bidPrice")?, num(&v, "askPrice")?, None, None)
            }
        };
        Ok(Reading {
            bid,
            ask,
            last,
            venue_time,
            received_ms,
            quote: self.quote,
        })
    }
}

fn kraken_key_matches(pair: &str, key: &str) -> bool {
    // Kraken answers with its canonical pair name: XBTUSD -> XXBTZUSD, ETHUSD -> XETHZUSD, USDTUSD -> USDTZUSD,
    // SOLUSD -> SOLUSD. Accept exactly the request or the known canonical form.
    let canonical = match pair {
        "XBTUSD" => "XXBTZUSD",
        "ETHUSD" => "XETHZUSD",
        "USDTUSD" => "USDTZUSD",
        other => other,
    };
    key == pair || key == canonical
}

fn digits(s: &str) -> Option<u64> {
    if s.is_empty() || s.len() > 19 || !s.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    s.parse().ok()
}

fn str_field<'a>(v: &'a Value, k: &'static str) -> Result<&'a str, ParseError> {
    v.get(k)
        .and_then(Value::as_str)
        .ok_or(ParseError::Missing(k))
}

fn num(v: &Value, k: &'static str) -> Result<u128, ParseError> {
    parse_d18(str_field(v, k)?).map_err(|_| ParseError::Number(k))
}

fn arr0(v: &Value, k: &'static str) -> Result<u128, ParseError> {
    let s = v
        .get(k)
        .and_then(Value::as_array)
        .and_then(|a| a.first())
        .and_then(Value::as_str)
        .ok_or(ParseError::Missing(k))?;
    parse_d18(s).map_err(|_| ParseError::Number(k))
}

/// A JSON number (exact text kept by `arbitrary_precision`); refused if negative, in exponent form or not a number.
fn json_num(v: &Value, k: &'static str) -> Result<u128, ParseError> {
    match v {
        Value::Number(n) => parse_d18(&n.to_string()).map_err(|_| ParseError::Number(k)),
        _ => Err(ParseError::Number(k)),
    }
}

/// `YYYY-MM-DDTHH:MM:SS[.fraction]Z` to Unix milliseconds (fraction truncated to ms). Only UTC `Z` is accepted.
pub fn parse_rfc3339_ms(s: &str) -> Result<u64, ParseError> {
    let b = s.as_bytes();
    if b.len() < 20
        || b[4] != b'-'
        || b[7] != b'-'
        || b[10] != b'T'
        || b[13] != b':'
        || b[16] != b':'
        || *b.last().unwrap() != b'Z'
    {
        return Err(ParseError::Time);
    }
    let n = |r: std::ops::Range<usize>| -> Result<u64, ParseError> {
        digits(&s[r]).ok_or(ParseError::Time)
    };
    let (y, mo, d, h, mi, se) = (
        n(0..4)?,
        n(5..7)?,
        n(8..10)?,
        n(11..13)?,
        n(14..16)?,
        n(17..19)?,
    );
    if !(1..=12).contains(&mo) || d == 0 || d > 31 || h > 23 || mi > 59 || se > 60 || y < 1970 {
        return Err(ParseError::Time);
    }
    let mut ms = 0u64;
    let rest = &s[19..s.len() - 1];
    if !rest.is_empty() {
        let frac = rest.strip_prefix('.').ok_or(ParseError::Time)?;
        if frac.is_empty() || frac.len() > 12 || !frac.bytes().all(|c| c.is_ascii_digit()) {
            return Err(ParseError::Time);
        }
        let f3 = format!("{:0<3}", &frac[..frac.len().min(3)]);
        ms = digits(&f3).ok_or(ParseError::Time)?;
    }
    // Days from civil (Howard Hinnant's algorithm), proleptic Gregorian.
    let (y, mo, d) = (y as i64, mo as i64, d as i64);
    let y2 = if mo <= 2 { y - 1 } else { y };
    let era = y2.div_euclid(400);
    let yoe = y2 - era * 400;
    let mp = (mo + 9) % 12;
    let doy = (153 * mp + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    let days = era * 146_097 + doe - 719_468;
    Ok((days as u64) * 86_400_000 + h * 3_600_000 + mi * 60_000 + se * 1000 + ms)
}

/// The default source set (SOURCES.md): five USD venues and two USDT venues per launch market; for the USDT/USD
/// conversion rate four USD venues, without Bitfinex (its operator shares ownership with the issuer of USDT, so
/// it is not an independent judge of USDT's own peg).
pub fn default_sources(base: &'static str) -> Vec<Source> {
    use Adapter::*;
    if base == "USDT" {
        return [Coinbase, Kraken, Bitstamp, Gemini]
            .iter()
            .map(|&a| Source::new(a, base, Quote::Usd))
            .collect();
    }
    let mut v: Vec<Source> = [Coinbase, Kraken, Bitstamp, Gemini, Bitfinex]
        .iter()
        .map(|&a| Source::new(a, base, Quote::Usd))
        .collect();
    v.extend(
        [Okx, Binance]
            .iter()
            .map(|&a| Source::new(a, base, Quote::Usdt)),
    );
    v
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rfc3339() {
        assert_eq!(parse_rfc3339_ms("1970-01-01T00:00:00Z"), Ok(0));
        assert_eq!(
            parse_rfc3339_ms("2026-11-17T04:06:08.311951968Z"),
            Ok(1_794_888_368_311)
        );
        assert_eq!(
            parse_rfc3339_ms("2024-02-29T12:00:00.5Z"),
            Ok(1_709_208_000_500)
        );
        for bad in [
            "2026-11-17T04:06:08+00:00",
            "2026-11-17 04:06:08Z",
            "2026-13-17T04:06:08Z",
            "2026-11-17T04:06:08.Z",
            "x",
        ] {
            assert!(parse_rfc3339_ms(bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn source_names_and_urls() {
        let s = Source::new(Adapter::Kraken, "BTC", Quote::Usd);
        assert_eq!(s.name, "kraken-BTC-USD");
        assert_eq!(s.url, "https://api.kraken.com/0/public/Ticker?pair=XBTUSD");
        let s = Source::new(Adapter::Bitfinex, "USDT", Quote::Usd);
        assert_eq!(s.pair, "tUSTUSD");
        assert_eq!(default_sources("SOL").len(), 7);
        assert_eq!(default_sources("USDT").len(), 4);
        assert!(default_sources("USDT")
            .iter()
            .all(|s| s.operator != "bitfinex"));
    }
}

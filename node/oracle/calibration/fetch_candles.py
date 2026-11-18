#!/usr/bin/env python3
"""Minute candles of the launch markets from public, anonymous, read-only venue endpoints (UNAUDITED PROTOTYPE).

Usage: python3 fetch_candles.py OUTDIR
Writes OUTDIR/<venue>-<PAIR>-<YYYY-MM-DD>.json = [[minuteStartMs, "open", "high", "low", "close"], ...] ascending,
prices as the venue's own decimal text. One file per venue, pair and UTC day; a file that exists is not fetched again.
Generic client name; at most about three requests per second in total; nothing identifying an operator.
Days: the worst days named in docs/MARKETS.md section 4 (2025-10-10, 2026-01-31, 2026-02-05, 2026-11-15), the day
after the largest one (2025-10-11), and the 30 days 2026-10-19 .. 2026-11-17.
"""
import datetime as dt, json, os, sys, time, urllib.request

OUT = sys.argv[1]
UA = "ticker-client/1.0"
WORST = ["2025-10-10", "2025-10-11", "2026-01-31", "2026-02-05", "2026-11-15"]
D0 = dt.date(2026, 10, 19)
DAYS = sorted(set(WORST + [(D0 + dt.timedelta(days=i)).isoformat() for i in range(30)]))
BASES = ["BTC", "ETH", "SOL"]


def get(url):
    for attempt in range(4):
        try:
            req = urllib.request.Request(url, headers={"User-Agent": UA, "Accept": "application/json"})
            with urllib.request.urlopen(req, timeout=20) as r:
                return json.loads(r.read().decode())
        except Exception as e:  # noqa: BLE001 (recorded, retried with a pause, then given up)
            err = repr(e)
            time.sleep(2 + 3 * attempt)
    print("GIVE UP", url, err, flush=True)
    return None


def day_ms(day):
    d = dt.datetime.fromisoformat(day).replace(tzinfo=dt.timezone.utc)
    return int(d.timestamp() * 1000)


def binance(base, day):
    s, out = day_ms(day), []
    for k in range(2):
        a = s + k * 720 * 60000
        v = get(f"https://api.binance.com/api/v3/klines?symbol={base}USDT&interval=1m&startTime={a}&endTime={a + 720 * 60000 - 1}&limit=1000")
        if v is None:
            return None
        out += [[r[0], r[1], r[2], r[3], r[4]] for r in v]
        time.sleep(0.35)
    return out


def coinbase(base, day, quote="USD"):
    s, out = day_ms(day) // 1000, []
    for k in range(5):
        a = s + k * 288 * 60
        iso = lambda t: dt.datetime.fromtimestamp(t, dt.timezone.utc).strftime("%Y-%m-%dT%H:%M:%SZ")
        v = get(f"https://api.exchange.coinbase.com/products/{base}-{quote}/candles?granularity=60&start={iso(a)}&end={iso(a + 288 * 60 - 60)}")
        if v is None:
            return None
        # [time s, low, high, open, close, volume], numbers: keep their JSON text through repr of the parsed value
        out += [[r[0] * 1000, repr(r[3]), repr(r[2]), repr(r[1]), repr(r[4])] for r in v]
        time.sleep(0.35)
    return sorted(out)


def bitstamp(base, day):
    s, out = day_ms(day) // 1000, []
    for k in range(2):
        a = s + k * 720 * 60
        v = get(f"https://www.bitstamp.net/api/v2/ohlc/{base.lower()}usd/?step=60&limit=720&start={a}&end={a + 720 * 60 - 60}")
        if v is None:
            return None
        out += [[int(r["timestamp"]) * 1000, r["open"], r["high"], r["low"], r["close"]] for r in v["data"]["ohlc"]]
        time.sleep(0.35)
    return sorted(out)


def bitfinex(base, day):
    s = day_ms(day)
    v = get(f"https://api-pub.bitfinex.com/v2/candles/trade:1m:t{base}USD/hist?start={s}&end={s + 86400000 - 1}&limit=10000&sort=1")
    time.sleep(2.2)
    if v is None:
        return None
    # [mts, open, close, high, low, volume]
    return [[r[0], repr(r[1]), repr(r[3]), repr(r[4]), repr(r[2])] for r in v]


VENUES = {"binance": binance, "coinbase": coinbase, "bitstamp": bitstamp, "bitfinex": bitfinex}

os.makedirs(OUT, exist_ok=True)
jobs = [(v, b, d) for d in DAYS for b in BASES for v in VENUES] + [("coinbase", "USDT", d) for d in DAYS]
for venue, base, day in jobs:
    quote = "USDT" if venue == "binance" else "USD"
    path = os.path.join(OUT, f"{venue}-{base}-{quote}-{day}.json")
    if os.path.exists(path):
        continue
    rows = VENUES[venue](base, day)
    if rows is None:
        continue
    lo, hi = day_ms(day), day_ms(day) + 86400000
    rows = [r for r in rows if lo <= r[0] < hi]
    with open(path + ".tmp", "w") as f:
        json.dump(rows, f, separators=(",", ":"))
    os.rename(path + ".tmp", path)
    print(venue, base, day, len(rows), flush=True)
print("DONE", flush=True)

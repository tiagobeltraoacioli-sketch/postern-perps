#!/usr/bin/env python3
"""Calibration statistics of the oracle parameters on public minute candles (UNAUDITED PROTOTYPE).

Usage: python3 analyse_candles.py CANDLEDIR > results.json
Input: the files written by fetch_candles.py. Statistics only (floating point is fine here: nothing in this file is
consensus arithmetic). Method, per market:

- Reference series per minute: the MEDIAN over venues of the minute's close, high and low, taken over the venues
  that traded in that minute (at least 2). Binance is quoted in USDT and converted with Coinbase's USDT-USD close of
  the same minute (last known value when the minute has no trade).
- jump: |close(t) / close(t-1) - 1| and (high - low) / low of the reference, in bps. A 3 s oracle round cannot move
  more than its minute's range, so "minutes whose range exceeds jumpBps" bounds the number of JUMP halts from above.
- dispersion: per minute with at least 3 venues, the largest |close_v - median| / median (bps) and the number of
  venues within 200 bps of the median (the feeder's F4/F5). Minute closes are last trades at different instants
  inside the minute, so this OVERSTATES the dispersion of simultaneous quotes.
- excursion(m): the largest move of the reference away from close(t) during the next m minutes (highs and lows
  included), in bps: what an index that went dark at t can be wrong by after m minutes. Compared with the exit band
  half-width h(m) = min(exitMaxHaircutBps, exitHaircutBps + exitHaircutPerMinBps * m).
- settle error: |close(t + 60) / close(t) - 1|: the error of settling at the pre-halt index after maxHaltMs = 1 h
  without any committee price.
"""
import glob, json, os, statistics, sys
from collections import defaultdict

D = sys.argv[1]
WORST = ["2025-10-10", "2025-10-11", "2026-01-31", "2026-02-05", "2026-11-15"]
JUMP = {"BTC": 500, "ETH": 500, "SOL": 800}
HORIZONS = [1, 2, 5, 10, 15, 30, 60]


def load(venue, base, quote):
    out = {}
    for p in sorted(glob.glob(os.path.join(D, f"{venue}-{base}-{quote}-*.json"))):
        for t, o, h, l, c in json.load(open(p)):
            out[t] = (float(o), float(h), float(l), float(c))
    return out


def q(v, p):
    if not v:
        return None
    s = sorted(v)
    return s[min(len(s) - 1, int(p * len(s)))]


def stats(v):
    return {"n": len(v), "p50": q(v, 0.5), "p99": q(v, 0.99), "p999": q(v, 0.999), "p9999": q(v, 0.9999), "max": max(v) if v else None}


def rnd(x):
    if isinstance(x, float):
        return round(x, 2)
    if isinstance(x, dict):
        return {k: rnd(v) for k, v in x.items()}
    if isinstance(x, list):
        return [rnd(v) for v in x]
    return x


usdt = load("coinbase", "USDT", "USD")
res = {"method": __doc__.strip().splitlines()[0], "worstDays": WORST, "markets": {}, "files": len(glob.glob(os.path.join(D, "*.json")))}
for base in ["BTC", "ETH", "SOL"]:
    venues = {"coinbase": load("coinbase", base, "USD"), "bitstamp": load("bitstamp", base, "USD"),
              "bitfinex": load("bitfinex", base, "USD")}
    b = load("binance", base, "USDT")
    last_rate, conv = None, {}
    for t in sorted(set(b) | set(usdt)):
        if t in usdt:
            last_rate = usdt[t][3]
        if t in b and last_rate:
            conv[t] = tuple(x * last_rate for x in b[t])
    venues["binance"] = conv
    minutes = sorted(set().union(*[set(v) for v in venues.values()]))
    ref = {}
    disp, within, novenue = {}, {}, 0
    for t in minutes:
        have = [(n, v[t]) for n, v in venues.items() if t in v]
        if len(have) < 2:
            novenue += 1
            continue
        ref[t] = (statistics.median(x[1][3] for x in have), statistics.median(x[1][1] for x in have),
                  statistics.median(x[1][2] for x in have))
        if len(have) >= 3:
            m = ref[t][0]
            devs = [abs(x[1][3] - m) / m * 1e4 for x in have]
            disp[t] = max(devs)
            within[t] = (sum(1 for d in devs if d <= 200), len(have))
    day = lambda t: __import__("datetime").datetime.fromtimestamp(t / 1000, __import__("datetime").timezone.utc).strftime("%Y-%m-%d")
    ts = sorted(ref)
    sets = {"all": lambda t: True, "worst": lambda t: day(t) in WORST, "last30d": lambda t: day(t) not in WORST or day(t) == "2026-11-15"}
    for d in WORST:
        sets[d] = (lambda dd: (lambda t: day(t) == dd))(d)
    out = {"minutes": len(ts), "minutesWithFewerThan2Venues": novenue, "venueMinutes": {n: len(v) for n, v in venues.items()}}
    # jump
    ret, rng = {}, {}
    for i in range(1, len(ts)):
        if ts[i] - ts[i - 1] == 60000:
            ret[ts[i]] = abs(ref[ts[i]][0] / ref[ts[i - 1]][0] - 1) * 1e4
    for t in ts:
        rng[t] = (ref[t][1] - ref[t][2]) / ref[t][2] * 1e4
    out["jump"] = {}
    for name, f in sets.items():
        r = [v for t, v in ret.items() if f(t)]
        g = [v for t, v in rng.items() if f(t)]
        out["jump"][name] = {"ret1mBps": stats(r), "range1mBps": stats(g),
                             "minutesRangeOver": {str(x): sum(1 for v in g if v > x) for x in [200, 300, 500, 800, 1000]},
                             "minutesRetOver": {str(x): sum(1 for v in r if v > x) for x in [200, 300, 500, 800, 1000]}}
    # dispersion
    out["dispersion"] = {}
    for name, f in sets.items():
        d = [v for t, v in disp.items() if f(t)]
        w = [v for t, v in within.items() if f(t)]
        out["dispersion"][name] = {"maxDevBps": stats(d), "minutesOver": {str(x): sum(1 for v in d if v > x) for x in [25, 50, 100, 200]},
                                   "minutesFeederWouldAbstain": sum(1 for a, n in w if a < 3 or 2 * a <= n)}
    # excursion and settle error over contiguous runs
    exc = {m: {} for m in HORIZONS}
    settle = {}
    idx = {t: i for i, t in enumerate(ts)}
    for i, t in enumerate(ts):
        hi, lo = 0.0, 0.0
        c = ref[t][0]
        k = 1
        for m in HORIZONS:
            ok = True
            while k <= m:
                j = i + k
                if j >= len(ts) or ts[j] != t + k * 60000:
                    ok = False
                    break
                hi = max(hi, ref[ts[j]][1] / c - 1)
                lo = max(lo, 1 - ref[ts[j]][2] / c)
                k += 1
            if not ok:
                break
            exc[m][t] = max(hi, lo) * 1e4
        j = i + 60
        if j < len(ts) and ts[j] == t + 3600000:
            settle[t] = abs(ref[ts[j]][0] / c - 1) * 1e4
    out["excursion"] = {}
    for name in ["all", "worst", "last30d"]:
        f = sets[name]
        rows = {}
        for m in HORIZONS:
            v = [x for t, x in exc[m].items() if f(t)]
            band = min(5000, JUMP[base] + 100 * m)
            rows[str(m)] = {"stats": stats(v), "bandBps": band, "minutesOverBand": sum(1 for x in v if x > band)}
        out["excursion"][name] = rows
    worst_ratio = max(((x - JUMP[base]) / m) for m in HORIZONS for x in exc[m].values())
    out["minPerMinuteSlopeCoveringEverything"] = worst_ratio
    out["settleErrorAfter60mBps"] = {name: stats([x for t, x in settle.items() if sets[name](t)]) for name in ["all", "worst", "last30d"]}
    res["markets"][base] = out
print(json.dumps(rnd(res), indent=1))

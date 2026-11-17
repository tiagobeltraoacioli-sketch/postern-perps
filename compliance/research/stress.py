#!/usr/bin/env python3
"""Stress measurements for the launch markets from recorded public candles (two independent venues).

UNAUDITED prototype tooling. Reads only the recorded files in marketdata/<date>/ (no network access), so the
numbers in docs/MARKETS.md can be recomputed byte for byte:

    python3 -I compliance/research/stress.py compliance/research/marketdata/20261117

Venues: Kraken public OHLC (`/0/public/OHLC`, rows [time, open, high, low, close, vwap, volume, count]) and
Coinbase Exchange public candles (`/products/<id>/candles`, rows [time, low, high, open, close, volume]).
Integer arithmetic on prices scaled by 1e8 (no floats in the decision values); moves in basis points rounded UP
(a stress figure is never understated).

Metrics per market, venue and interval:
  worst_adverse_long_bps   max over candles of (open - low) / open      (a long's worst intra-candle loss)
  worst_adverse_short_bps  max over candles of (high - open) / open     (a short's worst intra-candle loss)
  worst_range_bps          max over candles of (high - low) / low
  candles, first_utc, last_utc
"""
import json
import os
import sys
from datetime import datetime, timezone
from decimal import Decimal

SCALE = 10**8


def px(v):
    return int((Decimal(str(v)) * SCALE).to_integral_value())


def bps_up(num, den):
    # ceil(num * 10000 / den) for num >= 0, den > 0
    return -((-num * 10000) // den)


def load(path, venue):
    with open(path, encoding="utf8") as f:
        raw = json.load(f)
    rows = []
    if venue == "kraken":
        if raw.get("error"):
            raise SystemExit(f"{path}: venue error {raw['error']}")
        (key,) = [k for k in raw["result"] if k != "last"]
        for r in raw["result"][key]:
            rows.append((int(r[0]), px(r[1]), px(r[2]), px(r[3]), px(r[4])))
    else:
        for r in raw:
            rows.append((int(r[0]), px(r[3]), px(r[2]), px(r[1]), px(r[4])))
    rows.sort()
    for t, o, h, l, c in rows:
        if not (l > 0 and l <= o <= h and l <= c <= h):
            raise SystemExit(f"{path}: malformed candle at {t}")
    return rows


def utc(t):
    return datetime.fromtimestamp(t, tz=timezone.utc).strftime("%Y-%m-%dT%H:%MZ")


def metrics(rows):
    wl = max(rows, key=lambda r: bps_up(r[1] - r[3], r[1]))
    ws = max(rows, key=lambda r: bps_up(r[2] - r[1], r[1]))
    wr = max(rows, key=lambda r: bps_up(r[2] - r[3], r[3]))
    return {
        "candles": len(rows),
        "first_utc": utc(rows[0][0]),
        "last_utc": utc(rows[-1][0]),
        "worst_adverse_long_bps": bps_up(wl[1] - wl[3], wl[1]),
        "worst_adverse_long_at": utc(wl[0]),
        "worst_adverse_short_bps": bps_up(ws[2] - ws[1], ws[1]),
        "worst_adverse_short_at": utc(ws[0]),
        "worst_range_bps": bps_up(wr[2] - wr[3], wr[3]),
        "worst_range_at": utc(wr[0]),
    }


FILES = {
    "BTC-USD": {"kraken": "kraken_XBTUSD", "coinbase": "cb_BTC-USD"},
    "ETH-USD": {"kraken": "kraken_ETHUSD", "coinbase": "cb_ETH-USD"},
    "SOL-USD": {"kraken": "kraken_SOLUSD", "coinbase": "cb_SOL-USD"},
}


def main(d):
    out = {}
    for m, venues in FILES.items():
        out[m] = {}
        for venue, stem in venues.items():
            for iv in ("1h", "1d"):
                out[m][f"{venue}_{iv}"] = metrics(load(os.path.join(d, f"{stem}_{iv}.json"), venue))
        for iv in ("1h", "1d"):
            k, c = out[m][f"kraken_{iv}"], out[m][f"coinbase_{iv}"]
            out[m][f"worst_both_{iv}_bps"] = max(
                k["worst_adverse_long_bps"], k["worst_adverse_short_bps"],
                c["worst_adverse_long_bps"], c["worst_adverse_short_bps"])
    json.dump(out, sys.stdout, indent=1, sort_keys=True)
    sys.stdout.write("\n")


if __name__ == "__main__":
    main(sys.argv[1] if len(sys.argv) > 1 else os.path.join(os.path.dirname(__file__), "marketdata", "20261117"))

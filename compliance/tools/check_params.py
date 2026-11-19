#!/usr/bin/env python3
"""Checks compliance/markets.json and compliance/risk.json against the rules R-1..R-10 of risk.json and recomputes
the insurance-fund seed from the recorded stress measurements. UNAUDITED prototype tooling; stdlib only; exact
integer / Decimal arithmetic (no floats).

    python3 -I compliance/tools/check_params.py            # exit 0 = every rule holds, 1 = a violation

Reference prices: the newest close of the recorded Coinbase 1h candles (compliance/research/marketdata/<date>/),
the same files compliance/research/stress.py measures. Stress: compliance/research/stress-<date>.json.
"""
import json
import os
import sys
from decimal import Decimal

HERE = os.path.dirname(os.path.abspath(__file__))
ROOT = os.path.dirname(HERE)
DATE = "20261117"
FEE_RECIPIENT = "0x775a993F09Bd80aeCF8F8c2ADe0C84158E3a0544"
PRICE_SCALE = 10**8
SIZE_SCALE = 10**8
USDC_SCALE = 10**6
CB_FILES = {"BTC-USD": "cb_BTC-USD_1h.json", "ETH-USD": "cb_ETH-USD_1h.json", "SOL-USD": "cb_SOL-USD_1h.json"}


def exact(s, scale, what):
    if not isinstance(s, str):
        raise ValueError(f"{what}: must be a decimal string, got {s!r}")
    v = Decimal(s) * scale
    if v != v.to_integral_value():
        raise ValueError(f"{what}: {s} is not exact at scale {scale} (S-1.2: rejected, never rounded)")
    return int(v)


def frac_digits(s):
    return len(s.split(".")[1]) if "." in s else 0


def ceil_div(a, b):
    return -((-a) // b)


# R-10: the exit band of a halted market (SPEC S-4.10, S-9.2, 14; oracle README 6a). Floors per market from agent
# 432's calibration on public minute candles, 34 days including the worst days (node/oracle/calibration/CALIBRATION.md
# on the oracle line, cross-area 2026-11-18 23:57 and 2026-11-19 00:11 UTC): BTC 500, ETH 700, SOL 1300 bps. For SOL
# the engine's default band (the jump bound, then +100 bps per minute) was too narrow on 2025-10-10: 1,003 bps in two
# minutes against 1,000 and 1,366 in five against 1,300. The table only ever WIDENS a band: a floor may be raised, never
# lowered, without new numbers.
EXIT_HAIRCUT_FLOOR_BPS = {"BTC-USD": 500, "ETH-USD": 700, "SOL-USD": 1300}
EXIT_FIELDS = ("exitHaircutBps", "exitHaircutPerMinBps", "exitMaxHaircutBps", "maxHaltMs")


def check_exit_band(s, o):
    """R-10 for one market's oracle object; returns violations."""
    v = []
    missing = [k for k in EXIT_FIELDS if not isinstance(o.get(k), int) or isinstance(o.get(k), bool)]
    if missing:
        return [f"R-10 {s}: oracle.{k} must be an integer (the exit band is stated per market, never left to a default)" for k in missing]
    if o["exitHaircutBps"] < o["jumpBps"]:
        v.append(f"R-10 {s}: exitHaircutBps {o['exitHaircutBps']} below jumpBps {o['jumpBps']} (the band must cover the move that halts)")
    if s not in EXIT_HAIRCUT_FLOOR_BPS:
        v.append(f"R-10 {s}: no calibrated exit-band floor for this market")
    elif o["exitHaircutBps"] < EXIT_HAIRCUT_FLOOR_BPS[s]:
        v.append(f"R-10 {s}: exitHaircutBps {o['exitHaircutBps']} below the calibrated floor {EXIT_HAIRCUT_FLOOR_BPS[s]}")
    if not 1 <= o["exitHaircutPerMinBps"]:
        v.append(f"R-10 {s}: exitHaircutPerMinBps must be >= 1 (the band widens while the market is dark)")
    if not o["exitHaircutBps"] <= o["exitMaxHaircutBps"] < 10000:
        v.append(f"R-10 {s}: need exitHaircutBps <= exitMaxHaircutBps < 10000")
    if o["maxHaltMs"] < o["maxStalenessMs"]:
        v.append(f"R-10 {s}: maxHaltMs below maxStalenessMs")
    if o["maxHaltMs"] > 86_400_000:
        v.append(f"R-10 {s}: maxHaltMs above one day (the vault's minimum forcedDelay: a halted position must settle before a forced request can expire)")
    return v


def reference_prices(root=ROOT):
    out = {}
    for m, f in CB_FILES.items():
        with open(os.path.join(root, "research", "marketdata", DATE, f), encoding="utf8") as fh:
            rows = json.load(fh)
        newest = max(rows, key=lambda r: r[0])
        out[m] = Decimal(str(newest[4]))
    return out


def check(markets, risk, stress, prices):
    """Returns (violations, report). Pure function of its inputs (tests mutate copies)."""
    v = []
    rep = {"seedTerms": {}, "oiCapUsd": {}}
    g = risk["global"]
    if g["feeRecipient"] != FEE_RECIPIENT:
        v.append("R-9 feeRecipient is not the owner's address")
    ft = risk["feeTiers"]["tiers"]
    seed = 0
    for i, t in enumerate(ft):
        if not (t["takerFeePpm"] >= t["makerFeePpm"] >= 0):
            v.append(f"R-8 fee tier {t['tier']}: taker >= maker >= 0 fails")
        if i and not (exact(t["minVolume14d"], 1, "minVolume14d") > exact(ft[i - 1]["minVolume14d"], 1, "minVolume14d")
                      and t["takerFeePpm"] <= ft[i - 1]["takerFeePpm"] and t["makerFeePpm"] <= ft[i - 1]["makerFeePpm"]):
            v.append(f"R-8 fee tier {t['tier']}: volumes must ascend and fees must not rise")
    ids = [m["marketId"] for m in markets["markets"]]
    if ids != sorted(set(ids)):
        v.append("market ids must be unique and ascending")
    for m in markets["markets"]:
        s = m["symbol"]
        try:
            tick = exact(m["tick"], PRICE_SCALE, f"{s}.tick")
            step = exact(m["step"], SIZE_SCALE, f"{s}.step")
            mn = exact(m["minSize"], SIZE_SCALE, f"{s}.minSize")
            mx = exact(m["maxSize"], SIZE_SCALE, f"{s}.maxSize")
            oi = exact(m["oiCap"], SIZE_SCALE, f"{s}.oiCap")
        except ValueError as e:
            v.append(f"R-7 {e}")
            continue
        if tick <= 0 or step <= 0:
            v.append(f"R-7 {s}: tick and step must be > 0")
            continue
        if frac_digits(m["tick"]) != m["priceDecimals"] or frac_digits(m["step"]) != m["sizeDecimals"]:
            v.append(f"R-7 {s}: display decimals differ from tick/step precision")
        if mn % step or mx % step or oi % step:
            v.append(f"R-7 {s}: sizes must be multiples of step")
        if not (0 < mn <= mx <= oi):
            v.append(f"R-7 {s}: need 0 < minSize <= maxSize <= oiCap")
        px = prices[s]
        if Decimal(mn) / SIZE_SCALE * px < 10:
            v.append(f"R-7 {s}: minSize notional below 10 USD at reference price {px}")
        tiers = m["tiers"]
        if exact(tiers[0]["floor"], USDC_SCALE, "floor") != 0:
            v.append(f"R-3 {s}: first tier floor must be 0")
        for i, t in enumerate(tiers):
            if t["mmfPpm"] <= m["markBandBps"] * 100:
                v.append(f"R-1 {s} tier {i}: mmf {t['mmfPpm']} ppm <= mark band {m['markBandBps']} bps")
            if t["mmfPpm"] != ceil_div(t["imfPpm"], 2):
                v.append(f"R-2 {s} tier {i}: mmf must be ceil(imf / 2)")
            if not (0 < t["mmfPpm"] < t["imfPpm"] <= 10**6):
                v.append(f"R-2 {s} tier {i}: need 0 < mmf < imf <= 1e6")
            if i:
                p = tiers[i - 1]
                if not (exact(t["floor"], USDC_SCALE, "floor") > exact(p["floor"], USDC_SCALE, "floor")
                        and t["imfPpm"] >= p["imfPpm"] and t["mmfPpm"] >= p["mmfPpm"]):
                    v.append(f"R-3 {s} tier {i}: floors ascending, fractions non-decreasing")
        t0 = tiers[0]
        if m["maxLeverage"] != 10**6 // t0["imfPpm"]:
            v.append(f"R-4 {s}: maxLeverage {m['maxLeverage']} != floor(1e6 / imf) = {10**6 // t0['imfPpm']}")
        if not m["maxFundingRate"] * 24 < (t0["imfPpm"] - t0["mmfPpm"]) * 1000:
            v.append(f"R-5 {s}: maxFundingRate * 24 must stay below imf - mmf (one day of capped funding)")
        if not (0 <= m.get("fundingClamp", 0) <= m["maxFundingRate"]):
            v.append(f"R-5 {s}: fundingClamp must be within [0, maxFundingRate]")
        st = stress[s]["worst_both_1h_bps"]
        if t0["mmfPpm"] * 2 < st * 100:
            v.append(f"R-6 {s}: tier-0 mmf {t0['mmfPpm']} ppm below half the worst 1h move {st} bps")
        if m["takerFeePpm"] != ft[0]["takerFeePpm"] or m["makerFeePpm"] != ft[0]["makerFeePpm"]:
            v.append(f"R-8 {s}: market fees differ from fee tier 0")
        v += check_exit_band(s, m["oracle"])
        oi_usd = Decimal(oi) / SIZE_SCALE * px
        term = oi_usd * max(0, st - t0["mmfPpm"] // 100) / 10000
        term = int(term.to_integral_value(rounding="ROUND_CEILING"))
        rep["oiCapUsd"][s] = str(oi_usd)
        rep["seedTerms"][s] = term
        seed += term
    rep["seedComputed"] = seed
    if exact(risk["insuranceFund"]["seedRequired"], 1, "seedRequired") < seed:
        v.append(f"insurance seed {risk['insuranceFund']['seedRequired']} below computed {seed}")
    return v, rep


def load_all(root=ROOT):
    with open(os.path.join(root, "markets.json"), encoding="utf8") as f:
        markets = json.load(f)
    with open(os.path.join(root, "risk.json"), encoding="utf8") as f:
        risk = json.load(f)
    with open(os.path.join(root, "research", f"stress-{DATE}.json"), encoding="utf8") as f:
        stress = json.load(f)
    return markets, risk, stress, reference_prices(root)


def main():
    v, rep = check(*load_all())
    json.dump(rep, sys.stdout, indent=1, sort_keys=True)
    sys.stdout.write("\n")
    for x in v:
        print("VIOLATION", x)
    print("OK" if not v else f"{len(v)} violation(s)")
    return 1 if v else 0


if __name__ == "__main__":
    sys.exit(main())

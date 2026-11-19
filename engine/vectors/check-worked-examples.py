#!/usr/bin/env python3
"""Independent check of engine/vectors/worked-examples.json (PosternDEX Perpetuals, UNAUDITED prototype).

Every number of the file is recomputed here from the text of docs/SPEC.md (sections 1, 5, 6, 7, 9, 15) with Python
integers, without the engine. The liquidation price is derived in closed form per tier segment and then checked
against the definition (liquidatable at it, not liquidatable one price unit on the safe side), so it does not share
the engine's bisection. Usage: python3 check-worked-examples.py [worked-examples.json]. Exit 0 = every case equal.
"""
import json
import os
import sys

N = 10**10          # notional units per USDC unit
PPM = 10**6
RATE = 10**9
MAX_PRICE = 10**15


def ceil_div(a, b):
    return -((-a) // b)


def trunc_div(a, b):
    q = abs(a) // abs(b)
    return q if (a >= 0) == (b > 0) else -q


def tier_req(n, tiers, frac):
    """SPEC 6.2: sum over tiers of (part of n inside the tier) * fraction, / 10^6 rounded up."""
    if n <= 0:
        return 0
    acc = 0
    for i, t in enumerate(tiers):
        lo = t["floor"] * N
        if n <= lo:
            break
        hi = tiers[i + 1]["floor"] * N if i + 1 < len(tiers) else None
        top = n if hi is None else min(n, hi)
        acc += (top - lo) * frac(t)
    return ceil_div(acc, PPM)


def mr(n, tiers):
    return tier_req(n, tiers, lambda t: t["mmf"])


def ir(n, tiers, lev):
    lf = ceil_div(PPM, lev)
    return tier_req(n, tiers, lambda t: max(t["imf"], lf))


def liquidatable(balance, size, open_notional, mark, tiers):
    e = balance * N + size * mark - open_notional
    return e < mr(abs(size) * mark, tiers)


def liquidation_price(balance, size, open_notional, tiers):
    """Long: highest mark with E < MR; short: lowest. Closed form per tier segment, then verified by definition.

    E(p) - MR(p) is increasing in p for a long and decreasing for a short (every mmf < 100%), so the threshold is
    unique. Within one tier segment the requirement before rounding is linear in p; the candidate from the real
    solution is corrected by a few units with the exact predicate (the ceil makes it a step function).
    """
    if size == 0:
        return None
    a = abs(size)

    def liq(p):
        return liquidatable(balance, size, open_notional, p, tiers)

    if size > 0:
        if not liq(1):
            return None
        if liq(MAX_PRICE):
            return MAX_PRICE
    else:
        if not liq(MAX_PRICE):
            return None
        if liq(1):
            return 1
    # candidates: for each tier k, solve E = MR assuming the notional a*p ends in tier k
    cands = set()
    for k, t in enumerate(tiers):
        # MR*10^6 = sum_{j<k} (floor_{j+1}-floor_j)*N*mmf_j + (a*p - floor_k*N)*mmf_k
        const = sum((tiers[j + 1]["floor"] - tiers[j]["floor"]) * N * tiers[j]["mmf"] for j in range(k)) - t["floor"] * N * t["mmf"]
        # (balance*N + size*p - open) * 10^6 = const + a*p*mmf_k
        num = const - (balance * N - open_notional) * PPM
        den = size * PPM - a * t["mmf"]
        if den != 0:
            cands.add(num // den)
    best = None
    for c in cands:
        for p in range(max(1, c - 3), min(MAX_PRICE, c + 3) + 1):
            if size > 0 and liq(p) and (p == MAX_PRICE or not liq(p + 1)):
                best = p
            if size < 0 and liq(p) and (p == 1 or not liq(p - 1)):
                best = p
    return best


def main():
    path = sys.argv[1] if len(sys.argv) > 1 else os.path.join(os.path.dirname(os.path.abspath(__file__)), "worked-examples.json")
    with open(path) as f:
        v = json.load(f)
    sets = {s["name"]: [{"floor": int(t["floor"]), "imf": int(t["imfPpm"]), "mmf": int(t["mmfPpm"])} for t in s["tiers"]] for s in v["tierSets"]}
    bad = []
    count = 0

    def eq(what, i, got, want):
        nonlocal count
        count += 1
        want = None if want is None else int(want)
        if got != want:
            bad.append(f"{what}[{i}]: computed {got}, file {want}")

    for i, c in enumerate(v["fees"]):
        n = int(c["price"]) * int(c["size"])
        eq("fees.notional", i, n, c["notional"])
        eq("fees.fee", i, ceil_div(n * int(c["feePpm"]), PPM * N), c["fee"])
    for i, c in enumerate(v["margin"]):
        t = sets[c["tiers"]]
        n = int(c["price"]) * int(c["size"])
        lev = int(c["leverage"])
        eq("margin.notional", i, n, c["notional"])
        eq("margin.leverageFractionPpm", i, ceil_div(PPM, lev), c["leverageFractionPpm"])
        eq("margin.initialReqNotional", i, ir(n, t, lev), c["initialReqNotional"])
        eq("margin.initialMargin", i, ceil_div(ir(n, t, lev), N), c["initialMargin"])
        eq("margin.maintenanceReqNotional", i, mr(n, t), c["maintenanceReqNotional"])
        eq("margin.maintenanceMargin", i, ceil_div(mr(n, t), N), c["maintenanceMargin"])
    for i, c in enumerate(v["liquidationPrice"]):
        t = sets[c["tiers"]]
        bal, size, on = int(c["balance"]), int(c["size"]), int(c["openNotional"])
        eq("liquidationPrice.openNotional", i, size * int(c["entryPrice"]), c["openNotional"])
        lp = liquidation_price(bal, size, on, t)
        eq("liquidationPrice.liquidationPrice", i, lp, c["liquidationPrice"])
        for key in ("atLiquidationPrice", "oneUnitOnTheSafeSide"):
            d = c[key]
            if d is None:
                continue
            m = int(d["mark"])
            eq(f"liquidationPrice.{key}.equityNotional", i, bal * N + size * m - on, d["equityNotional"])
            eq(f"liquidationPrice.{key}.maintenanceReqNotional", i, mr(abs(size) * m, t), d["maintenanceReqNotional"])
            count += 1
            if liquidatable(bal, size, on, m, t) != d["liquidatable"] or d["liquidatable"] != (key == "atLiquidationPrice"):
                bad.append(f"liquidationPrice[{i}].{key}.liquidatable")
    for i, c in enumerate(v["liquidationExecution"]):
        size, on, rest, mark = int(c["size"]), int(c["openNotional"]), int(c["restNotional"]), int(c["mark"])
        num = on - rest
        bp = ceil_div(num, size) if size > 0 else (-num) // (-size)
        bp = max(1, min(MAX_PRICE, bp))
        eq("liquidationExecution.bankruptcyPrice", i, bp, c["bankruptcyPrice"])
        off = mark * (int(c["mmf0Ppm"]) // 2) // PPM
        bound = max(bp, max(1, mark - off)) if size > 0 else min(bp, min(MAX_PRICE, mark + off))
        eq("liquidationExecution.liquidationBound", i, bound, c["liquidationBound"])
        eq("liquidationExecution.liquidationFeeBeforeCap", i, ceil_div(abs(size) * mark * int(c["liquidationFeePpm"]), PPM * N), c["liquidationFeeBeforeCap"])
    for i, c in enumerate(v["fundingRate"]):
        smp = sorted(int(x) for x in c["premiumSamples"])
        interest, cl, mx = int(c["interest"]), int(c["fundingClamp"]), int(c["maxFundingRate"])
        if not smp:
            rate = None
        else:
            p = smp[(len(smp) - 1) // 2]
            rate = max(-mx, min(mx, p + max(-cl, min(cl, interest - p))))
        eq("fundingRate.rate", i, rate, c["rate"])
    for i, c in enumerate(v["fundingPayment"]):
        x = int(c["size"]) * int(c["mark"]) * int(c["rate"])
        eq("fundingPayment.credit", i, (-x) // (RATE * N), c["credit"])
    for i, c in enumerate(v["premium"]):
        ix = int(c["index"])
        ib = ix if c["impactBid"] is None else int(c["impactBid"])
        ia = ix if c["impactAsk"] is None else int(c["impactAsk"])
        eq("premium.premium", i, trunc_div((max(0, ib - ix) - max(0, ix - ia)) * RATE, ix), c["premium"])
    for i, c in enumerate(v["mark"]):
        ix = int(c["index"])
        g = lambda k: ix if c[k] is None else int(c[k])
        ref = sorted([g("bestBid"), g("bestAsk"), g("freshLastTrade")])[1]
        band = ix * int(c["markBandBps"]) // 10000
        eq("mark.mark", i, max(1, min(MAX_PRICE, ix + max(-band, min(band, ref - ix)))), c["mark"])
    for i, c in enumerate(v["pnl"]):
        u = int(c["size"]) * int(c["mark"]) - int(c["openNotional"])
        eq("pnl.unrealisedNotional", i, u, c["unrealisedNotional"])
        eq("pnl.unrealisedUsdcTowardProtocol", i, u // N, c["unrealisedUsdcTowardProtocol"])
    if bad:
        print(f"FAIL: {len(bad)} of {count} values differ")
        for b in bad[:40]:
            print("  " + b)
        sys.exit(1)
    print(f"OK: {count} values of {os.path.basename(path)} recomputed independently and equal")


if __name__ == "__main__":
    main()

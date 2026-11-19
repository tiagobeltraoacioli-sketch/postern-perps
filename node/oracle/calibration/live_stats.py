#!/usr/bin/env python3
"""Statistics of a live feeder run (`perps-oracle run --out FILE`): gaps, venue failures and latency, rate-limit
answers, disagreement between venues, round-to-round moves. UNAUDITED PROTOTYPE. Statistics only (floats).

Usage: python3 live_stats.py live-hour.jsonl > live-hour-stats.json
"""
import json, sys
from collections import Counter, defaultdict
from urllib.parse import urlsplit


def q(v, p):
    if not v:
        return None
    s = sorted(v)
    return s[min(len(s) - 1, int(p * len(s)))]


def st(v):
    return {"n": len(v), "p50": q(v, 0.5), "p90": q(v, 0.9), "p99": q(v, 0.99), "max": max(v) if v else None}


def rnd(x):
    if isinstance(x, float):
        return round(x, 3)
    if isinstance(x, dict):
        return {k: rnd(v) for k, v in x.items()}
    if isinstance(x, list):
        return [rnd(v) for v in x]
    return x


lines = [json.loads(l) for l in open(sys.argv[1]) if l.strip()]
health = [l for l in lines if "health" in l]
rounds = [l for l in lines if "health" not in l]
rep = [l["report"] for l in rounds]
out = {"rounds": len(rounds), "firstRound": rep[0]["round"], "lastRound": rep[-1]["round"],
       "firstTimeMs": rep[0]["timeMs"], "lastTimeMs": rep[-1]["timeMs"],
       "roundsMissingInSequence": rep[-1]["round"] - rep[0]["round"] + 1 - len(rounds),
       "clockRefused": sum(1 for l in rounds if "refused" in l),
       "noObservation": sum(1 for l in rounds if l.get("observation") is None and "refused" not in l),
       "samplingDelayMs": st([r["timeMs"] % 3000 for r in rep])}
# fetch units
by_host = defaultdict(lambda: {"requests": 0, "status": Counter(), "errors": Counter(), "ms": []})
for l in rounds:
    for f in l["fetches"]:
        h = by_host[urlsplit(f["url"]).netloc]
        h["requests"] += 1
        h["status"][str(f["status"])] += 1
        if f["error"]:
            h["errors"][f["error"][:60]] += 1
        h["ms"].append(f["ms"])
out["venueRequests"] = {k: {"requests": v["requests"], "status": dict(v["status"]), "errors": dict(v["errors"]), "latencyMs": st(v["ms"])} for k, v in sorted(by_host.items())}
out["requestsPerRound"] = st([len(l["fetches"]) for l in rounds])
out["dropEvents"] = [e for l in rounds for e in l.get("events", [])]
out["health"] = health[-1]["health"] if health else None
# markets
out["markets"] = {}
for mi in range(len(rep[0]["markets"])):
    sym = rep[0]["markets"][mi]["symbol"]
    prices, srcs, abst, spread, gaps, gap = [], Counter(), Counter(), [], [], 0
    reasons = defaultdict(Counter)
    dev = defaultdict(list)
    used = Counter()
    for r in rep:
        m = r["markets"][mi]
        if m["price"] is None:
            abst[m["abstain"]] += 1
            gap += 1
        else:
            if gap:
                gaps.append(gap)
            gap = 0
            p = int(m["price"]) / 1e8
            prices.append((r["round"], p))
            srcs[str(m["sources"])] += 1
            vals = [float(v["price"]) for v in m["venues"] if v["price"] is not None]
            if len(vals) >= 2:
                spread.append((max(vals) - min(vals)) / p * 1e4)
            for v in m["venues"]:
                if v["price"] is not None:
                    dev[v["name"]].append(abs(float(v["price"]) - p) / p * 1e4)
                    used[v["name"]] += 1 if v["used"] else 0
        for v in m["venues"]:
            reasons[v["name"]][v["reason"] or "ok"] += 1
    if gap:
        gaps.append(gap)
    moves = [abs(b[1] / a[1] - 1) * 1e4 for a, b in zip(prices, prices[1:]) if b[0] == a[0] + 1]
    out["markets"][sym] = {
        "roundsPriced": len(prices), "abstain": dict(abst), "sourcesHistogram": dict(sorted(srcs.items())),
        "gapsOfRoundsWithoutPrice": {"count": len(gaps), "longest": max(gaps) if gaps else 0},
        "priceFirstLast": [prices[0][1], prices[-1][1]] if prices else None,
        "priceMinMax": [min(p for _, p in prices), max(p for _, p in prices)] if prices else None,
        "venueSpreadBps(max-min over valid venues)": st(spread),
        "roundToRoundMoveBps(3 s)": st(moves),
        "venueDeviationFromObservationBps": {k: st(v) for k, v in sorted(dev.items())},
        "venueReasons": {k: dict(v) for k, v in sorted(reasons.items())},
        "venueUsedRounds": dict(sorted(used.items())),
    }
u = [float(r["usdtUsd"]) for r in rep if r["usdtUsd"]]
out["usdtUsd"] = {"roundsWithRate": len(u), "min": f"{min(u):.5f}" if u else None, "max": f"{max(u):.5f}" if u else None}
print(json.dumps(rnd(out), indent=1))

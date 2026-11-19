#!/usr/bin/env python3
"""Markdown tables of CALIBRATION.md section 4 from results/live-hour-20261118-stats.json.
Usage: python3 live_tables.py results/live-hour-20261118-stats.json"""
import json, sys

d = json.load(open(sys.argv[1]))
f = lambda x: "-" if x is None else (f"{x:.0f}" if abs(x) >= 100 else f"{x:.2f}")
dur = (d["lastTimeMs"] - d["firstTimeMs"]) / 1000
print(f"Rounds {d['rounds']} (rounds {d['firstRound']} to {d['lastRound']}, {dur:.0f} s, {dur / 60:.1f} min); rounds missing in the")
print(f"sequence {d['roundsMissingInSequence']}; refused by the clock check {d['clockRefused']}; rounds with nothing to sign {d['noObservation']};")
print(f"requests per round p50 {d['requestsPerRound']['p50']}, max {d['requestsPerRound']['max']}; drop events {len(d['dropEvents'])};")
print(f"USDT/USD rate between {d['usdtUsd']['min']} and {d['usdtUsd']['max']} ({d['usdtUsd']['roundsWithRate']} rounds with a rate).\n")
print("### L1 requests by venue host\n")
print("| Host | requests | HTTP 200 | other answers | no answer (timeout 1.5 s) | latency p50 / p90 / p99 / max (ms) |")
print("|---|---|---|---|---|---|")
for h, v in d["venueRequests"].items():
    ok = v["status"].get("200", 0)
    none = v["status"].get("0", 0)
    other = {k: n for k, n in v["status"].items() if k not in ("200", "0")}
    l = v["latencyMs"]
    print(f"| {h} | {v['requests']} | {ok} | {other if other else 0} | {none} | {l['p50']} / {l['p90']} / {l['p99']} / {l['max']} |")
print("\n### L2 markets\n")
print("| Market | rounds priced | rounds abstained | longest run without a price | sources 7 / 6 / 5 / fewer | price first -> last (min .. max) | venue spread bps p50 / p99 / max | 3 s move of the observation bps p50 / p90 / p99 / max |")
print("|---|---|---|---|---|---|---|---|")
for s, m in d["markets"].items():
    h = m["sourcesHistogram"]
    fewer = sum(n for k, n in h.items() if int(k) < 5)
    sp, mv = m["venueSpreadBps(max-min over valid venues)"], m["roundToRoundMoveBps(3 s)"]
    print(f"| {s} | {m['roundsPriced']} | {sum(m['abstain'].values())} | {m['gapsOfRoundsWithoutPrice']['longest']} | {h.get('7', 0)} / {h.get('6', 0)} / {h.get('5', 0)} / {fewer} | {m['priceFirstLast'][0]} -> {m['priceFirstLast'][1]} ({m['priceMinMax'][0]} .. {m['priceMinMax'][1]}) | {f(sp['p50'])} / {f(sp['p99'])} / {f(sp['max'])} | {f(mv['p50'])} / {f(mv['p90'])} / {f(mv['p99'])} / {f(mv['max'])} |")
print("\n### L3 venues: rounds used, reasons a reading was not valid, distance from the feeder's observation (bps)\n")
print("| Source | used | not valid (reason: rounds) | distance p50 / p99 / max |")
print("|---|---|---|---|")
for s, m in d["markets"].items():
    for v, r in m["venueReasons"].items():
        bad = {k: n for k, n in r.items() if k != "ok"}
        dv = m["venueDeviationFromObservationBps"].get(v, {})
        print(f"| {v} | {m['venueUsedRounds'].get(v, 0)} | {bad if bad else 0} | {f(dv.get('p50'))} / {f(dv.get('p99'))} / {f(dv.get('max'))} |")

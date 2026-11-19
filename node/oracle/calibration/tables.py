#!/usr/bin/env python3
"""Markdown tables of CALIBRATION.md from results/candles-20261118.json (so the document's numbers are the file's).
Usage: python3 tables.py results/candles-20261118.json"""
import json, sys

d = json.load(open(sys.argv[1]))
M = d["markets"]
PROPOSED = {"BTC": 500, "ETH": 700, "SOL": 1300}
JUMP = {"BTC": 500, "ETH": 500, "SOL": 800}
f = lambda x: "-" if x is None else (f"{x:.0f}" if x >= 100 else f"{x:.1f}")

print("### T1 minute moves of the four-venue median (bps)\n")
print("| Market | set | minutes | 1m return p50 | p99.9 | p99.99 | max | 1m range max | minutes with range > 300 / 500 / 800 / 1000 |")
print("|---|---|---|---|---|---|---|---|---|")
for b in M:
    for k in ["last30d", "worst", "2025-10-10", "2026-01-31", "2026-02-05", "2026-11-15"]:
        j = M[b]["jump"][k]
        r, o = j["ret1mBps"], j["minutesRangeOver"]
        print(f"| {b} | {k} | {r['n'] + 1} | {f(r['p50'])} | {f(r['p999'])} | {f(r['p9999'])} | {f(r['max'])} | {f(j['range1mBps']['max'])} | {o['300']} / {o['500']} / {o['800']} / {o['1000']} |")

print("\n### T2 dispersion between venues, per minute (largest distance of a venue's close from the median, bps)\n")
print("| Market | set | minutes | p50 | p99 | p99.9 | max | minutes > 50 / 100 / 200 | minutes the feeder rule F5 would abstain |")
print("|---|---|---|---|---|---|---|---|---|")
for b in M:
    for k in ["last30d", "worst"]:
        x = M[b]["dispersion"][k]
        s, o = x["maxDevBps"], x["minutesOver"]
        print(f"| {b} | {k} | {s['n']} | {f(s['p50'])} | {f(s['p99'])} | {f(s['p999'])} | {f(s['max'])} | {o['50']} / {o['100']} / {o['200']} | {x['minutesFeederWouldAbstain']} |")

print("\n### T3 largest move away from the price at t during the next m minutes (bps), against the exit band\n")
print("| Market | m (min) | p99.9 (all 35 days) | max (last 30 d) | max (worst days) | band now: jumpBps + 100 m | minutes over it | band proposed | proposed / max |")
print("|---|---|---|---|---|---|---|---|---|")
for b in M:
    for m in ["1", "2", "5", "10", "15", "30", "60"]:
        a, l, w = M[b]["excursion"]["all"][m], M[b]["excursion"]["last30d"][m], M[b]["excursion"]["worst"][m]
        now = min(5000, JUMP[b] + 100 * int(m))
        prop = min(5000, PROPOSED[b] + 100 * int(m))
        mx = max(a["stats"]["max"], w["stats"]["max"])
        print(f"| {b} | {m} | {f(a['stats']['p999'])} | {f(l['stats']['max'])} | {f(w['stats']['max'])} | {now} | {a['minutesOverBand']} | {prop} | {prop / mx:.2f} |")

print("\n### T4 error of a settlement at the pre-halt index after 60 minutes without a committee price (bps)\n")
print("| Market | set | p50 | p99 | p99.9 | max |")
print("|---|---|---|---|---|---|")
for b in M:
    for k in ["last30d", "worst"]:
        s = M[b]["settleErrorAfter60mBps"][k]
        print(f"| {b} | {k} | {f(s['p50'])} | {f(s['p99'])} | {f(s['p999'])} | {f(s['max'])} |")
print("\n### Coverage\n")
print("| Market | minutes with a reference | minutes with fewer than 2 venues | venue minutes |")
print("|---|---|---|---|")
for b in M:
    print(f"| {b} | {M[b]['minutes']} | {M[b]['minutesWithFewerThan2Venues']} | {', '.join(f'{k} {v}' for k, v in M[b]['venueMinutes'].items())} |")

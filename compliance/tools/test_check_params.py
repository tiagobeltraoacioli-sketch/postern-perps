#!/usr/bin/env python3
"""Tests for check_params.py: the committed files pass, and every rule R-1..R-10 rejects a mutation that breaks it
(each mutation is a test that would pass vacuously if its rule were removed). Also checks stress.py reproduces the
committed stress file byte for byte.

    python3 -I -m unittest discover -s compliance/tools -p 'test_*.py' -v
"""
import copy
import io
import json
import os
import subprocess
import sys
import unittest
from contextlib import redirect_stdout

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
import check_params as cp  # noqa: E402

BASE = cp.load_all()


def run(mut):
    m, r, s, p = (copy.deepcopy(x) for x in BASE)
    mut(m, r, s, p)
    return cp.check(m, r, s, p)[0]


def mkt(m, sym):
    return next(x for x in m["markets"] if x["symbol"] == sym)


class ExitBandR10(unittest.TestCase):
    """R-10 (0.2.2, asked by agent 432 with its calibration): every mutation below must be refused."""

    def has(self, v, text):
        self.assertTrue(any(x.startswith("R-10") and text in x for x in v), f"{text!r} not raised: {v}")

    def test_committed_values_are_the_calibrated_ones(self):
        got = {x["symbol"]: [x["oracle"][k] for k in cp.EXIT_FIELDS] for x in BASE[0]["markets"]}
        self.assertEqual(got, {"BTC-USD": [500, 100, 5000, 3600000], "ETH-USD": [700, 100, 5000, 3600000], "SOL-USD": [1300, 100, 5000, 3600000]})

    def test_a_missing_field_is_not_left_to_a_default(self):
        for k in cp.EXIT_FIELDS:
            self.has(run(lambda m, r, s, p, k=k: mkt(m, "ETH-USD")["oracle"].pop(k)), "oracle." + k)

    def test_band_below_the_jump_bound(self):
        def mut(m, r, s, p):
            mkt(m, "SOL-USD")["oracle"]["jumpBps"] = 1400
        self.has(run(mut), "below jumpBps")

    def test_band_below_the_calibrated_floor(self):
        for sym, floor in cp.EXIT_HAIRCUT_FLOOR_BPS.items():
            self.has(run(lambda m, r, s, p, sym=sym, floor=floor: mkt(m, sym)["oracle"].__setitem__("exitHaircutBps", floor - 1)), "calibrated floor")

    def test_band_that_never_widens(self):
        self.has(run(lambda m, r, s, p: mkt(m, "BTC-USD")["oracle"].__setitem__("exitHaircutPerMinBps", 0)), "exitHaircutPerMinBps")

    def test_maximum_haircut_bounds(self):
        self.has(run(lambda m, r, s, p: mkt(m, "BTC-USD")["oracle"].__setitem__("exitMaxHaircutBps", 10000)), "exitMaxHaircutBps < 10000")
        self.has(run(lambda m, r, s, p: mkt(m, "SOL-USD")["oracle"].__setitem__("exitMaxHaircutBps", 1299)), "exitMaxHaircutBps < 10000")

    def test_max_halt_bounds(self):
        self.has(run(lambda m, r, s, p: mkt(m, "BTC-USD")["oracle"].__setitem__("maxHaltMs", 9999)), "below maxStalenessMs")
        self.has(run(lambda m, r, s, p: mkt(m, "BTC-USD")["oracle"].__setitem__("maxHaltMs", 86_400_001)), "above one day")

    def test_a_float_or_a_string_is_not_an_integer(self):
        self.has(run(lambda m, r, s, p: mkt(m, "BTC-USD")["oracle"].__setitem__("exitHaircutBps", "500")), "must be an integer")
        self.has(run(lambda m, r, s, p: mkt(m, "BTC-USD")["oracle"].__setitem__("exitHaircutBps", 500.0)), "must be an integer")
        self.has(run(lambda m, r, s, p: mkt(m, "BTC-USD")["oracle"].__setitem__("exitHaircutBps", True)), "must be an integer")


class CommittedFiles(unittest.TestCase):
    def test_pass(self):
        v, rep = cp.check(*BASE)
        self.assertEqual(v, [])
        self.assertLessEqual(rep["seedComputed"], int(BASE[1]["insuranceFund"]["seedRequired"]))
        self.assertEqual(rep["seedComputed"], 57241)

    def test_three_markets_ids(self):
        self.assertEqual([x["marketId"] for x in BASE[0]["markets"]], [1, 2, 3])
        self.assertEqual([x["symbol"] for x in BASE[0]["markets"]], ["BTC-USD", "ETH-USD", "SOL-USD"])

    def test_stress_reproducible(self):
        root = cp.ROOT
        out = subprocess.run([sys.executable, "-I", os.path.join(root, "research", "stress.py"),
                              os.path.join(root, "research", "marketdata", cp.DATE)],
                             capture_output=True, check=True).stdout
        with open(os.path.join(root, "research", f"stress-{cp.DATE}.json"), "rb") as f:
            self.assertEqual(out, f.read())

    def test_main_exit_zero(self):
        with redirect_stdout(io.StringIO()):
            self.assertEqual(cp.main(), 0)


class EachRuleRejects(unittest.TestCase):
    def has(self, v, rule):
        self.assertTrue(any(x.startswith(rule) for x in v), f"{rule} not raised: {v}")

    def test_r1_mark_band(self):
        def mut(m, r, s, p):
            m["markets"][0]["markBandBps"] = 250
        self.has(run(mut), "R-1")

    def test_r2_half(self):
        def mut(m, r, s, p):
            m["markets"][0]["tiers"][0]["mmfPpm"] = 20000
        self.has(run(mut), "R-2")

    def test_r3_ascending(self):
        def mut(m, r, s, p):
            m["markets"][0]["tiers"][2]["floor"] = "100000"
        self.has(run(mut), "R-3")

    def test_r3_first_floor(self):
        def mut(m, r, s, p):
            m["markets"][0]["tiers"][0]["floor"] = "1"
        self.has(run(mut), "R-3")

    def test_r4_leverage(self):
        def mut(m, r, s, p):
            m["markets"][0]["maxLeverage"] = 40
        self.has(run(mut), "R-4")

    def test_r5_funding(self):
        def mut(m, r, s, p):
            m["markets"][0]["maxFundingRate"] = 5000000
        self.has(run(mut), "R-5")

    def test_r5_clamp(self):
        def mut(m, r, s, p):
            m["markets"][0]["fundingClamp"] = 2000000
        self.has(run(mut), "R-5")

    def test_r6_stress(self):
        def mut(m, r, s, p):
            s["BTC-USD"]["worst_both_1h_bps"] = 600
        self.has(run(mut), "R-6")

    def test_r7_inexact(self):
        def mut(m, r, s, p):
            m["markets"][0]["tick"] = "0.000000001"
        self.has(run(mut), "R-7")

    def test_r7_min_notional(self):
        def mut(m, r, s, p):
            m["markets"][0]["minSize"] = "0.0001"
        self.has(run(mut), "R-7")

    def test_r7_display(self):
        def mut(m, r, s, p):
            m["markets"][1]["priceDecimals"] = 1
        self.has(run(mut), "R-7")

    def test_r7_max_over_oi(self):
        def mut(m, r, s, p):
            m["markets"][2]["maxSize"] = "7000"
        self.has(run(mut), "R-7")

    def test_r7_number_not_string(self):
        def mut(m, r, s, p):
            m["markets"][2]["step"] = 0.01
        self.has(run(mut), "R-7")

    def test_r8_maker_over_taker(self):
        def mut(m, r, s, p):
            r["feeTiers"]["tiers"][2]["makerFeePpm"] = 400
        self.has(run(mut), "R-8")

    def test_r8_rebate(self):
        def mut(m, r, s, p):
            r["feeTiers"]["tiers"][4]["makerFeePpm"] = -10
        self.has(run(mut), "R-8")

    def test_r8_market_vs_tier0(self):
        def mut(m, r, s, p):
            m["markets"][1]["takerFeePpm"] = 400
        self.has(run(mut), "R-8")

    def test_r9_fee_recipient(self):
        def mut(m, r, s, p):
            r["global"]["feeRecipient"] = "0x775a993F09Bd80aeCF8F8c2ADe0C84158E3a0545"
        self.has(run(mut), "R-9")

    def test_seed_too_small(self):
        def mut(m, r, s, p):
            r["insuranceFund"]["seedRequired"] = "50000"
        self.has(run(mut), "insurance seed")

    def test_seed_grows_with_oi(self):
        def mut(m, r, s, p):
            m["markets"][0]["oiCap"] = "60"
        self.has(run(mut), "insurance seed")

    def test_duplicate_market_id(self):
        def mut(m, r, s, p):
            m["markets"][2]["marketId"] = 2
        self.assertIn("market ids must be unique and ascending", run(mut))


if __name__ == "__main__":
    unittest.main()

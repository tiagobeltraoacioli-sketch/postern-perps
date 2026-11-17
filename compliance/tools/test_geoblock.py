#!/usr/bin/env python3
"""Tests of compliance/geoblock.json 0.2 against its inputs and against 354's 0.1.1 guards (never weakened).

    python3 -I -m unittest discover -s compliance/tools -p 'test_*.py' -v
"""
import copy
import json
import os
import sys
import unittest

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
import build_geoblock as bg  # noqa: E402

with open(bg.OUT, encoding="utf8") as _f:
    GEO = json.load(_f)
OLD = bg.load("geoblock-0.1.1.json")
SRC = {s["id"]: s for s in bg.load("sources.json")["sources"]}
ISO = bg.iso_codes()


def violations(geo, old=OLD, src=SRC, iso=ISO):
    """Every property the list must have. Pure function so the tests can mutate copies."""
    v = []
    for k in ("rule", "tiers", "default_tier", "unknown_country_effect", "denied_policy"):
        if geo.get(k) != old.get(k):
            v.append(f"G-1 {k} changed from 0.1.1")
    if geo["tiers"]["restricted-review"]["effect"] != "deny" or geo["unknown_country_effect"] != "deny":
        v.append("G-1 deny by default lost")
    for kind in ("tor", "vpn_proxy"):
        if geo["anonymizers"].get(kind) != "deny":
            v.append(f"G-2 anonymizer {kind} not denied")
    if geo["anonymizers"].get("hosting") != old["anonymizers"]["hosting"]:
        v.append("G-2 hosting rule changed")
    for table in ("countries", "subdivisions"):
        for code, e in old[table].items():
            n = geo[table].get(code)
            if n is None:
                v.append(f"G-3 {table} {code} of 0.1.1 missing")
            elif bg.STRICTNESS[n["tier"]] < bg.STRICTNESS[e["tier"]]:
                v.append(f"G-3 {table} {code} weakened from {e['tier']} to {n['tier']}")
    missing = set(iso) - set(geo["countries"])
    if missing:
        v.append(f"G-4 ISO codes missing: {sorted(missing)}")
    extra = set(geo["countries"]) - set(iso)
    if extra:
        v.append(f"G-4 codes not in ISO 3166-1: {sorted(extra)}")
    cats = set(geo["reason_categories"])
    for table in ("countries", "subdivisions"):
        for code, e in geo[table].items():
            if e["tier"] not in geo["tiers"]:
                v.append(f"G-5 {code}: unknown tier")
            for r in e["reasons"]:
                if r not in cats:
                    v.append(f"G-5 {code}: reason {r} not in reason_categories")
            for s in e["sources"]:
                if s not in src:
                    v.append(f"G-6 {code}: source {s} not in sources.json")
            for q in e.get("counsel", []):
                if q not in geo["research"]["counsel_questions"]:
                    v.append(f"G-6 {code}: counsel question {q} undefined")
            if e["tier"] == "allowed":
                good = [s for s in e["sources"] if s in src and "UNVERIFIED" not in src[s].get("status", "")
                        and src[s].get("quotes")]
                if not good or e.get("confidence") != "high" or "not-researched" in e["reasons"]:
                    v.append(f"G-7 {code}: allowed without a verified, quoted source at high confidence")
            if e["tier"] == "blocked" and not e["sources"] and not e.get("counsel"):
                v.append(f"G-8 {code}: blocked without sources must carry a counsel question")
            if e["researched"] != bool(e["sources"]):
                v.append(f"G-8 {code}: researched flag inconsistent")
    return v


class Geoblock(unittest.TestCase):
    def test_committed_is_build(self):
        with open(bg.OUT, encoding="utf8") as f:
            text = f.read()
        self.assertEqual(text, bg.build())
        with open(bg.OUT_MD, encoding="utf8") as f:
            self.assertEqual(f.read(), bg.render_md(text))

    def test_no_violations(self):
        self.assertEqual(violations(GEO), [])

    def test_counts(self):
        self.assertEqual(len(GEO["countries"]), 249)
        tiers = [e["tier"] for e in GEO["countries"].values()]
        self.assertEqual(tiers.count("allowed"), 0)
        self.assertGreaterEqual(tiers.count("blocked"), 17)  # 0.1.1 had 17 blocked countries
        self.assertEqual(len(GEO["subdivisions"]), 6)

    def test_brazil_blocked_with_primary_sources(self):
        br = GEO["countries"]["BR"]
        self.assertEqual(br["tier"], "blocked")
        self.assertTrue({"S-CVM-BYBIT", "S-BR-L6385", "S-BR-L14478"} <= set(br["sources"]))
        for s in br["sources"]:
            self.assertTrue(SRC[s]["quotes"])

    def test_every_quoted_source_has_url_and_date(self):
        for s in SRC.values():
            self.assertTrue(s.get("url") and s.get("read_utc"), s["id"])


class Mutations(unittest.TestCase):
    def mut(self, f):
        g = copy.deepcopy(GEO)
        f(g)
        return violations(g)

    def has(self, v, tag):
        self.assertTrue(any(x.startswith(tag) for x in v), f"{tag} not raised: {v}")

    def test_g1_unknown_allowed(self):
        self.has(self.mut(lambda g: g.__setitem__("unknown_country_effect", "allow")), "G-1")

    def test_g1_review_allowed(self):
        self.has(self.mut(lambda g: g["tiers"]["restricted-review"].__setitem__("effect", "allow")), "G-1")

    def test_g1_exit_only_changed(self):
        self.has(self.mut(lambda g: g["denied_policy"].__setitem__("policy", "none")), "G-1")

    def test_g2_vpn(self):
        self.has(self.mut(lambda g: g["anonymizers"].__setitem__("vpn_proxy", "allow")), "G-2")

    def test_g3_weaken_brazil(self):
        self.has(self.mut(lambda g: g["countries"]["BR"].__setitem__("tier", "restricted-review")), "G-3")

    def test_g3_drop_crimea(self):
        self.has(self.mut(lambda g: g["subdivisions"].pop("UA-43")), "G-3")

    def test_g4_missing_iso(self):
        self.has(self.mut(lambda g: g["countries"].pop("FR")), "G-4")

    def test_g5_bad_reason(self):
        self.has(self.mut(lambda g: g["countries"]["GB"]["reasons"].append("because")), "G-5")

    def test_g6_bad_source(self):
        self.has(self.mut(lambda g: g["countries"]["GB"]["sources"].append("S-NOPE")), "G-6")

    def test_g7_allowed_unresearched(self):
        self.has(self.mut(lambda g: g["countries"]["PA"].__setitem__("tier", "allowed")), "G-7")

    def test_g7_allowed_on_unverified_source(self):
        def f(g):
            g["countries"]["DE"].update(tier="allowed", confidence="high", reasons=["authorisation-required"])
        self.has(self.mut(f), "G-7")

    def test_g8_blocked_unsourced_needs_counsel(self):
        self.has(self.mut(lambda g: g["subdivisions"]["UA-23"].pop("counsel")), "G-8")


if __name__ == "__main__":
    unittest.main()

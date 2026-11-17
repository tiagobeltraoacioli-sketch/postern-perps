#!/usr/bin/env python3
"""Tests that the legal drafts and the Interface texts agree with the machine-readable parameters and the
geoblock list, so a parameter or list change that is not carried into the texts fails here.

    python3 -I -m unittest discover -s compliance/tools -p 'test_*.py' -v
"""
import hashlib
import json
import os
import sys
import unittest

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
import build_notices as bn  # noqa: E402

COMP = bn.COMP
with open(bn.OUT, encoding="utf8") as _f:
    NOT = json.load(_f)
with open(os.path.join(COMP, "markets.json"), encoding="utf8") as _f:
    MK = json.load(_f)
with open(os.path.join(COMP, "risk.json"), encoding="utf8") as _f:
    RK = json.load(_f)
with open(os.path.join(COMP, "geoblock.json"), encoding="utf8") as _f:
    GEO = json.load(_f)


def read(rel):
    with open(os.path.join(bn.REPO, rel), encoding="utf8") as f:
        return f.read()


TERMS = " ".join(read("compliance/legal/terms-of-use.md").split())  # whitespace-normalised for phrase checks
RISK = " ".join(read("compliance/legal/risk-disclosure.md").split())
NAMES = {  # the words the texts use for each blocked code
    "US": "United States", "PR": "territories", "GU": "territories", "VI": "territories", "AS": "territories",
    "MP": "territories", "UM": "territories", "CA": "Canada", "GB": "United Kingdom", "BR": "Brazil",
    "CN": "China", "CU": "Cuba", "IR": "Iran", "KP": "North Korea", "SY": "Syria", "RU": "Russia",
    "BY": "Belarus", "MM": "Myanmar", "AU": "Australia", "SG": "Singapore", "UA-43": "Crimea", "UA-40": "Sevastopol", "UA-14": "Donetsk",
    "UA-09": "Luhansk", "UA-23": "Zaporizhzhia", "UA-65": "Kherson",
}


def blocked_codes(geo):
    return sorted([c for c, e in geo["countries"].items() if e["tier"] == "blocked"]
                  + [c for c, e in geo["subdivisions"].items() if e["tier"] == "blocked"])


class Notices(unittest.TestCase):
    def test_committed_is_build(self):
        with open(bn.OUT, encoding="utf8") as f:
            self.assertEqual(f.read(), bn.build())

    def test_document_hashes(self):
        for d in NOT["documents"].values():
            with open(os.path.join(bn.REPO, d["path"]), "rb") as f:
                self.assertEqual(hashlib.sha256(f.read()).hexdigest(), d["sha256"])

    def test_attestation_hash(self):
        a = NOT["attestation"]
        self.assertEqual(hashlib.sha256(a["text"].encode("utf8")).hexdigest(), a["sha256"])
        self.assertEqual(a["eip712"]["documentHash"], "0x" + a["sha256"])
        self.assertTrue(a["text"].startswith(a["documentId"] + "\n"))
        d = a["eip712"]["domain"]
        self.assertEqual((d["name"], d["version"], d["chainId"]), ("PosternPerps", "1", 700770))  # SPEC 2.1
        self.assertIn("PerpsVault", d["verifyingContract"])  # bound to one deployment (R395-2)

    def test_every_blocked_entry_named_in_terms_and_summary(self):
        codes = blocked_codes(GEO)
        self.assertEqual(set(codes), set(NAMES), "blocked list changed: update NAMES, the terms and the summary")
        for c in codes:
            self.assertIn(NAMES[c], TERMS, c)
            self.assertIn(NAMES[c], NOT["restrictedSummary"], c)

    def test_drafts_marked(self):
        for d in NOT["documents"].values():
            t = read(d["path"])
            self.assertIn("DRAFT FOR COUNSEL", t)
            self.assertIn("Not legal advice", t)
            self.assertIn("## Not done / not proven", t)
        self.assertIn("by Bloch Inc", NOT["operatorLine"])
        self.assertIn("operated by Bloch Inc (Panama)", NOT["operatorLine"])

    def test_numbers_match_parameters(self):
        m = {x["symbol"]: x for x in MK["markets"]}
        self.assertEqual({x["liquidationFeePpm"] for x in m.values()}, {5000})
        self.assertIn("0.5% of the closed size", RISK)
        self.assertIn("0.5% liquidation fee", " ".join(NOT["riskPoints"]))
        self.assertEqual({x["maxFundingRate"] for x in m.values()}, {500000})
        self.assertIn("0.05% of position value per hour", RISK)
        self.assertIn("0.05% of position value per hour", " ".join(NOT["riskPoints"]))
        self.assertEqual({x["markBandBps"] for x in m.values()}, {50})
        self.assertIn("bounded to 0.5% around the oracle index", RISK)
        self.assertEqual({x["marketSlippageBps"] for x in m.values()}, {500})
        self.assertIn("never beyond 5% from the mark", RISK)
        self.assertEqual(max(x["maxLeverage"] for x in m.values()), 20)
        self.assertIn("At 20x a 5% move", RISK)
        self.assertEqual(RK["global"]["withdrawalFee"], "1")
        self.assertIn("flat fee of 1 bUSDC", RISK)
        self.assertIn(RK["global"]["feeRecipient"], TERMS)
        for x in m.values():
            for t in x["tiers"]:
                self.assertEqual(t["mmfPpm"], -(-t["imfPpm"] // 2))
        self.assertIn("half of the initial", RISK)


if __name__ == "__main__":
    unittest.main()

#!/usr/bin/env python3
"""Builds compliance/legal/notices.json from notices.src.json: adds each document's id (from its first
'Document id:' line) and SHA-256, and the attestation text (the points joined by newlines, prefixed by the document
id) with its SHA-256, so the Interface can pin texts by hash. UNAUDITED prototype tooling; stdlib only.

    python3 -I compliance/tools/build_notices.py [--check]
"""
import hashlib
import json
import os
import re
import sys

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
from keccak import keccak256  # noqa: E402

HERE = os.path.dirname(os.path.abspath(__file__))
COMP = os.path.dirname(HERE)
REPO = os.path.dirname(COMP)
SRC = os.path.join(COMP, "legal", "notices.src.json")
OUT = os.path.join(COMP, "legal", "notices.json")


def doc_id(text):
    m = re.search(r"^Document id: `([A-Z]+-\d+\.\d+-\d{8})`", text, re.M)
    if not m:
        raise SystemExit("document without a 'Document id:' line")
    return m.group(1)


def build():
    with open(SRC, encoding="utf8") as f:
        src = json.load(f)
    docs = {}
    for k, rel in src["documents"].items():
        with open(os.path.join(REPO, rel), "rb") as f:
            raw = f.read()
        docs[k] = {"path": rel, "id": doc_id(raw.decode("utf8")), "sha256": hashlib.sha256(raw).hexdigest()}
    att_id = docs["attestation"]["id"]
    text = att_id + "\n" + "\n".join(src["attestationPoints"])
    out = dict(src)
    out["status"] = src["status"].replace("generated into notices.json", "this file is generated")
    out["documents"] = docs
    out["attestation"] = {
        "documentId": att_id,
        "statementId": att_id.lower(),
        "text": text,
        "statementHash": "0x" + keccak256(text.encode("utf8")).hex(),
        "sha256": hashlib.sha256(text.encode("utf8")).hexdigest(),
        "typedData": "agent 397: compliance/geoblock/src/attestation.mjs (Attestation(address account,string "
                     "statementId,bytes32 statementHash,string residenceCountry,uint64 issuedAt,uint64 expiresAt)); "
                     "statementId and statementHash above are its inputs",
    }
    return json.dumps(out, indent=2, ensure_ascii=False) + "\n"


def main(argv):
    text = build()
    if "--check" in argv:
        with open(OUT, encoding="utf8") as f:
            same = f.read() == text
        print("notices.json is the build output" if same else "notices.json DIFFERS from the build")
        return 0 if same else 1
    with open(OUT, "w", encoding="utf8") as f:
        f.write(text)
    print(f"wrote {OUT}")
    return 0


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))

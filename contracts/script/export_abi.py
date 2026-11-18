#!/usr/bin/env python3
# Copyright (c) 2026 Bloch Inc.
"""Writes (or with --check compares) the committed ABI files `abi/PerpsVault.abi.json` and `abi/MockBUSDC.abi.json`
from the Foundry build output (`forge build` first). PROTOTYPE, UNAUDITED.

The node (431), the frontend (434) and the e2e stack (435) read these files; `--check` exits 1 when a committed
file differs from the build, so a contract change without an ABI update is caught (run it after `forge build`
and before every commit that touches `src/`; `script/e2e_anvil.py` runs it too)."""
import json
import os
import sys

CONTRACTS = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
FILES = [("PerpsVault.sol", "PerpsVault"), ("MockBUSDC.sol", "MockBUSDC")]


def main():
    check = "--check" in sys.argv[1:]
    bad = 0
    os.makedirs(os.path.join(CONTRACTS, "abi"), exist_ok=True)
    for source, name in FILES:
        with open(os.path.join(CONTRACTS, "out", source, name + ".json")) as f:
            text = json.dumps(json.load(f)["abi"], indent=1, sort_keys=True) + "\n"
        path = os.path.join(CONTRACTS, "abi", name + ".abi.json")
        if check:
            old = open(path).read() if os.path.exists(path) else ""
            if old != text:
                sys.stderr.write("export_abi: %s differs from the build\n" % path)
                bad = 1
        else:
            with open(path, "w") as f:
                f.write(text)
    if check and not bad:
        print("export_abi: committed ABI files equal the build")
    sys.exit(bad)


if __name__ == "__main__":
    main()

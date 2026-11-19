#!/usr/bin/env python3
# Copyright (c) 2026 Bloch Inc.
"""Replays the engine-produced vault vectors against the real PerpsVault on a PRIVATE anvil (PROTOTYPE, UNAUDITED).

Round 2, agent 433, task 2 (anvil half; `test/EngineVectors.t.sol` is the forge half). For each story of
`test/vectors/engine/` it starts its own anvil (loopback, chain id 700770, a port of 49700-49799), deploys
MockBUSDC and PerpsVault from anvil's unlocked dev account so that the vault sits at the address the vectors were
signed for, sends every step as a real transaction at the step's L2 timestamp, checks the effect the generator
expects (indexes, deadlines, sequence numbers, payouts, pools, final balances, conservation), checks by `eth_call`
that every step the vault must refuse is refused with the expected error, and records the gas of every transaction.
It stops the anvil it started by PID. No private key is taken or written: senders are anvil's unlocked accounts and
impersonated throwaway addresses.

  python3 script/e2e_anvil.py [--port 49710] [--stories normal,insurance,backstop,shortfall] [--out evidence.json]
"""
import argparse
import json
import os
import signal
import statistics
import subprocess
import sys
import time

HERE = os.path.dirname(os.path.abspath(__file__))
CONTRACTS = os.path.dirname(HERE)
T0 = 1_800_000_000
GAS_LIMIT = "3000000"  # fixed, never the estimate: an exact estimate can be short one block later (F434-1)
CP = "(uint64,uint64,bytes32,bytes32,bytes32,bytes32,uint64,uint64,uint64,uint128,uint128,bytes32,bytes32,bytes32,uint64,uint64)"
WD = "(uint64,address,address,uint64,uint64)"
AC = "(address,uint32,int128,uint64,bytes32)"
ERRORS = ["InvalidCheckpoint()", "InvalidWithdrawal()", "InvalidSurplus()", "InvalidProof()", "AlreadyRegistered()",
          "InvalidCertificate()", "InvalidRefund()", "NotReady()"]
MAX64 = str(2**64 - 1)


def die(message):
    sys.stderr.write("e2e_anvil: FAIL: " + message + "\n")
    raise SystemExit(1)


class Foundry:
    def __init__(self, directory, rpc):
        self.dir = directory
        self.rpc = rpc
        self.env = dict(os.environ)
        lib = os.path.join(os.path.dirname(directory.rstrip("/")), "libusb", "lib")
        self.prefix = ["nice", "-n", "10", "/usr/bin/env"]
        if os.path.isdir(lib):
            self.prefix.append("DYLD_LIBRARY_PATH=" + lib)

    def path(self, tool):
        return os.path.join(self.dir, tool) if self.dir else tool

    def cast(self, *args, ok=True):
        done = subprocess.run(self.prefix + [self.path("cast")] + list(args), stdout=subprocess.PIPE,
                              stderr=subprocess.PIPE, cwd=CONTRACTS)
        out, err = done.stdout.decode().strip(), done.stderr.decode().strip()
        if ok and done.returncode != 0:
            die("cast %s: %s" % (" ".join(args[:4]), err[-500:]))
        return out if done.returncode == 0 else None, err

    def rpc_call(self, method, *params):
        return self.cast("rpc", "--rpc-url", self.rpc, method, *params)[0]

    def call(self, to, signature, *args):
        return self.cast("call", "--rpc-url", self.rpc, to, signature, *args)[0]

    def num(self, to, signature, *args):
        return int(self.call(to, signature, *args).split()[0], 0)

    def refused(self, to, sender, signature, *args):
        """eth_call must revert; returns the error name it reverted with."""
        out, err = self.cast("call", "--rpc-url", self.rpc, "--from", sender, to, signature, *args, ok=False)
        if out is not None:
            die("%s was expected to be refused and is accepted" % signature.split("(")[0])
        for name in ERRORS:
            selector = self.cast("sig", name)[0]
            if name.split("(")[0] in err or selector in err:
                return name
        die("unrecognised refusal: " + err[-300:])

    def send(self, timestamp, sender, to, signature, *args, create=None):
        self.rpc_call("evm_setNextBlockTimestamp", str(timestamp))
        base = ["send", "--rpc-url", self.rpc, "--unlocked", "--from", sender, "--gas-limit", GAS_LIMIT if not create
                else "8000000", "--json"]
        out, _ = self.cast(*(base + (["--create", create] if create else [to, signature] + list(args))))
        receipt = json.loads(out)
        if int(receipt["status"], 16) != 1:
            die("transaction reverted: %s" % signature)
        block = json.loads(self.cast("block", "--rpc-url", self.rpc, receipt["blockNumber"], "--json")[0])
        if int(block["timestamp"], 16) != timestamp:
            die("block timestamp %d, wanted %d" % (int(block["timestamp"], 16), timestamp))
        return receipt


def tup(values):
    return "(" + ",".join(str(v) for v in values) + ")"


def arr(values):
    return "[" + ",".join(values) + "]"


def checkpoint_tuple(cp):
    keys = ["height", "time", "logHash", "accountsRoot", "marketsRoot", "withdrawalsRoot", "nextDepositIndex",
            "nextForcedIndex", "nextOrderId", "totalEscape", "dust", "auxHash", "depositsHead", "forcedHead",
            "withdrawalCount", "withdrawalsTotal"]
    return tup(cp[k] for k in keys)


def account_tuple(a):
    return tup([a["account"], a["index"], a["balance"], a["escapeValue"], a["positionsHash"]])


def run_story(name, port, foundry_dir, gas):
    with open(os.path.join(CONTRACTS, "test", "vectors", "engine", name + ".json")) as f:
        v = json.load(f)
    rpc = "http://127.0.0.1:%d" % port
    f = Foundry(foundry_dir, rpc)
    log = open(os.devnull, "w")
    anvil = subprocess.Popen(f.prefix + [f.path("anvil"), "--host", "127.0.0.1", "--port", str(port), "--chain-id",
                                         str(v["chainId"]), "--timestamp", str(T0 - 2000), "--silent"],
                             stdout=log, stderr=log)
    try:
        for _ in range(100):
            if f.cast("chain-id", "--rpc-url", rpc, ok=False)[0] is not None:
                break
            time.sleep(0.2)
        else:
            die("anvil did not start on port %d" % port)
        if f.rpc_call("anvil_nodeInfo") is None:
            die("not an anvil")
        return replay(v, f, gas)
    finally:
        anvil.send_signal(signal.SIGTERM)
        try:
            anvil.wait(timeout=10)
        except subprocess.TimeoutExpired:
            anvil.kill()


def replay(v, f, gas):
    accounts = json.loads(f.rpc_call("eth_accounts"))
    deployer, relayer = accounts[0], accounts[1]
    vault, fee = v["vault"], v["feeRecipient"]

    def artifact(source, name):
        with open(os.path.join(CONTRACTS, "out", source, name + ".json")) as fh:
            return json.load(fh)["bytecode"]["object"]

    clock = [T0 - 900]

    def prep(sender, to, signature, *args):
        clock[0] += 1
        return f.send(clock[0], sender, to, signature, *args)

    # token (deployer nonce 0), then every preparation that does not need the vault to exist
    clock[0] += 1
    token = f.send(clock[0], deployer, None, "create token", create=artifact("MockBUSDC.sol", "MockBUSDC"))["contractAddress"]
    senders = {}
    for s in v["steps"]:
        if s["kind"] == "deposit":
            senders[s["from"]] = senders.get(s["from"], 0) + int(s["amount"])
        elif s["kind"] == "forced":
            senders.setdefault(s["from"], 0)
    for who, total in senders.items():
        f.rpc_call("anvil_impersonateAccount", who)
        f.rpc_call("anvil_setBalance", who, hex(10**18))
        if total:
            # the faucet mints at most 10M per call; the stories stay far below
            prep(relayer, token, "mint(address,uint256)", who, str(total))
            prep(who, token, "approve(address,uint256)", vault, str(total))
    limits = v["limits"]
    config = "(%s,%s,%s,%s,%s,%s,%s,%s,%s,%s,%s,%s,%s,%s,%s,%s)" % (
        token, fee, arr(v["validators"]), limits["threshold"], limits["unpauseThreshold"], limits["cap"],
        limits["minDeposit"], limits["windowLimit"], limits["windowSeconds"], limits["largeThreshold"],
        limits["largeDelay"], limits["forcedDelay"], limits["escapeDelay"], arr(v["guardians"]),
        limits["pauseDuration"], limits["pauseCooldown"])
    encoded = f.cast("abi-encode", "constructor((address,address,address[],uint8,uint8,uint64,uint64,uint64,uint64,"
                     "uint64,uint64,uint64,uint64,address[],uint64,uint64))", config)[0]
    receipt = f.send(T0, deployer, None, "create vault", create=artifact("PerpsVault.sol", "PerpsVault") + encoded[2:])
    if receipt["contractAddress"].lower() != vault.lower():
        die("vault deployed at %s, the vectors were signed for %s" % (receipt["contractAddress"], vault))
    gas.setdefault("deployVault", []).append(int(receipt["gasUsed"], 16))

    def balance(who):
        return f.num(token, "balanceOf(address)(uint256)", who)

    def expect(got, want, what, step):
        if int(got) != int(want):
            die("step %s (%s): %s = %s, expected %s" % (step["n"], step["kind"], what, got, want))

    def empty_block(t):
        f.rpc_call("evm_setNextBlockTimestamp", str(t))
        f.rpc_call("evm_mine")

    checks = 0
    refusals = 0
    for s in v["steps"]:
        t, kind = int(s["l2Time"]), s["kind"]
        if kind == "deposit":
            r = f.send(t, s["from"], vault, "deposit(uint64,address)", s["amount"], s["account"])
            expect(f.num(vault, "depositCount()(uint64)") - 1, s["expectIndex"], "deposit index", s)
        elif kind == "forced":
            r = f.send(t, s["from"], vault, "requestForced(uint8,uint64)", str(s["forcedKind"]), s["arg"])
            expect(f.num(vault, "forcedDeadline(uint64)(uint64)", str(s["expectIndex"])), s["expectDeadline"],
                   "forced deadline", s)
        elif kind == "checkpoint":
            cp = checkpoint_tuple(s["cp"])
            sigs = arr(s["signatures"])
            sig = "submitCheckpoint(%s,bytes[])" % CP
            expect(int(f.call(vault, "checkpointDigest(%s)(bytes32)" % CP, cp), 16), int(s["digest"], 16),
                   "checkpoint digest", s)
            expect(int(f.call(vault, "stateRootOf(%s)(bytes32)" % CP, cp), 16), int(s["cp"]["stateRoot"], 16),
                   "state root", s)
            if s["expect"] != "accepted":
                empty_block(t)
                if f.refused(vault, relayer, sig, cp, sigs) != s["expect"] + "()":
                    die("step %s: wrong refusal" % s["n"])
                refusals += 1
                checks += 3
                continue
            r = f.send(t, relayer, vault, sig, cp, sigs)
            expect(f.num(vault, "checkpointCount()(uint64)"), s["expectSeq"], "checkpoint seq", s)
            if f.refused(vault, relayer, sig, cp, sigs) != "InvalidCheckpoint()":
                die("step %s: a checkpoint was accepted twice" % s["n"])
            refusals += 1
        elif kind == "withdraw":
            w = s["w"]
            args = [str(s["seq"]), tup([w["withdrawalId"], w["account"], w["recipient"], w["amount"], w["height"]]),
                    arr(s["siblings"]), MAX64]
            sig = "withdraw(uint64,%s,bytes32[],uint64)" % WD
            before = balance(s["expectRecipient"])
            r = f.send(t, relayer, vault, sig, *args)
            expect(balance(s["expectRecipient"]) - before, s["expectPaid"], "withdrawal paid", s)
            if f.refused(vault, relayer, sig, *args) != "AlreadyRegistered()":
                die("step %s: a record registered twice" % s["n"])
            refusals += 1
        elif kind == "warp":
            empty_block(t)
            continue
        elif kind == "triggerEscape":
            r = f.send(t, relayer, vault, "triggerEscape()")
            expect(f.num(vault, "escapePool()(uint64)"), s["expectPool"], "escape pool", s)
        elif kind == "proveSystemShares":
            sig = "proveSystemShares(%s,bytes32[],%s,bytes32[])" % (AC, AC)
            r = f.send(t, relayer, vault, sig, account_tuple(s["insurance"]["a"]), arr(s["insurance"]["siblings"]),
                       account_tuple(s["backstop"]["a"]), arr(s["backstop"]["siblings"]))
            expect(f.num(vault, "escapePayable()(uint64)"), s["expectPayable"], "escape payable", s)
            expect(f.num(vault, "subordinatedTotal()(uint128)"), s["expectSubordinated"], "subordinated", s)
        elif kind == "claimEscape":
            sig = "claimEscape(%s,bytes32[],uint64)" % AC
            args = [account_tuple(s["proof"]["a"]), arr(s["proof"]["siblings"]), MAX64]
            if s["expect"] != "registered":
                empty_block(t)
                if f.refused(vault, relayer, sig, *args) != s["expect"] + "()":
                    die("step %s: wrong refusal" % s["n"])
                refusals += 1
                checks += 1
                continue
            before = balance(s["expectRecipient"])
            r = f.send(t, relayer, vault, sig, *args)
            expect(balance(s["expectRecipient"]) - before, s["expectPaidNow"], "escape paid now", s)
            if f.refused(vault, relayer, sig, *args) != "AlreadyRegistered()":
                die("step %s: an escape value claimed twice" % s["n"])
            refusals += 1
        elif kind == "releaseEscape":
            key = f.call(vault, "escapeKey(address)(bytes32)", s["account"])
            before = balance(s["expectRecipient"])
            r = f.send(t, relayer, vault, "release(bytes32,uint64)", key, MAX64)
            expect(balance(s["expectRecipient"]) - before, s["expectPaid"], "delayed escape payout", s)
        elif kind == "refund":
            before = balance(s["expectRecipient"])
            args = [str(s["index"]), s["account"], s["amount"]]
            r = f.send(t, relayer, vault, "refundDeposit(uint64,address,uint64)", *args)
            expect(balance(s["expectRecipient"]) - before, s["amount"], "refund", s)
            if f.refused(vault, relayer, "refundDeposit(uint64,address,uint64)", *args) != "InvalidRefund()":
                die("step %s: a deposit refunded twice" % s["n"])
            refusals += 1
        elif kind == "releaseSystem":
            key = f.call(vault, "escapeKey(address)(bytes32)", "0x0000000000000000000000000000000000000002")
            before = balance(fee)
            r = f.send(t, relayer, vault, "release(bytes32,uint64)", key, MAX64)
            expect(balance(fee) - before, s["expectPaid"], "system remainder", s)
        elif kind == "sweep":
            if s["expect"] == "swept":
                before = balance(fee)
                r = f.send(t, relayer, vault, "sweepEscapeSurplus()")
                expect(balance(fee) - before, s["expectAmount"], "surplus", s)
            else:
                empty_block(t)
                if f.refused(vault, relayer, "sweepEscapeSurplus()") != "InvalidSurplus()":
                    die("step %s: wrong refusal" % s["n"])
                refusals += 1
                checks += 1
                continue
        else:
            die("unknown step kind " + kind)
        gas.setdefault(kind, []).append(int(r["gasUsed"], 16))
        checks += 2

    e = v["expect"]
    final = {"vaultBalance": balance(vault), "depositedTotal": f.num(vault, "depositedTotal()(uint64)"),
             "withdrawnTotal": f.num(vault, "withdrawnTotal()(uint64)"),
             "escapeClaimedTotal": f.num(vault, "escapeClaimedTotal()(uint64)"),
             "refundedTotal": f.num(vault, "refundedTotal()(uint64)"), "reserved": f.num(vault, "reserved()(uint64)")}
    for key in ("vaultBalance", "depositedTotal", "withdrawnTotal", "escapeClaimedTotal", "refundedTotal"):
        if final[key] != int(e[key]):
            die("final %s = %s, expected %s" % (key, final[key], e[key]))
    if final["reserved"] != 0:
        die("something is still reserved at the end")
    received = 0
    for item in e["received"]:
        got = balance(item["account"])
        if got != int(item["received"]):
            die("final balance of %s = %s, expected %s" % (item["account"], got, item["received"]))
        received += got
    if received + final["vaultBalance"] != final["depositedTotal"]:
        die("conservation: received %d + vault %d != deposited %d" % (received, final["vaultBalance"], final["depositedTotal"]))
    return {"story": v["story"], "steps": v["stepCount"], "checks": checks + 6 + len(e["received"]),
            "refusalsChecked": refusals, "vault": vault, "token": token, "final": final,
            "engineFinalStateRoot": v["engine"]["finalStateRoot"], "result": "PASS"}


def main():
    p = argparse.ArgumentParser()
    p.add_argument("--port", type=int, default=49710)
    p.add_argument("--stories", default="normal,insurance,backstop,shortfall")
    p.add_argument("--out", default="")
    p.add_argument("--foundry-dir", default=os.environ.get(
        "FOUNDRY_DIR", os.path.expanduser("~/.local/share/dex-tools/foundry")))
    a = p.parse_args()
    if not 49700 <= a.port <= 49799:
        die("port outside agent 433's range 49700-49799")
    foundry_dir = a.foundry_dir if os.path.isdir(a.foundry_dir) else ""
    check = subprocess.run([sys.executable, os.path.join(HERE, "export_abi.py"), "--check"])
    if check.returncode != 0:
        die("the committed ABI differs from the build (run forge build, then script/export_abi.py)")
    gas, results = {}, []
    for story in a.stories.split(","):
        result = run_story(story, a.port, foundry_dir, gas)
        results.append(result)
        print("%-10s PASS  %d steps, %d checks, %d refusals checked" % (story, result["steps"], result["checks"],
                                                                         result["refusalsChecked"]))
    table = {k: {"n": len(x), "min": min(x), "median": int(statistics.median(x)), "max": max(x)} for k, x in
             sorted(gas.items())}
    evidence = {"schema": "postern-perps/vault-anvil-e2e/1", "status": "PROTOTYPE, UNAUDITED; private anvil; throwaway keys",
                "anvil": "127.0.0.1:%d, chain id 700770, one fresh instance per story" % a.port,
                "gasLimitPerCall": int(GAS_LIMIT), "results": results, "gasUsed": table}
    print(json.dumps(table, indent=1))
    if a.out:
        with open(a.out, "w") as fh:
            json.dump(evidence, fh, indent=1)
            fh.write("\n")
    print("E2E-ANVIL OK %d stories" % len(results))


if __name__ == "__main__":
    main()

#!/usr/bin/env python3
# Copyright (c) 2026 Bloch Inc.
"""Local deployment of MockBUSDC + PerpsVault on a PRIVATE anvil (PROTOTYPE, UNAUDITED).

For the local full stack only (BRIEF-PERPS-ROUND2, agent 433 task 1). It can not deploy to a real network:

  * the RPC must be a loopback URL and must answer `anvil_nodeInfo` (only anvil does);
  * it never reads or takes a private key: transactions are sent from anvil's own unlocked dev account
    (`eth_accounts[0]`, `eth_sendTransaction`);
  * the committee is either the addresses you pass (`--validators`, the node's throwaway committee) or seven
    throwaway keys generated at run time by `cast wallet new` and written, mode 0600, to the output directory,
    which must be ignored by git (or outside any repository).

Usage (from `contracts/`; python3 standard library only; needs forge and cast):

  python3 script/local_deploy.py --rpc http://127.0.0.1:49700 --out deploy/local \
      [--validators 0xA,0xB,...(7)] [--guardians 0xG1,0xG2,0xG3] [--chain-id 700770] \
      [--fund 0xUSER1,0xUSER2] [--fund-amount 1000000000000] [--set KEY=VALUE ...] [--foundry-dir DIR]

Writes to `--out`: `deployment.json` (addresses, chain id, committee, limits, domain separator, chain heads, deploy
block), `PerpsVault.abi.json`, `MockBUSDC.abi.json`, and, only if it generated keys, `throwaway-keys.json` (0600).
Prints `deployment.json` on stdout (last line: `LOCAL-DEPLOY OK <vault> <token>`); exits non-zero on any refusal.

`--set` overrides one limit of the proposed launch configuration (`deploy/vault.config.json`), e.g.
`--set forcedDelay=86400`. The constructor still enforces every bound of VAULT.md section 3.
"""
import argparse
import json
import os
import re
import stat
import subprocess
import sys
import urllib.parse

HERE = os.path.dirname(os.path.abspath(__file__))
CONTRACTS = os.path.dirname(HERE)
OWNER_FEE_ADDRESS = "0x775a993F09Bd80aeCF8F8c2ADe0C84158E3a0544"
LIMIT_KEYS = [
    "threshold", "unpauseThreshold", "cap", "minDeposit", "windowLimit", "windowSeconds", "largeThreshold",
    "largeDelay", "forcedDelay", "escapeDelay", "pauseDuration", "pauseCooldown",
]
CONFIG_TUPLE = (
    "(address,address,address[],uint8,uint8,uint64,uint64,uint64,uint64,uint64,uint64,uint64,uint64,address[],"
    "uint64,uint64)"
)
ADDRESS = re.compile(r"^0x[0-9a-fA-F]{40}$")


def die(message):
    sys.stderr.write("local_deploy: REFUSED: " + message + "\n")
    sys.exit(1)


class Tools:
    def __init__(self, foundry_dir):
        self.dir = foundry_dir
        self.env = dict(os.environ)
        lib = os.path.join(os.path.dirname(foundry_dir.rstrip("/")), "libusb", "lib")
        if os.path.isdir(lib):
            self.env["DYLD_LIBRARY_PATH"] = lib

    def run(self, tool, *args, cwd=None, quiet=False):
        path = os.path.join(self.dir, tool) if self.dir else tool
        # `env` re-exports DYLD_LIBRARY_PATH after `nice` (macOS strips it from protected binaries).
        command = ["nice", "-n", "10", "/usr/bin/env"]
        if "DYLD_LIBRARY_PATH" in self.env:
            command.append("DYLD_LIBRARY_PATH=" + self.env["DYLD_LIBRARY_PATH"])
        command += [path] + list(args)
        done = subprocess.run(command, cwd=cwd, env=self.env, stdout=subprocess.PIPE, stderr=subprocess.PIPE)
        if done.returncode != 0:
            if quiet:
                return None
            die("%s %s failed: %s" % (tool, " ".join(args[:3]), done.stderr.decode(errors="replace").strip()[-600:]))
        return done.stdout.decode().strip()


def parse_addresses(text, what, count):
    items = [a.strip() for a in text.split(",") if a.strip()]
    for a in items:
        if not ADDRESS.match(a):
            die("%s: not an address: %s" % (what, a))
    if len(items) != count:
        die("%s: expected %d addresses, got %d" % (what, count, len(items)))
    if len({a.lower() for a in items}) != len(items):
        die("%s: duplicate address" % what)
    return sorted(items, key=lambda a: int(a, 16))


def git_ignored_or_outside(path, tools_env):
    """True when `path` is outside every git work tree, or ignored by the one it is in."""
    probe = path
    while not os.path.isdir(probe):
        probe = os.path.dirname(probe)
    inside = subprocess.run(["git", "-C", probe, "rev-parse", "--is-inside-work-tree"], stdout=subprocess.PIPE,
                            stderr=subprocess.PIPE)
    if inside.returncode != 0 or inside.stdout.decode().strip() != "true":
        return True
    ignored = subprocess.run(["git", "-C", probe, "check-ignore", "-q", os.path.join(path, "throwaway-keys.json")])
    return ignored.returncode == 0


def main():
    p = argparse.ArgumentParser(description="PerpsVault local (anvil only) deployment; PROTOTYPE, UNAUDITED")
    p.add_argument("--rpc", default="http://127.0.0.1:49700")
    p.add_argument("--out", default=os.path.join(CONTRACTS, "deploy", "local"))
    p.add_argument("--chain-id", type=int, default=700770)
    p.add_argument("--validators", default="")
    p.add_argument("--guardians", default="")
    p.add_argument("--fund", default="")
    p.add_argument("--fund-amount", type=int, default=1_000_000 * 10**6)
    p.add_argument("--set", action="append", default=[], metavar="KEY=VALUE")
    p.add_argument("--foundry-dir", default=os.environ.get(
        "FOUNDRY_DIR", os.path.expanduser("~/.local/share/dex-tools/foundry")))
    p.add_argument("--skip-build", action="store_true")
    a = p.parse_args()

    url = urllib.parse.urlparse(a.rpc)
    if url.scheme != "http" or url.hostname not in ("127.0.0.1", "localhost", "::1"):
        die("the RPC must be a loopback http URL (got %s): this tool deploys to a private anvil only" % a.rpc)
    t = Tools(a.foundry_dir if os.path.isdir(a.foundry_dir) else "")
    cast = lambda *args, **kw: t.run("cast", *args, **kw)  # noqa: E731

    if cast("rpc", "--rpc-url", a.rpc, "anvil_nodeInfo", quiet=True) is None:
        die("the RPC does not answer anvil_nodeInfo: not an anvil, nothing sent")
    chain_id = int(cast("chain-id", "--rpc-url", a.rpc))
    if chain_id != a.chain_id:
        die("RPC chain id %d differs from --chain-id %d" % (chain_id, a.chain_id))

    out = os.path.abspath(a.out)
    generate = not a.validators or not a.guardians
    if generate and not git_ignored_or_outside(out, t.env):
        die("--out %s is inside a git work tree and not ignored: throwaway keys are never written there" % out)
    os.makedirs(out, exist_ok=True)

    # Limits: the proposed launch configuration, with explicit overrides.
    with open(os.path.join(CONTRACTS, "deploy", "vault.config.json")) as f:
        proposal = json.load(f)
    limits = {k: int(proposal[k]) for k in LIMIT_KEYS}
    for item in a.set:
        key, _, value = item.partition("=")
        if key not in LIMIT_KEYS or not value.isdigit():
            die("--set %s: KEY must be one of %s and VALUE a decimal integer" % (item, ", ".join(LIMIT_KEYS)))
        limits[key] = int(value)

    generated = {}

    def fresh(count, role):
        made = []
        for _ in range(count):
            w = json.loads(cast("wallet", "new", "--json"))[0]
            made.append(w)
        made.sort(key=lambda w: int(w["address"], 16))
        generated[role] = made
        return [w["address"] for w in made]

    validators = parse_addresses(a.validators, "--validators", 7) if a.validators else fresh(7, "validators")
    guardians = parse_addresses(a.guardians, "--guardians", 3) if a.guardians else fresh(3, "guardians")
    if {x.lower() for x in validators} & {x.lower() for x in guardians}:
        die("a guardian is also a validator")

    accounts = json.loads(cast("rpc", "--rpc-url", a.rpc, "eth_accounts"))
    if not accounts:
        die("anvil exposes no unlocked account")
    deployer = accounts[0]

    if not a.skip_build:
        t.run("forge", "build", "--offline", cwd=CONTRACTS)

    def artifact(source, name):
        with open(os.path.join(CONTRACTS, "out", source, name + ".json")) as f:
            return json.load(f)

    def create(source, name, encoded_args=""):
        art = artifact(source, name)
        code = art["bytecode"]["object"] + (encoded_args[2:] if encoded_args else "")
        receipt = json.loads(cast("send", "--rpc-url", a.rpc, "--unlocked", "--from", deployer, "--json", "--create",
                                  code))
        if int(receipt["status"], 16) != 1 or not receipt.get("contractAddress"):
            die("creation of %s failed" % name)
        address = cast("to-check-sum-address", receipt["contractAddress"])
        return address, int(receipt["gasUsed"], 16), int(receipt["blockNumber"], 16), art["abi"]

    token, token_gas, _, token_abi = create("MockBUSDC.sol", "MockBUSDC")
    config = "(%s,%s,[%s],%d,%d,%d,%d,%d,%d,%d,%d,%d,%d,[%s],%d,%d)" % (
        token, OWNER_FEE_ADDRESS, ",".join(validators), limits["threshold"], limits["unpauseThreshold"],
        limits["cap"], limits["minDeposit"], limits["windowLimit"], limits["windowSeconds"],
        limits["largeThreshold"], limits["largeDelay"], limits["forcedDelay"], limits["escapeDelay"],
        ",".join(guardians), limits["pauseDuration"], limits["pauseCooldown"])
    encoded = cast("abi-encode", "constructor(%s)" % CONFIG_TUPLE, config)
    vault, vault_gas, vault_block, vault_abi = create("PerpsVault.sol", "PerpsVault", encoded)

    def call(signature, *args):
        return cast("call", "--rpc-url", a.rpc, vault, signature, *args)

    def num(text):
        return int(text.split()[0], 0)

    # Post gate: the deployed vault equals what was asked (same checks as DeployPerpsVault.s.sol `_check`).
    if call("token()(address)").lower() != token.lower() or call("feeRecipient()(address)").lower() != \
            OWNER_FEE_ADDRESS.lower():
        die("post gate: token / fee recipient")
    for key in LIMIT_KEYS:
        if num(call("%s()(uint64)" % key)) != limits[key]:
            die("post gate: %s" % key)
    if num(call("validatorCount()(uint256)")) != 7 or num(call("guardianCount()(uint256)")) != 3:
        die("post gate: set sizes")
    for i, v in enumerate(validators):
        if call("validators(uint256)(address)", str(i)).lower() != v.lower():
            die("post gate: validator %d" % i)
    for i, g in enumerate(guardians):
        if call("guardians(uint256)(address)", str(i)).lower() != g.lower():
            die("post gate: guardian %d" % i)
    if num(call("checkpointCount()(uint64)")) != 0 or num(call("depositCount()(uint64)")) != 0:
        die("post gate: fresh vault expected")

    funded = []
    if a.fund:
        for user in [u.strip() for u in a.fund.split(",") if u.strip()]:
            if not ADDRESS.match(user):
                die("--fund: not an address: %s" % user)
            cast("send", "--rpc-url", a.rpc, "--unlocked", "--from", deployer, token, "mint(address,uint256)", user,
                 str(a.fund_amount))
            funded.append({"account": user, "amount": str(a.fund_amount)})

    deployment = {
        "schema": "postern-perps/local-deployment/1",
        "status": "PROTOTYPE, UNAUDITED: private anvil only; throwaway committee; mock token without value",
        "rpc": a.rpc,
        "chainId": chain_id,
        "token": token,
        "tokenDecimals": 6,
        "tokenFaucet": "mint(address to, uint256 value): open to anyone, at most 10,000,000 bUSDC per call",
        "vault": vault,
        "vaultDeployBlock": vault_block,
        "deployer": deployer,
        "feeRecipient": OWNER_FEE_ADDRESS,
        "validators": validators,
        "guardians": guardians,
        "limits": {k: str(v) for k, v in limits.items()},
        "eip712Domain": {"name": "PosternPerpsVault", "version": "1", "chainId": chain_id,
                         "verifyingContract": vault},
        "domainSeparator": call("domainSeparator()(bytes32)"),
        "depositHead0": call("depositHead(uint64)(bytes32)", "0"),
        "forcedHead0": call("forcedHead(uint64)(bytes32)", "0"),
        "gasUsed": {"token": token_gas, "vault": vault_gas},
        "abi": {"vault": "PerpsVault.abi.json", "token": "MockBUSDC.abi.json"},
        "funded": funded,
        "committeeKeys": "throwaway-keys.json" if generated else "not generated here (addresses were passed in)",
    }
    with open(os.path.join(out, "PerpsVault.abi.json"), "w") as f:
        json.dump(vault_abi, f, indent=1, sort_keys=True)
        f.write("\n")
    with open(os.path.join(out, "MockBUSDC.abi.json"), "w") as f:
        json.dump(token_abi, f, indent=1, sort_keys=True)
        f.write("\n")
    if generated:
        path = os.path.join(out, "throwaway-keys.json")
        fd = os.open(path, os.O_WRONLY | os.O_CREAT | os.O_TRUNC, stat.S_IRUSR | stat.S_IWUSR)
        with os.fdopen(fd, "w") as f:
            json.dump({"note": "THROWAWAY keys generated at run time for a private anvil; never reuse, never commit",
                       "vault": vault, **generated}, f, indent=1)
            f.write("\n")
        os.chmod(path, stat.S_IRUSR | stat.S_IWUSR)
    with open(os.path.join(out, "deployment.json"), "w") as f:
        json.dump(deployment, f, indent=1)
        f.write("\n")
    print(json.dumps(deployment, indent=1))
    print("LOCAL-DEPLOY OK %s %s" % (vault, token))


if __name__ == "__main__":
    main()

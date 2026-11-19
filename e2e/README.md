# `e2e/`: the local full stack and its scripted rehearsal (UNAUDITED prototype)

Everything here runs LOCAL processes on loopback with THROWAWAY keys derived from public labels or generated at run
time. Nothing is deployed, nothing touches a real network, no real key or fund is involved. Owner: agent 435
(round 2). Copyright (c) 2026 Bloch Inc. All rights reserved.

## What runs

| Piece | Where | What it is |
|---|---|---|
| L2 | build host, `anvil` chain id 700770, 1 s blocks, `finalized` 8 blocks behind `latest` | the stand-in for the Bloch L2 (the only stand-in left) |
| second L2 endpoint | build host, `host/rpc-proxy.py` | a pass-through JSON-RPC proxy in front of the same anvil; the rehearsal makes it lie (amount, missing log, block hash) |
| token + vault | deployed by `contracts/deploy/local.sh` (433) | mock bUSDC (6 decimals) and the UNCHANGED `PerpsVault`, committee = the node's dev committee 5 of 7, fee recipient `0x775a…0544`, production-shape delays (forced 2 d, escape 7 d, large 1 d: the rehearsal moves anvil's clock) |
| sequencer | build host, `postern-perps-node dev` (431), release build | engine signing domain bound to the deployed vault; watcher on the two L2 endpoints |
| validators | build host; `--local-validators N` keeps N inside the sequencer process, the others are `postern-perps-node validator --index i` processes | 5 of 7 sign every checkpoint |
| oracle | build host, `perps-oracle replay-feed` (432) through the node's authenticated ingest | a dev oracle committee of fresh throwaway keys and an ingest token, both generated on the host at run time; the price path is a tape the rehearsal replaces |
| frontend | Mac, `frontend/scripts/dev-local.mjs` (434), optional | the trading page against this stack |
| tunnel | Mac, one `ssh -N -L` | API 49900, L2 49901, proxy 49902 |

Ports: build host 53500 (API), 53510 (anvil), 53511 (proxy); Mac 49900-49902, 49920 (frontend).
Rust is never built on the Mac: `build.sh` syncs a tree to the build host and builds there through the shared
build-slot limiter.

The build host address and the ssh identity path are operator facts kept in the git-ignored `e2e/local.env`
(`local.env.example` shows the two variables).

## Commands

```
bash e2e/integrate.sh                      # local integration worktree = main + every round-2 branch (never pushed)
bash e2e/build.sh int HEAD ~/worktrees/perps-e2e-r2-int-20261118
bash e2e/up.sh --label int --local-validators 4 [--frontend]   # 4 validators in the sequencer process, 3 separate
node e2e/rehearsal/run.mjs                 # every phase; --only A,B,C or --skip K; evidence JSON
bash e2e/down.sh                           # stops everything by PID
bash e2e/hostctl.sh test-all.sh int        # whole Rust test set of a built tree (on the build host)
bash e2e/test-mac.sh                       # the Mac half: e2e, mock, compliance, geoblock, forge, frontend
```

`up.sh` options: `--label L` (a tree built by `build.sh`), `--tree DIR` (the Mac checkout of the same tree),
`--l2 anvil|fake`, `--oracle replay|dev`, `--local-validators N`, `--funding-ms MS` (dev genesis funding interval,
default 60000; production is 3600000 and `--funding-ms 0` keeps it), `--frontend`, `--keep` (restart on the existing
data). The description of the running stack is `e2e/.run/stack.json`.

Processes are controlled by PID files only (`host/svc.sh`: a PID is signalled only while its environment still
carries the marker the script gave it; nothing is ever killed by name or pattern).

## The rehearsal

`rehearsal/run.mjs` runs numbered checks in phases. A check ends as `pass` (the property was observed; the numbers
are in its evidence), `fail` (not observed; exit 1) or `blocked` (could not run, with the written reason; never
counted as passed). The evidence file holds every check and the stack description.

| Phase | What is shown |
|---|---|
| A stack | dev mode declared unaudited; signing domain bound to the deployed vault; the vault holds the node's committee, the owner's fee recipient, production-shape delays; two L2 endpoints agree; live committee index on every market |
| B deposit | approve + deposit on the L2; NOT credited before L2 finality; credited exactly after; custody = deposits; with one endpoint lying three ways (amount, hidden log, block hash) nothing is credited until both agree, then exactly once |
| C trade | resting quotes, a taker fill at the maker price, taker fee `ceil(notional x 4.5 bps)` and maker fee `ceil(notional x 1.5 bps)` to the fee account to the unit, positions, fills and public trade, four engine refusals, agent wallet limits, I-2 with every position flat |
| D funding | three intervals at the interest rate, each payment `size x mark x rate` exact (payer up, receiver down), balances moved by exactly the sum; a book quoted above the index settles at the cap, never above; I-2 again |
| E liquidation | scripted index falls: liquidation through the book (public trade flagged, balance exact, fee to insurance); by ADL with no book and an unfunded backstop (outside the tape, no fee, never beyond bankruptcy, counterparty flat); through the backstop after seeding insurance and backstop through the vault (backstop holds the position, sizes sum to zero) |
| F finality | checkpoint digest and state root recomputed independently; 5 distinct committee signatures; INDEPENDENT REPLAY by the engine CLI of the published log, every state root and log hash equal; the vault accepts the checkpoint and computes the same root; refuses a changed field, 4 signatures, a replay |
| G withdraw | signed withdrawal: gross debited, flat fee to the fee account, record for the net; certified; proof submitted by a stranger, vault pays the recipient exactly once; four attacks refused; a large withdrawal waits the large-amount delay |
| H forced | a user who sends nothing to the API: forced CancelAll, forced CloseAll, forced Withdraw of everything posted on the L2, served by the engine, acknowledged in checkpoints, paid by the vault |
| I validators | signature by a stranger refused; a committee member signing another state root refused and never published; 3 of 7 stopped: blocks continue, nothing becomes final, finality returns with the fifth; each restarted validator signs again (finality with exactly 5 running when it is one of them); sequencer stopped cleanly and by SIGKILL: same chain after restart, finality resumes |
| J geoblock | opening order from Brazil 451, exit actions still admitted, unknown country denied |
| L conformance | the whole shared API conformance suite against this stack with two funded accounts, nothing skipped |
| K escape (last, one way) | every account's escape leaf fetched and verified for the vault's last checkpoint; sequencer and feeder stopped; the same proofs served by a validator's own read service WITHOUT the sequencer, verified against the vault's root and used for the claims; a late deposit; escape refused early, triggered after `escapeDelay`; vault takes nothing more; claims wait for the system shares; users are senior and paid in full; fee share only to the owner's address; no claim twice; late deposit refunded; books close to the unit; system share locked |

**Variant of phase K (R436-19): `REHEARSAL_ESCAPE=forced node e2e/rehearsal/run.mjs`.** Instead of going silent the
exchange keeps running but never receives a user's forced withdrawal (one L2 endpoint hides the log, so the
two-endpoint watchers of the sequencer and of the validators make no progress); the request passes its deadline, the
vault refuses the next checkpoint, anyone triggers escape with reason 2, and every account claims with proofs served
by a validator. What it does not show: a sequencer that sees the request and ignores it while HONEST validators see
it too (they would have to refuse to sign; the node has no fault switch for that). First run (phases A, B, C, K on
the code of `main` 1a44d73): 31 of 31 checks, books closed to the unit. Still not rehearsed: a shortfall escape
(insurance first, then pro rata), the fee sweep at the UTC day boundary, an exit from a halted market at the exit band.

Arithmetic and key derivations of the rehearsal have their own tests (`e2e/test/`), as does the proxy.

## Not done / not proven

- The L2 is anvil, not the Bloch L2: no proof-based finality, no real bUSDC bytecode, gas not measured there.
- The two L2 endpoints read ONE chain: their agreement proves nothing by itself; only the lying-endpoint checks do.
- Escape without the sequencer is rehearsed with ONE validator's read service still running (K-02b); nothing
  obliges a validator to keep serving its log, and a claimant with no source at all is not rehearsed (R436-11).
- A shortfall escape (an account under water at the last checkpoint: insurance first, then pro rata) is covered by
  the vault's forge tests on engine-produced vectors (433), not by this rehearsal.
- The edge module (per-action geo assertion, `compliance/geoblock/edge`) is not a process of the stack: J-04 is
  blocked with that reason.
- The fee sweep at the UTC day boundary (fee account -> withdrawal record -> owner's address) needs a day boundary:
  not rehearsed.
- The backstop keeps the position it took (the engine has no unwind): a stack is rehearsed once, front to back.
- UNAUDITED. A passing rehearsal is evidence about one run on one machine, not a proof.

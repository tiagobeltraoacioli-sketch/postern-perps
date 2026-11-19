# postern-perps: PosternDEX Perpetuals (UNAUDITED private prototype)

A perpetual-futures exchange in the "transparent order book run by a validator set, collateral in a contract on
another chain" model: a deterministic engine, one sequencer and a 5-of-7 validator committee, an oracle committee,
a custody vault on the Bloch L2 (EVM, chain 700770), a trading frontend, and the compliance data that gates access.

**Status.** Prototype, not audited, not deployed, not for deployment before an external audit and a legal review.
Nothing in this repository is a statement that the product may be offered anywhere: every legal text here is a draft
for counsel. No real key, fund or network is used by anything here; every key in tests and local runs is a
throwaway derived from a public label or generated at run time.

**Licence.** Proprietary. Copyright (c) 2026 Bloch Inc. All rights reserved (see [LICENSE](LICENSE)); no open-source
licence has been granted.

## What is where

| Directory | What it is | Its document |
|---|---|---|
| `engine/` | the deterministic state machine (Rust, integer arithmetic only): accounts, cross margin, order book, mark, funding, liquidation (book, backstop, ADL), fees, forced requests, state root and proofs, snapshots; CLI `engine replay / prove / genesis` | [docs/SPEC.md](docs/SPEC.md) |
| `node/` | sequencer, validators, checkpoints, L2 watcher (two endpoints pinned to one block hash, finality), oracle ingest, checkpoint relayer, REST + WebSocket API | [docs/API.md](docs/API.md) |
| `node/oracle/` | oracle rule (committee median, agreement, jump halt, exit band), feeder with venue adapters, recorded and scripted replay | `node/oracle/README.md` |
| `node/mock/` | a mock of the API for frontend work, and the shared API conformance suite that runs unchanged against the mock or the real node | `node/mock/README.md` |
| `contracts/` | `PerpsVault` (Solidity): deposits, withdrawals against committee-signed checkpoints and Merkle proofs, forced requests, escape hatch; no owner, no upgrade | [docs/VAULT.md](docs/VAULT.md), `contracts/DEPLOY.md` |
| `frontend/` | the trading application (React, self-hosted assets, in-house canvas chart) | none yet (scripts in `frontend/package.json`; `frontend/scripts/dev-local.mjs` runs it against the local stack) |
| `compliance/` | markets and risk parameters, restricted-jurisdiction list with sources, legal drafts for counsel, the geoblocking module (JS, Rust port, edge module) | [docs/MARKETS.md](docs/MARKETS.md), `compliance/README.md`, `compliance/geoblock/DESIGN.md` |
| `e2e/` | the local full stack (one command up, one down) and the scripted rehearsal | [e2e/README.md](e2e/README.md) |

Which branch is what, and the state of each line: [BRANCHES.md](BRANCHES.md).

## The system in one paragraph

A user deposits bUSDC into the vault on the L2; the node credits the engine only from the L2's `finalized` tag when
two independent endpoints agree. Every user action is an EIP-712 message (domain bound to the vault address) that
the engine itself verifies; the sequencer orders actions into blocks but cannot forge, replay or alter one. Seven
validators replay every block; five signatures over a checkpoint make it final. The vault pays a withdrawal only
against a final checkpoint and a depth-32 Merkle proof of the withdrawal record. A user the sequencer ignores posts
a forced request on the L2 (cancel, close, withdraw); a checkpoint that leaves it unprocessed past its deadline is
refused, and anyone can then put the vault into escape, where every account leaves with a proof against the last
accepted checkpoint, users before the insurance and backstop shares.

## Running it

Rust is built only on the build host; Node.js, Foundry and Python run on the Mac. `e2e/README.md` has the commands:

```
bash e2e/build.sh <label> [git-ref] [tree]     # sync a tree to the build host and build node, engine CLI, oracle feeder
bash e2e/up.sh --label <label> --local-validators 4
node e2e/rehearsal/run.mjs                      # numbered checks, evidence JSON
bash e2e/down.sh
```

The rehearsal proves, on one local stack: deposit credited only after L2 finality and only when both L2 endpoints
agree; exact fees to the fee account; funding over several intervals to the unit; liquidation through the book, by
auto-deleveraging and through the backstop on a scripted oracle path; a withdrawal proven and paid by the vault; a
user exiting by forced requests alone; committee faults and restarts of validators and of the sequencer; an
independent replay of the published log by the engine CLI; geoblocked requests refused; the whole API conformance
suite; and the escape hatch end to end, with the books closing to the unit.

## Tests

| Area | Command | Where |
|---|---|---|
| engine | `cargo test --release` in `engine/` | build host |
| node | `cargo test --release --locked` and `cargo test --locked --features dev-oracle` in `node/` (the dev oracle must not compile in a release build) | build host |
| oracle | `cargo test --release --workspace` in `node/oracle/` | build host |
| geoblock Rust port | `cargo test --release` in `compliance/geoblock/rust/` | build host |
| all of the above | `bash e2e/hostctl.sh test-all.sh <label>` | build host |
| contracts | `forge test --offline` in `contracts/` | Mac |
| mock + conformance | `node --test test/*.test.mjs` in `node/mock/` | Mac |
| compliance data | `python3 -I -m unittest discover -s compliance/tools -p 'test_*.py'` | Mac |
| geoblock JS | `node --test` in `compliance/geoblock/` | Mac |
| e2e helpers | `node --test test/*.test.mjs` in `e2e/` | Mac |
| frontend | `npm run check` in `frontend/` | Mac |
| the Mac half in one go | `bash e2e/test-mac.sh [tree]` | Mac |

The counts at the current `main` are in [BRANCHES.md](BRANCHES.md).

## Known open findings (no Critical or High; details in the project's cross-area log and reviewer reports)

| ID | Severity | Area | What |
|---|---|---|---|
| R436-11 | Medium (design) | vault / node / frontend | escape needs the account tree of the last checkpoint; validators serve it from their own log, nothing obliges anyone to keep serving it |
| R436-18 | Low | frontend | the forced-close text does not say that a close in a halted market executes at the exit band |
| R436-19 (residue) | Low | e2e | the forced-request-to-escape story is rehearsed with an exchange that never SEES the request; a sequencer that sees it and ignores it while honest validators see it is not shown |
| R436-5 (residual) | Low | engine | no cap on the backstop's unrealised loss, no unwind of what it takes |
| R435-2 | Low | node | status endpoints know only in-process validators |
| R435-3 | Low | node API, SPEC wording | the liquidation fee an account pays is published nowhere; SPEC does not say it is taken on size x mark |
| R433-3 | Low (liveness) | vault | a backlog of old unrelayed checkpoints postpones escape-by-silence one `escapeDelay` each; a forced request cuts through |
| R395-9 | Info | oracle | the sequencer's choice among signed observations biases the median inside a stated bound |

## Not done / not proven
- The page has not yet placed an order or made a withdrawal against the real node: the harness
  `frontend/integration/browser/stack-write.mjs`, run once against the stack (rehearsal phase P), got the page's
  DEPOSIT through (vault and node), then stopped on two defects of the harness itself (a wrong `deposit` signature
  in its own ABI list; no reconnect after a page reload). The page, the node and the vault were not at fault.

- No external audit. The internal reviews (agents 395 and 436) are recorded in the cross-area log of the project
  folder; their open items are listed in BRANCHES.md.
- Not rehearsed: the fee sweep at the UTC day boundary (the engine clock cannot be moved in a local run) and a
  shortfall escape (insurance first, then pro rata: covered by the vault's forge tests on engine vectors only).
- The L2 in every run is a local anvil, not the Bloch L2 (no proof-based finality, no real bUSDC bytecode).
- One sequencer; a BFT sequencer set is designed for, not built. The committee (5 of 7) is trusted for safety, as
  docs/VAULT.md section 8 states.
- No validity proof of the engine. Escape proofs need the account tree of the last checkpoint: who serves it when
  the sequencer is gone is a stated design gap (R436-11).
- Isolated margin is specified but not implemented; the backstop has no unwind and no loss cap.
- Every parameter that is an owner decision (vault limits and delays, insurance and backstop seeds, the committee
  and guardian addresses) is a proposal in the documents, not a decision.
- Legal: which jurisdictions may be served at all is for counsel; the list denies every country until counsel
  decides.

# Branch index (state at the end of round 2, 2026-11-19)

UNAUDITED private prototype. Nothing on any branch is deployed by being here. "Reviewed" and "fit to merge" are the
verdicts of this project's own reviewers (agent 395 in round 1, agent 436 in round 2); no external audit has been
done. Tips move: read the current one with `git ls-remote origin <branch>`.

## `main`

Integrator: agent 435 (round 2; agent 393 in round 1). A branch is merged into `main` only when the whole test set
is green on the merged tree and no Critical or High finding is open against it.

On `main` at the end of round 2 (on top of `f0e3967`, the end of round 1): the whole round-2 system.

| Merged | Tip | What came in |
|---|---|---|
| `codex/perps-contracts-r2-20261118` | `95029f9` | vault v0.3 (system accounts fundable through the vault), local deployment, engine-produced vectors replayed in forge, invariants, deploy gate |
| `codex/perps-engine-r2-20261118` (inside the node branch) | `5106fea` | engine + SPEC 0.2.4 |
| `codex/perps-node-r2-20261118` | `737f5a7` | node API 0.3 from the real engine, oracle ingest, proofs, checkpoint relayer, validator processes with their own read service |
| `codex/perps-oracle-r2-20261118` | `c88ac6e` (first `850e3ae`) | feeder to ingest, replay tapes, calibration, rule C2d, exit-price rule |
| `codex/perps-e2e-r2-20261118` | branch tip | `e2e/` (stack and rehearsal), conformance for API 0.3, mock account tree in the SPEC 11 format, geoblock.json 0.2.2, markets.json 0.2.2 |
| `codex/perps-frontend-r2-20261118` | `a41ebe1` | the page against the real node and vault, vault module, wallet path, exact arithmetic, product pages |

They reached `main` through the integration branch `codex/perps-e2e-r2-candidate-20261118`, tested as one tree and
rehearsed end to end. It was held once, for a node test that was flaky in the release profile (R435-5: a nonce
collision inside the test; fixed by its owner at `cdf1b6b`, 25 of 25 release runs alone afterwards).

## Test counts (the no-regression rule: no count may fall without a written reason)

| Area | End of round 1 (`f0e3967`) | End of round 2 (`main`) | Measured |
|---|---|---|---|
| engine | 50 | 94 | build host, `cargo test --release` |
| node (release) | 39 | 93 + 1 ignored (the anvil scenario, run by hand) | build host |
| node (`dev-oracle` feature) | 41 | 96 + 1 ignored | build host |
| a release build with the dev oracle | refused | refused | build host |
| node/oracle | 60 | 95 | build host, `cargo test --release --workspace` |
| compliance/geoblock Rust port | 9 | 10 | build host |
| contracts | 54 | 81 | Mac, `forge test --offline` |
| node/mock | 29 | 37 | Mac |
| conformance checks against the mock | 222 | 231 | Mac |
| conformance checks against the real node | 188 | 228 | local stack, two funded accounts |
| compliance/tools | 48 | 59 | Mac |
| compliance/geoblock JS | 63 | 64 | Mac |
| e2e helpers | none | 8 | Mac |
| frontend (vitest) | 70 + 87 (app, chart) | 296 | Mac, `npm run check` |
| rehearsal checks | none | 74 checks of phases A to L and K: every one passed on the merged code, 1 blocked with its reason (the edge module is not a stack process); plus the forced-request escape variant; plus phase P (the page writes to the stack, 6 checks): 1 pass, 5 fail, all on the page harness (see the README) | local stack |

No test was removed in round 2 (each owner's statement, checked by reviewer 436 for engine, node, contracts and
oracle, and by the integrator's counts above). Tests restated with a written reason, as their owners report: three
engine tests (forced close in a halt, settlement after a long halt, R395-39) after review 436's findings; one oracle
test (liar values above the price bound); one contracts test (keyless deposits, vault v0.3); and, by the
integrator, guard G-1 of the jurisdiction list and one geoblock decision vector (RevokeAgent became an exit action,
R436-7: the property each pinned is kept by new tests).

## Round-2 branches (2026-11-18)

| Branch | Agent | What it is | State |
|---|---|---|---|
| `codex/perps-engine-r2-20261118` | 430 | engine + SPEC 0.2.x: round-1 findings closed, compliance parameters read at genesis, snapshots, oracle vectors, property suite, worked-example and proof vectors, review-436 fixes | merged at `5106fea`; reviewer 436: fit to merge, both Highs closed |
| `codex/perps-node-r2-20261118` | 431 | API 0.3 from the real engine, oracle ingest, account and withdrawal proofs, checkpoint relayer, separate validator processes, `/v1/genesis`, CORS | merged at `737f5a7`; reviewer 436: fit to merge, no High or Critical (passes 1 and 2 and the re-checks) |
| `codex/perps-oracle-r2-20261118` | 432 | feeder to node ingest, scripted and recorded replay tapes, run-time dev committee, calibration, rule C2d | merged at `c88ac6e` (reviewer 436: fit to merge); later commits (live hour, README) not merged |
| `codex/perps-contracts-r2-20261118` | 433 | vault v0.3 (system accounts fundable), local deployment, engine-produced vectors replayed in forge, invariants, keeper duty, deploy gate | merged at `95029f9`; reviewer 436: no finding above Low |
| `codex/perps-frontend-r2-20261118` | 434 | the page against the real node and vault, vault module, wallet path, exact arithmetic from the engine's vectors, product pages | merged at `a41ebe1` (`npm run check` green on the merged tree); reviewer 436 pass 2: Low findings only |
| `codex/perps-e2e-r2-20261118` | 435 | `e2e/` (stack + rehearsal), conformance suite for API 0.3, mock in the SPEC 11 tree format, compliance data fixes (geoblock 0.2.2, markets 0.2.2) | merged |
| `codex/perps-review2-r2-20261118` | 436 | reviewer demonstrations and the independent fuzzer | review material, never merged |

## Round-1 branches (2026-11-17)

All merged into `main` at `f0e3967` except the review branch; kept for history, do not build on them.

| Branch | Tip | What it is |
|---|---|---|
| `codex/perps-engine-20261117` | `e2fca7519` | engine and SPEC 0.1.6 (agents 350, 388) |
| `codex/perps-node-20261117` | `3ce7e6407` | sequencer, validators, API (351, 389) |
| `codex/perps-oracle-20261117` | `ccdb290ed` | oracle core and feeder (394) |
| `codex/perps-contracts-20261117` | `a637065b1` | PerpsVault v0.2 (352, 390) |
| `codex/perps-frontend-20261117` | `7575c78b8` | trading screen (353, 391) |
| `codex/perps-chart-20261117` | `3f4d4c32e` | canvas chart (396) |
| `codex/perps-compliance-20261117` | `47f95bdff` | jurisdictions, legal drafts, markets (354, 392) |
| `codex/perps-geoblock-20261117` | `38ca9a4ab` | geoblocking module (397) |
| `codex/perps-mock-20261117` | `a3e0b2f43` | API mock and conformance suite (393) |
| `codex/perps-hygiene-20261117` | `126ea62ce` | repository hygiene |
| `codex/perps-review-20261117` | `5169900ef` | reviewer demonstrations (395), never merged |

## How to verify a line

```
git fetch origin
git checkout --detach origin/<branch>
```

then the commands of the README's "Tests" table (Rust on the build host only), and for the system as a whole
`e2e/README.md`. Evidence of the rehearsal runs is kept outside the repository, in the project folder
(`agents/435-evidence/`).

## Open findings at the end of round 2

No Critical or High finding is open. Open at the end of the round (IDs as in the project's cross-area log):

| ID | Severity | Area | What |
|---|---|---|---|
| R436-11 | Medium (design) | vault / node / frontend | escape needs the account tree of the last checkpoint; validators now serve it from their own log (rehearsed), but nothing obliges anyone to keep serving it |
| R436-5 (residual) | Low | engine | no cap on the backstop's unrealised loss; no unwind of the position it takes (SPEC O-9) |
| R435-2 | Low | node | the status endpoints know only in-process validators; a remote validator that lags is invisible to an operator |
| R435-3 | Low | node API, SPEC wording | the liquidation fee an account pays is published nowhere; SPEC 6.3 does not say it is taken on size x mark |
| R395-9, R395-10, R395-21 | Info | oracle, API, SPEC | stated bounds and risks (selection bias bound; agent wallets against a colluding account; third-party deposit to an agent address) |
| R436-18 | Low | frontend | the forced-close text does not say that a close in a halted market executes at the exit band |
| owner inputs | n/a | vault, compliance | vault limits and delays, insurance and backstop seeds, committee and guardian addresses, every legal question (which countries, terms): proposals only |

Closed in round 2, as verified by reviewer 436 on the merged tips: R436-1, R436-4 and R436-10 (High), R436-2, -3,
-7, -8, -9, -12, -13, and R395-22. Reported fixed by their owners and not (yet) re-verified by the reviewer at the
time of the merge: R436-6 and R436-11 (b) (node), R435-1 and R435-4 (node; both observed fixed by the rehearsal
and the conformance suite), and the round-1 items R395-15, -17, -19, -20, -39 (engine, contracts). R395-32 (chart)
is reported fixed by the frontend's owner. R435-5 (a flaky node test) is fixed and re-run by the integrator.

## Not done / not proven

See the README's list. In addition: the counts above are from one run each on the stated machines; the Mac was
heavily loaded during the round, and a test that timed out under load was rerun alone before being called a failure.

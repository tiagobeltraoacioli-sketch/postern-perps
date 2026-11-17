# Branch index: the lines pushed on 2026-11-17

Which branch is what, which is audited or reviewed, which is work in progress, and how to verify it.
Tips are those of `origin` at 2026-11-17 04:56 UTC; branches keep moving during the day, so read the current tip with
`git ls-remote origin <branch>`. "Audit candidate" and "fit for external audit" are the verdicts of this
project's own reviewers; no external audit has been done. Nothing on any branch is deployed by being here.

| Branch | Tip | What it is | Status |
|---|---|---|---|
| `main` | `fafc6b7ef` | Perpetuals prototype (integrator: agent 393) | prototype; private; not deployed (legal review first) |
| `codex/perps-chart-20261117` | `3f4d4c32e` | Canvas chart component (agent 396) | WIP |
| `codex/perps-compliance-20261117` | `47f95bdff` | Jurisdictions, legal drafts for counsel, markets (agent 392) | WIP |
| `codex/perps-contracts-20261117` | `115aba656` | PerpsVault and docs/VAULT.md (agent 390) | WIP draft |
| `codex/perps-engine-20261117` | `318a9a683` | Engine and docs/SPEC.md (agent 388) | WIP |
| `codex/perps-frontend-20261117` | `525dae374` | Trading screen (agent 391) | WIP |
| `codex/perps-geoblock-20261117` | `6ec4469f9` | Geoblocking and sanctions module (agent 397) | WIP |
| `codex/perps-hygiene-20261117` | `00d62b03f` | Publication hygiene: this index (agent 398) | docs only |
| `codex/perps-mock-20261117` | `c93cd2ee4` | Mock server of docs/API.md and shared schema tests (agent 393) | WIP |
| `codex/perps-node-20261117` | `c9bb54b0c` | Sequencer, validators, API server (agent 389) | WIP |
| `codex/perps-oracle-20261117` | `09fa395c0` | Oracle adapters (agent 394) | WIP |
| `codex/perps-review-20261117` | `1928f3df6` | Reviewer demonstrations (review 395) | review material, not for merge |

Status words: **audit candidate / fit for external audit**: the internal reviewers found no open Critical,
High or Medium finding on that tip; **reviewed**: an internal review pass covered that tip; **WIP**: work in
progress, tests not necessarily green; **superseded**: kept for history, do not build on it; **review
material**: tests and scripts that demonstrate review findings, never merged.

## How to verify a line

```
git fetch origin
git checkout --detach origin/<branch>
each area's README names its tests
```

A tip's own documents (the route or package document under `docs/`) give its full test table and rehearsals.

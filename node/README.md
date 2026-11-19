# PosternDEX Perpetuals: the node (`node/`)

UNAUDITED PROTOTYPE. Nothing here is deployed; every key any command below uses is a THROWAWAY key derived from a
seed or generated at run time. `live` mode is refused by the binary. Copyright (c) 2026 Bloch Inc.

The node is the part of the exchange between the users and the deterministic engine (`engine/`, `docs/SPEC.md`):
ONE sequencer that orders signed actions into blocks, N validators that replay every block and sign checkpoints,
the watcher of the collateral vault on the L2 (`contracts/`, `docs/VAULT.md`), the relayer that carries final
checkpoints to that vault, the oracle ingest, and the public API (`docs/API.md`). Owned by agent 351 / 389
(round 1) and 431 (round 2); `node/oracle/` and `node/mock/` have their own owners and documents.

## 1. Trust model (v1, deliberately simple and said so)

| Party | Can | Cannot | Why |
|---|---|---|---|
| Sequencer (one) | order, delay and censor actions inside their nonce window (120 s past, 30 s future); choose the block time inside the validators' clock bound; delay oracle rounds (markets then halt) | forge or alter an action; invent or alter a deposit or a forced request; move funds; get a different state past the validators | every action carries the user's EIP-712 signature and is re-verified by each validator's engine; each validator compares L2 inputs with its OWN two-endpoint finalized view; withdrawals need k-of-n signatures over a root the validators computed |
| Validators (k of n, default 5 of 7) | sign a wrong checkpoint together (then funds are at risk: same trust as the vault's committee) | sign alone; sign two checkpoints at one height (persisted guard); be made to sign a block timed behind real time (R395-29) or past a missed forced deadline | set-once committee in the genesis and in the vault |
| Oracle signers (q of n) | move the index inside the honest range when at most (q-1)/2 lie | forge another signer's observation | `node/oracle/README.md`; the node only relays (section 6) |
| Checkpoint relayer | fail to submit (then VAULT.md 8a's bounds run) | anything else: `submitCheckpoint` takes any caller and verifies the committee's signatures | a gas-only key |
| A user who is censored | post `requestForced` on the L2; validators stop signing past its deadline; the vault enters escape | | `docs/VAULT.md` 7 |

A BFT sequencer set is a later step, designed for, not built.

## 2. Processes and commands

```
postern-perps-node dev          the sequencer, the API, and --local-validators of the committee in one process
postern-perps-node validator    one validator in its own process (--index i), following --sequencer URL
postern-perps-node verify-log   opens a data directory, replays it, prints height / roots / index digest
postern-perps-node dev-genesis  prints the dev genesis as JSON (ADDRESSES only, no key)
perps-loadgen                   signed PlaceOrders over HTTP against a dev node
perps-bench                     the sequencer path measured in process on the real engine (section 8)
```

GENESIS flags (set once; every process of one deployment gets exactly the same, or its `logHash(0)` differs and
it refuses the data directory): `--seed dev --validators 7 --threshold 5 --checkpoint-interval 10 --genesis-time MS
--vault 0x.. --oracle-committee 0xA,0xB,.. --oracle-quorum Q | --oracle-committee-file committee.json
--dev-funding-interval-ms MS`.

`dev` flags: `--data DIR --listen 127.0.0.1:48900 --local-validators N --block-ms 200 --snapshot-every 1000
--engine real|stub --l2 http://A,http://B --l2-start-block N --l2-poll-ms 1000 --token 0x.. --cors-origin ORIGIN (repeatable; none = no CORS)
--oracle-token-file F --oracle-gather-ms 500 --relay-rpc http://A[,http://B] --relay-key-file F --relay-poll-ms 2000
--geoblock FILE --edge-secret-file F --edge-client-header NAME --dev-default-country ZZ --ip-rate 50 --account-rate 20 --read-rate 100
--dev-oracle` (the last one only in a binary built with the dev-only cargo feature `dev-oracle`, which a release
build refuses to compile).

`validator` flags: GENESIS + `--index I --sequencer URL --data DIR --listen 127.0.0.1:P --l2 A,B --l2-start-block N
--l2-poll-ms MS`.

Both listen on loopback only and stop cleanly on SIGINT / SIGTERM (nothing is lost by stopping: a block is
fsynced before it is published).

A local stack (what `e2e/` automates): `dev-genesis` -> deploy the vault with that committee
(`contracts/deploy/local.sh`) -> `dev --vault .. --token .. --l2 anvil,proxy --oracle-committee-file ..
--oracle-token-file .. --relay-rpc .. --relay-key-file .. --local-validators 4` + three `validator` processes +
the oracle feeder.

## 3. Modules

| Module | What it does | Key rule |
|---|---|---|
| `eip712`, `sig`, `actions` | typed data, secp256k1, action digest and signer | strict: 65 bytes, low-s, v 27/28; decimals converted exactly or refused; successful recoveries memoised (pure function) |
| `types`, `genesis` | canonical inputs and blocks; set-once parameters hashed into `logHash(0)` | one encoding per value; Withdraw / ApproveAgent / RevokeAgent never agent-allowed |
| `engine` (`real`, `stub`, `view`) | the `Engine` trait, the adapter over `engine/`, API views | inputs cross as BYTES decoded by the engine's own decoder; views are read-only; display replicas are stated in `view.rs` |
| `log` | the durable hash-chained input log, its index, snapshots | section 4 |
| `sequencer` | admission, block building, recovery, signatures, frames, proofs | section 5 |
| `validator`, `checkpoint` | replay, latches, equivocation guard, checkpoint digest and aggregation | latch on any divergence; wait on an unseen deposit; no signature for a late block or past a missed forced deadline |
| `watcher` | vault events from two L2 endpoints at `finalized` | both endpoints must agree on chain id, finalized block hash and logs, or no progress |
| `relayer` | final checkpoints to the vault | section 7 |
| `oracle_pool` | observations per round from the feeders | section 6 |
| `index` | orders, fills, trades, candles, funding from the engine's events | not consensus; a function of the log; cross-checked against the engine every block |
| `archive`, `vserve` | state at a past height by replay; a validator's own log and read service | section 5 (proofs) |
| `geo` | the geoblock decision and the edge assertion | deny by default; exit-only actions from the list inside a fixed bound; sanctioned = refused for all |
| `api` | REST and WebSocket of `docs/API.md` | body 16 KiB; per-IP and per-account limits; reads rate-limited; heavy reads outside the sequencer lock |

## 4. Data directory

| File | Content | Rule |
|---|---|---|
| `blocks.log` | `"PPLB" \| len \| block \| keccak(block)` per block | fsync before publish; full re-verification at open (chain, times, sequencer signature) |
| `blocks.idx` | one 60-byte entry per COMPLETED append | written and fsynced after the record; decides the torn-tail rule: bytes with no entry are an append that never completed (cut); a record the index knows and the log cannot produce intact is refused, never cut |
| `snap-<height>.bin` | the engine's snapshot (SPEC 16) with height, logHash, state root, checksum | used only when the restored root equals the root logged for that height; the newest two kept |
| `index-<height>.json` | the market-data index of the same height | used only when checksum, height, logHash agree; else history starts after the snapshot |
| `checkpoints.jsonl` | final checkpoints with their signatures | each re-verified and matched to the log at open |
| `pinned-frames.json` | heights of checkpoints the vault accepted | their states are kept for account proofs |
| validator: `signed.log` | height and digest of every checkpoint it signed | equivocation guard |
| validator: `vlog/blocks.log`, `vlog/blocks.idx` | its own copy of the log | only blocks its `process` accepted in full |

Recovery: newest usable snapshot + replay of the tail, every block's root compared with the logged one; a snapshot
above the log's last block, a log shorter than its index, or any mismatch refuses to start (fail closed).

## 5. Sequencer

- **Admission** (a pre-filter; the engine re-checks everything): signature, signer authority, SPEC 2.5 nonce
  window and single use (applied state plus pending), the ENGINE's own decoder on the action (R436-4), per-account
  and per-IP rates, geoblock (API layer).
- **A block** takes: one oracle round (section 6), at most 2,000 deposits and 1,000 forced requests (consecutive
  prefixes in vault order), at most 5,000 actions, never more inputs or bytes than the engine accepts (R436-3).
  It is applied on a copy, logged, indexed, signed, then published. If the engine refuses a block the sequencer
  built, each input is asked on its own, refused actions are dropped and acknowledged as rejected, and the block
  is rebuilt once; the sequencer halts only when nothing droppable explains the refusal or an L2 input is the
  cause.
- **Checkpoints**: every `checkpointInterval` blocks and every block that creates a withdrawal or carries a forced
  request. The engine state of each checkpoint height is kept as a frame (the 8 newest final ones, the newest 8 the
  vault accepted, up to 32 not yet final, the 4 newest rebuilt).
- **Proofs**: `GET /v1/accounts/{a}/proof?height=H` (the vault's `claimEscape` input) from a frame, or by
  rebuilding the state of ANY final checkpoint from the log (`archive`); `GET /v1/withdrawals/{id}/proof?count=N`
  from an incremental tree (32 hashes). Both are re-verified against the checkpoint's root before they are served.
  Every `validator --listen` serves the same account proofs and the log from its OWN replay, so claims do not
  depend on the sequencer (R436-11).

## 6. Oracle ingest

Wire form and rules: `docs/API.md` 2.1 (agreed with agent 432). The node relays; the engine decides. The pool
pre-filters with the engine crate's own digest, keeps one observation per (round, signer), never selects (a round
enters a block with every observation the engine accepts at the block's time; R395-9), defers a round while a held
observation is still in the engine's future, and leaves out what can never be accepted (R436-12).

## 7. Relayer

`docs/VAULT.md` 8a: submits this node's newest final checkpoint when a withdrawal waits, when a forced request was
processed (at once), or `forcedDelay / 4` of L2 time after the vault's last checkpoint. It submits checkpoints only:
`withdraw`, `claimEscape` and the other user calls are the user's.

No L2 endpoint is trusted alone (R436-15, the watcher's standard): `--relay-rpc` takes at least TWO endpoints of
independent operators; the vault is read at the `finalized` block and only when every endpoint names the same
block and the same state; the agreed reading must be a checkpoint this node finalized, with its counts, or it is
not believed; the gas price is twice the highest any endpoint names and never above `--relay-max-gas-price-gwei`
(default 50; above it nothing is signed; one transaction costs at most `15,000,000 x cap`); a submission counts
when every endpoint returns its receipt with status 1. Retry on the next endpoint with a backoff that doubles to
30 s; gas limit `1.5 x estimate + 40,000`; EIP-155 legacy transactions signed in process.

Alarms (stderr `RELAYER ALARM`, `/v1/node` `relayer.alarm`): from the believed reading, `forcedDelay / 2` since an
open forced request, `escapeDelay / 2` of silence, escape; and from the node's OWN clock, whatever the endpoints
say: no agreed and plausible reading for `--relay-stall-alarm-ms` (120 s), a due checkpoint not confirmed for
`--relay-delivery-alarm-ms` (600 s), a gas price above the cap. The alarm is a log line and an API field: nothing
pages anyone (an operator must watch it).

## 8. Tests and measurements

`cargo test` in `node/` (build host only; `--features dev-oracle` adds the tests that need the dev-only
injection). `tests/anvil.rs` is ignored by default: it needs anvil, cast, forge and agent 433's contracts tree
(`PERPS_ANVIL_DIR`, `PERPS_CONTRACTS_DIR`, `PERPS_ANVIL_PORT`, `-- --ignored`) and proves the watcher, the relayer
and both proofs against the real PerpsVault. `tests/process.rs` runs the binaries as processes.

Performance of the sequencer path: `perps-bench` (method in its header), numbers and host in
`agents/431-node-r2.md` of the round's report folder.

## 9. Not done / not proven

- UNAUDITED prototype; not deployed; no real key was generated, read or used; `live` mode is refused.
- One sequencer: censorship and reordering inside the nonce window are possible and bounded only by the forced
  path; there is no sequencer rotation or BFT ordering.
- The API and the sequencer share one lock: account, ticker, book and list reads take it for a bounded piece of
  work each; a read replica is not built.
- History (index) is in memory, bounded, rebuilt from the log or restored from the index file; no history database.
- The state root is recomputed by the engine from every account on every block (linear in the number of
  accounts); frames are whole copies of the engine state.
- The watcher speaks plain HTTP to its two endpoints (a TLS-terminating proxy is needed for remote ones) and reads
  only the `finalized` tag; it was run against anvil, never against the Bloch L2.
- The relayer was run against anvil only; it does not watch payouts (`Withdrawal.status` stops at `certified`).
- The validator read service keeps whole engine states as anchors (every 1,000 blocks, 16 of them) and rebuilt
  states (4): bounded in number, not in bytes; heights older than the anchored range replay from genesis.
- The relayer's alarm is a log line and an API field; nothing pages an operator. Its gas-price cap default
  (50 gwei) is a placeholder: the Bloch L2's real price range is an owner input.
- Geoblocking is a control of the operator's access point; the L2 forced path cannot be geoblocked by the node.

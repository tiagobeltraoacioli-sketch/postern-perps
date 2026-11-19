# `node/mock/`: reference mock of the API and the shared conformance tests (UNAUDITED prototype)

Owner: agent 393. Development and test use only: every key here is a throwaway key derived from a public label,
the secp256k1 code is not constant time, and nothing here is the engine. Never point it at real funds or keys.

## What it is

- `server.mjs`: a deterministic mock of `docs/API.md` v0.1 (REST and WebSocket) whose signing rules follow
  `docs/SPEC.md` v0.1: EIP-712 domain `{PosternPerps, 1, 700770, verifyingContract = the mock vault}`, the eight
  action types in SPEC field order, prices and sizes in the fixed 1e-8 scales, USDC in 1e-6, the nonce window
  `[T - 120 s, T + 30 s]` with a 64-entry used set per signer, SPEC error names, `actionHash = keccak(digest ||
  signer)`. Market parameters are read from `compliance/markets.json` and `compliance/risk.json` (392);
  geoblocking from `compliance/geoblock.json` (deny by default, exit-only) plus a development allow-list
  (`fixtures/geoblock.mock.json`, which can never override a listed country: the server refuses to start).
- `conformance/run.mjs`: the shared API tests. They run unchanged against this mock or the real node in dev mode:
  every response validated against the schema (`node/api/schema.json` when 389 publishes it, else the draft
  `schema/api.schema.json`), plus semantic checks (book order, tick and step, candle OHLC, pagination, the
  log-hash chain recomputed, checkpoint digests recomputed and every signature recovered, certificates, admission
  refusals: replay, nonce window, high-s, v 0/1, short signature, stranger, unknown field, non-canonical integer,
  16 KiB, tampered message, price 0, agent limits, geoblocking, sanctions, halted market, sequencer down; WebSocket
  channels with delta chaining). `--mock` adds the checks that need scenario switches.
- `lib/`: keccak256, secp256k1 sign/recover, EIP-712 (flat structs, 3- or 4-field domain), fixed-point decimals,
  a SplitMix64 RNG, a small JSON-schema validator (an unknown keyword is an error), the exchange simulation, the
  API wire shapes and admission. All checked against viem-recorded vectors (`test/vectors/`).

## Run

```
node node/mock/server.mjs --port 49300
```

```
node node/mock/server.mjs --port 49300 --static --start-time now
```

```
node node/mock/conformance/run.mjs --base http://127.0.0.1:49300 --mock
```

```
node node/mock/conformance/run.mjs --base http://127.0.0.1:48900 --funded
```

```
cd node/mock && node --test test/*.test.mjs
```

Against a real node (no `--mock`), the checks that need an account with collateral (price-0 refusal by the engine,
agent approval and rights, book deltas) are SKIPPED, listed and never counted as passed, unless `--funded [LABEL]`:
the suite then signs as the throwaway account `--print-accounts` names, which the operator funds through the dev
vault deposit on the L2 before the run (the API has no deposit route by design: deposits come only from the vault).
Without an L2, `conformance/fake-l2.mjs` serves the vault's VAULT.md 0.2 `Deposit` events (head-chained) to a dev
node's watcher: start it twice (the node requires two agreeing endpoints) with the node's vault address and
`--deposit ACCOUNT:AMOUNT`, and give the node `--l2 http://127.0.0.1:P1,http://127.0.0.1:P2`. With `--funded` and
no checkpoint yet, the suite sends CancelAll actions (at most 40) until one exists.

Since API 0.3 the suite also checks, bit by bit: the account proof (`leafData.encoded` is the 88-byte SPEC 11 leaf
of the named fields, `leaf = keccak256(0x00 || encoded)`, the 32 siblings walk to the final checkpoint's
`accountsRoot`); and, against a server that does not call itself a mock, with `--funded` AND `--funded2 [LABEL]`
(a second throwaway account funded the same way): a real fill between the two accounts (order ids and status in the
action result, `GET /v1/orders/{id}`, both fills, the public trade, both positions, the 1m candle, the ticker, the
fees charged against the published `takerFeeBps` / `makerFeeBps`, the round trip costing exactly the fees), then a
withdrawal followed to `certified` and its `WithdrawalProof` verified to the checkpoint's `withdrawalsRoot`.
Without `--funded2` those checks are skipped with that reason. From `lib/merkle32.mjs`, written from SPEC 11 and
VAULT.md 5.5 and pinned to the vault's own vectors (`contracts/test/vectors/vault-vectors.json`); the mock's
account tree and account proof are in that format too (its withdrawal tree is still its own: "Fidelity").

Options: `--seed N` (default 7), `--start-time MS|now` (default 1794878400000; `now` makes the engine time
follow the wall clock from launch so a browser's millisecond nonces fall inside the window), `--static` (blocks
only when an action arrives or on `step`: byte-stable screenshots and tests), `--blocks N` initial blocks,
`--block-ms N`, `--no-rate-limit`, `--nonce-window BACK:AHEAD|off` (off only for development against a clock that
differs from the engine time), `--eip712 FILE`, `--markets FILE`. Loopback only. The server needs Node 20+;
the conformance tests and the test suite need Node 22+ (global WebSocket).

Demo data: `GET /mock/info` names the demo account (positions long BTC cross, short ETH isolated; a resting
partly filled order, a post-only ask, an untriggered stop; history in every final status; funding payments;
deposits `seen`, `l2Finalized`, `credited`; withdrawals in every status, the certified ones with real committee
signatures and a Merkle proof). Any other address read while `accounts` is not `empty` gets the same demo data
on first read (mock convenience; the real node returns a zero account).

## Scenario switches: `GET /mock/scenario?<name>=<value>` (several per request)

| Name | Value | Effect |
|---|---|---|
| `halt` / `resume` / `closeOnly` | symbol | market status `halted` (with reason) / `active` / `closeOnly` |
| `oracle` | `SYMBOL:ok\|stale\|deviation\|insufficientSources` | oracle status; anything but ok halts the market |
| `busy` | `1` / `0` | 10 ms blocks with three takers per market (>= 300 WebSocket messages/s on book + trades) |
| `geo` | ISO2 or `none` | default country of requests without `X-Mock-Country` / `?country=` |
| `sanction` / `unsanction` | address | add to / remove from the mock sanctions list (451 `SANCTIONED` for every action) |
| `vault` | `normal\|paused\|escape\|unknown` | `Status.vaultMode`; withdrawals stop before `submitted` unless normal |
| `sequencer` | `running\|recovering\|halted` | no blocks; actions refused 503 `SEQUENCER_UNAVAILABLE` |
| `finality` | `stalled` / anything else | no new checkpoints (and `committee.online` below threshold) |
| `eip712` | `pinned` / `drift` | `drift` publishes typed data that differs from the pin (the frontend must refuse to sign) |
| `accounts` | `empty` / anything else | unknown addresses read as zero accounts / get demo data |
| `demo` | address | seed demo data for that address now |
| `deposit` | `ADDRESS:AMOUNT` | an L2 deposit: `seen`, then `l2Finalized`, then credited by a deposit input |
| `disconnect` | seconds | drop every WebSocket and refuse new ones for that long |
| `liquidate` | symbol | liquidate every user position in that market at the mark against the backstop |
| `step` | N | produce N blocks now |
| `nonceWindow` | `BACK:AHEAD` / `off` | change the nonce window |

The names match the frontend stand-in `frontend/mock/server.mjs` (391), so its harness runs against either.

## Fidelity (what the mock does NOT do like the engine)

Decided (technical): the mock follows SPEC v0.1 wherever a client can observe it through signing, admission or a
documented response field, and is simplified inside. Not like SPEC v0.1:
- matching is price-time but the maker ladder is simulated (one liquidity account, `GET /mock/info`), simulated
  takers trade only through it and users' resting orders; trades between simulated quotes and simulated takers
  settle nothing (both sides are the liquidity account);
- margin: tier fraction by notional (not the continuous piecewise schedule of SPEC 6.2), open orders are not
  counted in the initial requirement, isolated positions are kept in the account-level checks; liquidation closes
  whole positions at the mark against the liquidity account (no bankruptcy-price IOC, no ADL, no fee to INSURANCE
  from the remaining equity rule);
- funding uses the average (mark - index) / index sample per block, not impact prices; no daily fee sweep;
- the oracle input IS 394's `OracleUpdate` (five of seven derived oracle keys sign `OracleObservation` in the
  engine domain every 3 s round) and `inputsEncoded` IS SPEC 3.2's binary encoding (the conformance suite decodes
  it strictly and compares it with `inputs`), but a mock block may carry no input and the simulated market moves
  without inputs, so the mock's log is not replayable by an engine; the oracle committee keys are not published
  by the API; the state root, account proof and withdrawal Merkle tree are the mock's own formats (commutative
  pairs), not SPEC 11's; the checkpoint is VAULT.md v0.2's sixteen fields and its stateRoot follows the SPEC 11 equation
  (system hash, chain heads as VAULT.md 5.4), but the three subtrees are the mock's own Merkle format;
- `accounts` defaults to demo data for unknown addresses (the API says a zero account).

Exactness that IS kept: integer arithmetic only, `priceDecimals + sizeDecimals <= 6` checked so price x size
converts to USDC exactly, exact cost-basis accounting, and the conservation invariant `sum(balances + isolated
margins) + insurance - sum(signed cost basis) == net deposits` with equality at every block (tested), sizes sum
to zero per market (tested).

## Not done / not proven

The mock is not the engine and proves nothing about the engine. Not done: SPEC 11 trees and proofs, forced requests, the fee
sweep, ADL, open-order margin. The conformance suite has not yet been run against the real node (it does not
exist yet). The busy-scenario rate is wall-clock dependent.

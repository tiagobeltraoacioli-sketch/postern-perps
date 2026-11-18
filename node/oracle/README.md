# PosternDEX Perpetuals: price oracle (`node/oracle/`)

UNAUDITED PROTOTYPE. Owned by agent 394. Nothing here is deployed. Version 0.2 (2026-11-17): code in `core/` and `feeder/` implements every rule below.

The oracle gives the engine one USD index price per market. It never trusts one venue and never trusts one
signer: every index value is a median of medians (venues inside each validator's feeder, then validators inside the
engine), and every step has a stated bound, a halt condition and a test.

Files: this document (design and normative rules), `SOURCES.md` (venues per market, endpoints, terms of use with the
read date), `core/` (deterministic rules in Rust: the engine's acceptance rule and the feeder's aggregation rule),
`feeder/` (venue adapters, fetch loop, signer, CLI), `fixtures/` (recorded venue responses and adversarial cases),
`vectors/` (EIP-712 and encoding test vectors shared with the engine).

## 1. Trust model

| Party | Can | Cannot | Bounded by |
|---|---|---|---|
| One venue (lies, freezes, flash-crashes, goes down) | report any price for its own pair | move a validator's observation outside the range of the honest venues while honest venues are a strict majority of the valid readings | feeder rule F4 to F6 (median, outlier rejection, strict-majority rule), frozen and staleness checks |
| A majority of the venues of one market (coordinated manipulation) | move every honest validator's observation | move the index by more than `jumpBps` per round without `confirmRounds` stable rounds | committee rule C5 (jump halt and confirmation) |
| One oracle signer (validator's feeder key) | sign any price | move the committee median outside the honest range while at most `(q-1)/2` signers lie | committee rule C3 (median of at least `q` signed observations) and C4 (agreement band) |
| The sequencer | choose which valid observations of a round to include (at least `q`), delay or withhold updates | forge or alter an observation (signed), replay an old round (C1), move the median outside the range of the honest observations it includes | C1 to C4; withholding only halts the market (fail closed, `STALE`) |

Liveness needs `q` honest, online signers whose feeders reach at least `minSources` venues, and an honest sequencer.
Safety (the index is inside the honest range) needs at most `(q-1)/2` lying signers among the included observations
and at least `q` agreeing within `agreeBps`. With the default committee `n = 7`, `q = 5`: two signers may be offline
or lie.

Decided (technical): the oracle signers are the validator operators, each with a DEDICATED oracle key (not the
checkpoint key). Reason: the oracle key signs every 3 s in a process that makes outbound connections to public
venues (larger attack surface); its compromise must not give checkpoint (withdrawal-authorising) power. The EIP-712
type already separates the two message kinds; the separate key separates the blast radius. The oracle committee is
set once in the engine's genesis, like the checkpoint committee.

## 2. Rounds and time

- `ROUND_MS = 3000`. Round `r` covers observation times `[r * 3000, r * 3000 + 3000)` in Unix milliseconds.
- Each feeder samples once per round, starting at the round's first millisecond (clocks synchronised by NTP; a
  feeder whose clock is off by more than `maxClockSkewMs` from the median venue clock signs nothing).
- The engine never reads a clock: it compares observation times with the block time `T` that the sequencer
  proposes and the validators accept (`docs/API.md` section 1, "Time").

## 3. The signed observation (what each oracle signer signs)

EIP-712, the ENGINE domain of `docs/SPEC.md` 2.1 (agreed with 388 as O-7, 2026-11-17; v0.1 of this document had
the provisional `{PosternPerps, 1, 7007701}` without `verifyingContract`):
`EIP712Domain(string name,string version,uint256 chainId,address verifyingContract)` =
`{PosternPerps, 1, 700770, <PerpsVault address of the deployment>}`, so an observation signed for a test or staging
deployment is invalid on production. Primary type `OracleObservation`:

```
OracleObservation(uint64 round,uint64 timeMs,OraclePrice[] prices)OraclePrice(uint32 marketId,uint64 price,uint8 sources)
```

| Field | Rule |
|---|---|
| `round` | the round of `timeMs`: `timeMs / 3000` (integer division) |
| `timeMs` | the signer's sampling time, inside the round |
| `prices` | one entry per market observed this round, `marketId` strictly ascending, 1 to 256 entries; a market the signer cannot price is absent (it abstains) |
| `price` | USD per one whole base unit, as an integer in the market's price scale (the scale of `PlaceOrder.price` in `docs/SPEC.md`), rounded half up from the exact value; never 0 |
| `sources` | number of venues that survived the feeder's rule F5 (informational, and checked: the engine ignores an entry below the market's `minSources`) |

Signature: 65 bytes `r || s || v`, `s <= n/2` (low-s), `v` in `{27, 28}`, over
`keccak256(0x19 0x01 || domainSeparator || hashStruct(observation))`. Test vectors: `vectors/eip712.json`
(digests cross-checked against an independent implementation).

## 4. The oracle update input (what the sequencer puts in a block)

`OracleUpdate { round: u64, observations: [SignedObservation] }`,
`SignedObservation { signer: u8, timeMs: u64, prices: [OraclePrice], signature: [u8; 65] }` where `signer` is the
index of the signer in the oracle committee and the signed message is `OracleObservation{round, timeMs, prices}`.

JSON (block `inputs` entry `{"kind":"oracle","payload":…}` of `docs/API.md` section 3.3, and the feeder's submission
to the sequencer):

```json
{"round":598295763,"observations":[{"signer":0,"timeMs":1794887290471,
  "prices":[{"marketId":1,"price":"649924","sources":7}],"signature":"0x…(65 bytes)"}]}
```

Canonical bytes (proposal for `inputsEncoded`; `docs/SPEC.md` decides the envelope and the input tag):

```
OracleUpdate      = round:u64be  count:u8  SignedObservation{count}
SignedObservation = signer:u8  timeMs:u64be  priceCount:u16be  OraclePrice{priceCount}  signature:65 bytes
OraclePrice       = marketId:u32be  price:u64be  sources:u8
```

No trailing bytes; `count`, `priceCount` and every ordering rule below are part of the canonical form.

## 5. The engine's acceptance rule (normative; deterministic integer arithmetic)

Parameters per engine: oracle committee `signers[0..n)` (set once), quorum `q`; per market: `minSources`,
`agreeBps`, `jumpBps`, `maxStalenessMs`, `confirmRounds`; global: `maxFutureMs`, `maxAgeMs`.

Parameters are refused at genesis (`CommitteeParams::validate`) unless: `1 <= n <= 255`, `n/2 < q <= n`, signers
distinct and non-zero, markets strictly ascending (1 to 256), `minSources >= 1`, `1 <= agreeBps <= jumpBps <= 10000`,
`maxStalenessMs >= 3000`, `maxAgeMs <= maxStalenessMs` of every market, `confirmRounds >= 2`.

Validity of the whole input (any failure: the input is rejected with the named error and NO state changes). Error
order, normative so every implementation reports the same error: C1; then C2 over the whole input; then for each
observation in input order C2c before C2b; the first failing check is reported:

- C1 `ORACLE_ROUND_NOT_NEW`: `round > lastOracleRound`.
- C2 `ORACLE_MALFORMED`: `q <= count <= n`; `signer` strictly ascending and `< n`; every `prices` list strictly
  ascending by `marketId`, 1 to 256 entries, every `price > 0`.
- C2b `ORACLE_BAD_SIGNATURE`: every signature is 65 bytes, low-s, `v` in `{27, 28}`, and recovers to
  `signers[signer]`.
- C2c `ORACLE_TIME`: every `timeMs` is inside the round, `timeMs <= T + maxFutureMs` and `T - timeMs <= maxAgeMs`.

Then `lastOracleRound = round`, and for each market `m` the engine knows (entries for unknown markets are ignored),
in ascending `marketId` order:

- A round rejected by C3 or C4 for a market still consumes the round number (C1 is about the input).
- C3 values `V` = the `price` of `m` in each observation whose entry has `sources >= minSources(m)`. If `|V| < q`:
  no change for `m` (`NO_QUORUM`). Else `M = sort(V)[|V| / 2]` (the median; the upper median when `|V|` is even).
- C4 `A` = the number of `v` in `V` with `|v - M| * 10000 <= agreeBps * M` (u128). If `A < q`: no change for `m`
  (`DISAGREE`). Else `M` is the committee price of `m` for round `round`, at time `tau = round * 3000`.
- C5 the state machine of section 6 consumes `(M, tau)`.

Why the median is safe: with `|V| >= q = 2f + 1` and at most `f` lying values, `sort(V)[|V|/2]` has at least `f`
values on each side, so it lies between two honest values. C4 refuses a round in which honest signers disagree
(a sign that venues diverge or that sampling times are too far apart).

## 6. Halt and resume (per market, normative)

State per market: `index` (u64, market price scale; 0 before the first), `indexTimeMs`, `status` (`active` or
`halted`), `haltReason` (`INIT` at genesis, `STALE` or `JUMP`), and a confirmation run `{last, count, lastTimeMs}`.
Genesis: every market `halted` with `INIT`; the first index needs `confirmRounds` agreeing committee prices.
Canonical state bytes for the engine's commitment: `OracleState::encode` in `core/src/committee.rs`. A committee
price that is `NoQuorum` or `Disagree` leaves the run unchanged (the freshness bound on the run handles gaps).

- H1 (at the start of every block, before its inputs): if `status = active` and `T - indexTimeMs > maxStalenessMs`:
  `status = halted`, `haltReason = STALE`, run cleared.
- H2 committee price `(M, tau)` while `active`: if `|M - index| * 10000 <= jumpBps * index`: `index = M`,
  `indexTimeMs = tau`. Else `status = halted`, `haltReason = JUMP`, run `= {M, 1, tau}`; the index does not move.
- H3 committee price `(M, tau)` while `halted`:
  - fast return: if `tau - indexTimeMs <= maxStalenessMs` and `|M - index| * 10000 <= jumpBps * index` (the move
    reverted while the index is still fresh): `index = M`, `indexTimeMs = tau`, `status = active`, run cleared.
  - otherwise, confirmation: if a run exists, `tau - run.lastTimeMs <= maxStalenessMs` and
    `|M - run.last| * 10000 <= agreeBps * run.last`: `run.count += 1`, `run.last = M`, `run.lastTimeMs = tau`; else
    run `= {M, 1, tau}`. When `run.count >= confirmRounds`: `index = M`, `indexTimeMs = tau`, `status = active`,
    run cleared.
- The index moves only by (a) a bounded step from a fresh index, or (b) `confirmRounds` successive committee prices
  that agree with each other. A flash move that reverts within the staleness window never reaches the index.
- While `halted` (recommendation to the engine, 388 decides): no matching except cancels, no liquidations, no
  stop or take-profit triggers, no funding samples. Exits are NOT blocked: they use the exit price of section 6a.

## 6a. Exit price path (normative; R395-5, coordinator decision 2026-11-17 04:59 UTC)

The sequencer alone can halt every market (it chooses whether an oracle input is in a block: H1 then halts the
market 10 s later). A halt must therefore never leave a withdrawal of free collateral or a forced close without a
price. `exit_price(params, state, market, T)` (`core/src/committee.rs`) defines one in every state except genesis
(`INIT`: no index ever existed, so no trade and no position can exist):

- X0 active: `low = high = settle = index`.
- X1 halted: the band `[low, high]` with half-width `h = min(exitMaxHaircutBps, exitHaircutBps +
  exitHaircutPerMinBps * floor(age / 60000))` bps, `age = T - indexTimeMs` (time since the last good index);
  `low = floor(min(index, haltLow) * (10000 - h) / 10000)` (at least 1), `high = ceil(max(index, haltHigh) *
  (10000 + h) / 10000)`.
- X2 `haltLow`/`haltHigh` are the lowest and highest committee price (C3 and C4 passed) since the market halted,
  including the price that caused a `JUMP` halt; they only widen the band and are cleared on resume; they are part
  of the canonical state bytes. `settle` = the latest committee price since the halt, else the last good index.
- X3 `settleDue` when `age >= maxHaltMs`.

Proposed use by the engine (388 decides and writes it into SPEC): a withdrawal of free collateral values every
position in a halted market at the side of the band worse for its holder (long at `low`, short at `high`); a forced
close of a position in a halted market settles at `settle` and is checked against the band (a forced close is
never refused for a halt); after `settleDue` every position of the market is settled at `settle`, and a forced
request that still cannot be served is not consumed (the vault's deadline then fires escape).

Why this is safe for the other users: the band covers the last good index and every price the committee has
signed since, widened by at least one permitted step (`exitHaircutBps >= jumpBps`) and by a further amount per
minute of darkness, so a long cannot withdraw collateral that a crash the committee already saw would have consumed;
the sequencer cannot narrow it (X2 only widens; withholding only ages the index, which widens `h`). Cost (stated):
during a long halt honest users can withdraw less than their mark-to-market equity, and `settle` after a halt
without any committee price is the pre-halt index (the only price the protocol stands behind).

Defaults (section 8): `exitHaircutBps = jumpBps`, `exitHaircutPerMinBps = 100`, `exitMaxHaircutBps = 5000`,
`maxHaltMs = 3600000` (one hour, well inside the vault's one-day forced-request delay).
- Stated limit: the halt machine bounds the size of one step (`jumpBps` per round from a fresh index), not the
  level. A coordinated manipulation of a majority of a market's venues that creeps in steps below `jumpBps`, or
  holds a new level for `confirmRounds` rounds, reaches the index. The defences against that are the choice of
  deep, independent USD venues (`SOURCES.md`), the engine's mark-price and liquidation rules (388) and the
  validators' own refusal to sign a checkpoint whose price deviates from their own view (389's proposal E).
- Trade-off (stated): during a violent genuine move the market stays halted until prices are stable for
  `confirmRounds` rounds, so liquidations wait and the insurance fund carries the gap; the alternative (following a
  jump at once) is what an oracle manipulator needs.

## 7. The feeder's aggregation rule (per validator, per round, per market)

- F1 one reading per configured venue (one per operator), fetched with a timeout of `fetchTimeoutMs`.
- F2 a reading is valid if `bid > 0`, `ask > 0`, `bid <= ask`, `(ask - bid) * 10000 <= maxSpreadBps * mid`, `last`
  (when the venue gives one) `> 0`, its venue time (when the venue gives one) is not more than `maxFutureSkewMs`
  ahead of the receive time and not older than the bound of its kind (snapshot time `maxSnapshotAgeMs`, last-trade
  time `maxTradeAgeMs`), and it is not frozen (unchanged for `frozenAfterMs` while the market's observation moved
  by at least `frozenMoveBps`).
- F3 the reading's price is (USDT/USD rate sources: four USD venues, without Bitfinex, see `SOURCES.md`) `median(bid, ask, last)` (equal to `last` clamped into `[bid, ask]`), or `(bid + ask) / 2`
  when the venue gives no last price, at 18 decimals; a venue quoted in USDT is converted with the round's
  USDT/USD rate (the same rule F1 to F6 over USD venues of the pair USDT/USD); without a rate, or with a rate more
  than `maxDepegBps` from 1, readings in that quote are dropped.
- F4 `M0 = sort(p)[c / 2]` over the `c` valid readings; survivors are the readings with
  `|p - M0| * 10000 <= maxSourceDeviationBps * M0`.
- F5 abstain unless `survivors >= minSources` and `2 * survivors > c` (a strict majority of the valid readings agree).
- F6 the observation price is `sort(survivors)[survivors / 2]`, rounded half up to the market's price scale;
  `sources = survivors`.

## 8. Default parameters (proposal; 392 publishes the final values in `markets.json`)

| Parameter | Default | Applies in |
|---|---|---|
| `ROUND_MS` | 3000 | everywhere |
| committee `n`, quorum `q` | 7, 5 | engine |
| `maxFutureMs`, `maxAgeMs` | 1000, 6000 | engine |
| `maxStalenessMs` | 10000 | engine, per market |
| `agreeBps` | 100 | engine, per market |
| `jumpBps` | BTC 500, ETH 500, SOL 800 | engine, per market |
| `confirmRounds` | 3 | engine, per market |
| `minSources` | 3 | feeder and engine, per market |
| `maxSourceDeviationBps` | 200 | feeder, per market |
| `maxSpreadBps` | 100 | feeder, per market |
| `maxSnapshotAgeMs`, `maxTradeAgeMs`, `maxFutureSkewMs` | 5000, 300000, 2000 | feeder |
| `frozenAfterMs`, `frozenMoveBps` | 60000, 20 | feeder |
| `fetchTimeoutMs`, `maxClockSkewMs` | 1500, 1000 | feeder |
| `exitHaircutBps`, `exitHaircutPerMinBps`, `exitMaxHaircutBps`, `maxHaltMs` | `jumpBps`, 100, 5000, 3600000 | engine, per market (section 6a) |
| USDT/USD conversion: `minSources`, `maxDepegBps` | 3, 500 | feeder |

## 9. Not done / not proven

See the report `~/Desktop/POSTERNDEX-PERPS-20261117/agents/394-oracle.md` for the current list. In short: not
reviewed; parameters not calibrated on historical data; venue terms of use and most rate limits unconfirmed; the
feeder's submission to the sequencer and the node's ingest are not wired (389); the engine does not call `core` yet
(388); the price scale (10^priceDecimals per market vs a fixed 1e8) waits for SPEC.md.

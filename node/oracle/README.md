# PosternDEX Perpetuals: price oracle (`node/oracle/`)

UNAUDITED PROTOTYPE. Nothing here is deployed. Version 0.3 (2026-11-18, agent 432, round 2; versions 0.1 and 0.2 by
agent 394): code in `core/` and `feeder/` implements every rule below. Round 2 added: the engine price scale
(R432-1), the node ingest (section 4a), the selection bound of R395-9 (section 5a), the drop rule for a failing
venue and Kraken batching (section 7a), the deterministic replay source (section 7b), a one-hour live run and the
calibration of the parameters on public minute candles (section 8a, `calibration/`).

The oracle gives the engine one USD index price per market. It never trusts one venue and never trusts one
signer: every index value is a median of medians (venues inside each validator's feeder, then validators inside the
engine), and every step has a stated bound, a halt condition and a test.

Files: this document (design and normative rules), `SOURCES.md` (venues per market, endpoints, terms of use with the
read date), `core/` (deterministic rules in Rust: the engine's acceptance rule and the feeder's aggregation rule),
`feeder/` (venue adapters, fetch loop, signer, CLI), `fixtures/` (recorded venue responses and adversarial cases),
`vectors/` (EIP-712 and encoding test vectors shared with the engine), `tapes/` (replay tapes), `calibration/`
(scripts, results and evidence of the live run and of the parameter calibration).

## 1. Trust model

| Party | Can | Cannot | Bounded by |
|---|---|---|---|
| One venue (lies, freezes, flash-crashes, goes down) | report any price for its own pair | move a validator's observation outside the range of the honest venues while honest venues are a strict majority of the valid readings | feeder rule F4 to F6 (median, outlier rejection, strict-majority rule), frozen and staleness checks |
| A majority of the venues of one market (coordinated manipulation) | move every honest validator's observation | move the index by more than `jumpBps` per round without `confirmRounds` stable rounds | committee rule C5 (jump halt and confirmation) |
| One oracle signer (validator's feeder key) | sign any price | move the committee median outside the honest range while at most `(q-1)/2` signers lie | committee rule C3 (median of at least `q` signed observations) and C4 (agreement band) |
| The sequencer | choose which valid observations of a round to include (at least `q`), delay or withhold updates | forge or alter an observation (signed), replay an old round (C1), move the median outside the range of the honest observations it includes, or (alone) outside the order statistics of section 5a | C1 to C4, section 5a (R395-9: the exact reach of the choice, measured size, what detects it); withholding only halts the market (fail closed, `STALE`) |

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
| `price` | USD per one whole base unit, as an integer at the ENGINE's price scale, `PRICE_SCALE = 10^8` for every market (`docs/SPEC.md` section 1: 1 unit = 1e-8 USD), rounded half up from the exact value; never 0. NOT on the market's tick and NOT at its display `priceDecimals` (R432-1: the round-1 feeder used the display decimals, which the engine would have read 10^5 to 10^7 times too small; fixed with a failing-first test, `feeder/tests/r2.rs`) |
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
  "prices":[{"marketId":1,"price":"6499240000000","sources":7}],"signature":"0x…(65 bytes)"}]}
```

Canonical bytes (proposal for `inputsEncoded`; `docs/SPEC.md` decides the envelope and the input tag):

```
OracleUpdate      = round:u64be  count:u8  SignedObservation{count}
SignedObservation = signer:u8  timeMs:u64be  priceCount:u16be  OraclePrice{priceCount}  signature:65 bytes
OraclePrice       = marketId:u32be  price:u64be  sources:u8
```

No trailing bytes; `count`, `priceCount` and every ordering rule below are part of the canonical form.

## 4a. Ingest: from the feeders to the sequencer (agreed with 431, cross-area log 2026-11-18)

The node (431, `docs/API.md`) takes signed observations on an operator channel, not behind the geoblock and not
public:

- `POST /v1/oracle/observations`, body `{"round":R,"observation":{"signer","timeMs","prices","signature"}}`: ONE
  signer's observation. This is the production shape: each validator's feeder posts its own.
- `POST /v1/oracle/updates`, body = the whole `OracleUpdate` JSON of section 4. For a process that holds a whole
  DEV committee (the replay source of section 7b, `run --key-dir`); never a production shape.
- `Authorization: Bearer <token>`, the token read by both sides from a file (never an argument, never logged).
- JSON forms sent by this feeder: `round`, `signer`, `timeMs`, `marketId`, `sources` as JSON numbers; `price` as
  the canonical decimal string of the 1e-8 USD integer; `signature` as `0x` + 130 lowercase hex characters.
- The node only relays: it pre-filters (round of `timeMs`, signer index, signature recovers to the genesis oracle
  committee member under the engine domain), keeps one observation per `(round, signer)` (first wins, a different
  second one is refused and logged) and builds the block input from EVERY observation it holds for the round,
  signer ascending (never a subset: section 5a). The engine decides C1 to C5.

The feeder's client (`feeder/src/submit.rs`) speaks plain HTTP/1.1 to a LOOPBACK address only (`http://127.x.y.z`,
`http://[::1]`, `http://localhost`; anything else is refused before a byte is sent), never retries (the next round is
the retry: an observation is useless once its round has passed), and reads the token from a 0600 file. A feeder on
another host than its sequencer needs an authenticated and encrypted channel: not built (section 9).

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

## 5a. What the sequencer's choice of observations can do (R395-9)

The engine sees only the observations the sequencer included; with `n = 7`, `q = 5` the sequencer may leave out up
to two valid ones and nobody can tell from the block.

**Bound (exact, tested).** Let `x[0] <= ... <= x[n-1]` be ALL valid values of one market in one round (every signer
honest). Every subset of at least `q` of them has its C3 median between `x[q/2]` and `x[n-q+q/2]`, and both ends are
reached (drop the `n-q` highest, or the `n-q` lowest). Default committee: between the THIRD and the FIFTH lowest of
the seven; a sequencer that includes everything gets the fourth. So the choice moves the index by at most one order
statistic of the honest observations either way, per round, and it can lean the same way every round.
`core/src/selection.rs::selection_range` is this bound; the test `selection_range_is_exact` checks it against every
subset for every committee size up to 9 and every strict-majority quorum (more than 20,000 subsets, ties included).
With `f <= (q-1)/2` LYING signers colluding with the sequencer the reach is the whole range of the honest values it
includes (section 5, "why the median is safe"): still never outside the honest range.

**Size (measured, section 8a).** Honest feeders read the same venues in the same round, so their observations
differ only by when inside the 3 s round each one sampled and by which venues answered. The move of one feeder's
observation from one round to the next is an upper proxy for the spread between two feeders of the same round: in
the live hour of 2026-11-18 it was at the median below 1 bps and at the 99th percentile below 10 bps for the three
markets (exact figures in section 8a). The hard cap in a round that passes C4 is `agreeBps` (100 bps) from the
included values. For comparison, the mark may already differ from the index by `markBandBps = 50` (SPEC S-5.3) and
tier-0 maintenance is 250 bps or more: the bias cannot by itself liquidate an account that is not already within a
few bps of maintenance, but it is a free, repeatable edge for whoever orders the liquidations. It is stated here
and must be stated in SPEC section 12 (cross-area line to 430).

**What is implemented against it.**

1. The sequencer never selects: 431's ingest (section 4a) builds the update from every observation it holds. This
   makes an honest sequencer unbiased; it does not bind a dishonest one.
2. Detection: `core/src/selection.rs::omitted(update, round, held)` names the signers whose observation a
   validator KNOWS the sequencer received in time (its own, with the sequencer's 200 answer) and that the block
   left out. A validator that finds its own acknowledged observation missing has proof of selection.

**Why the round digest over the full observation set (a second signature of the committee over the set the
sequencer assembled) is NOT implemented.** A signer knows only its own observation. If each signer countersigns the
set only when its own observation is in it, a sequencer that leaves out two signers still collects the five
countersignatures of the signers it kept: the rule accepts exactly the subsets the present rule accepts. Any
threshold on countersignatures that an honest round with two signers offline can meet, a selected round with two
signers left out meets as well, because the engine cannot tell "offline" from "left out". A countersignature only
adds a round trip. It would help only if signers refused to countersign a set that misses observations OF OTHERS
that they have seen, which needs the observations to be broadcast among the committee (a network plane the node
does not have); with it, the simpler rule 3 below does the same job without a new signed message.

**What would close it (not built; for 431/430, each with its cost).**

3. Validators refuse the checkpoint over a block whose oracle update leaves out an observation they hold
   (`omitted` non-empty). With only their OWN observation known, leaving out two signers leaves exactly five
   validators willing to sign: the checkpoint threshold (5 of 7) is still met, so this alone does not prevent
   selection, it makes it attributable (the two omitted operators hold the sequencer's acknowledgements) and
   removes every liveness margin of the sequencer that does it. With observations gossiped among validators every
   honest validator refuses, and selection stops finality (prevention), at the price of a gossip plane and of one
   lost observation costing a checkpoint when the gossip and the sequencer disagree by timing.
4. A median that selection cannot push: accept as the committee price only the value closest to the previous index
   inside the interval of medians that ANY completion of the missing `n - |V|` observations could give
   (`[V[k-(n-|V|)], V[k]]`, `k = n/2`). Then leaving observations out can only make the index LAG (which the
   sequencer can already do by withholding), never lead. Not adopted: it changes consensus rule C3 (430's engine
   and the shared vectors), it makes two lying signers able to hold the index back by up to `agreeBps` whenever two
   others are missing (today they can only move it inside the honest range), and a flat price with two signers
   offline needs an extra freshness rule. The trade is not clearly safer; recorded for the next design round.
5. Requiring more observations (`|V| >= 6`) narrows the reach to `[x[3], x[4]]` but halts the market when two
   feeders are down: liveness for a bias of a few bps. Not proposed.

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

## 7a. A venue that fails: the drop rule, and batching (`feeder/src/health.rs`, `venues.rs`)

A venue that LIES is not dropped: its price stays in the round as an outlier (F4) where every report shows it.
The drop rule is about a venue that does not answer, answers with an error, or answers something its adapter
refuses; asking again every 3 s gains nothing and, when the venue is rate-limiting, makes it worse.

- D1 a fetch unit (one URL: one pair of one venue, or one batched request) FAILS in a round when the request fails
  (timeout after `fetchTimeoutMs`, TLS, any HTTP status other than 200) or every source reading that body is
  refused by its adapter. It is REFUSED when the status is 429, 418 or 403.
- D2 after `dropAfterFails = 5` consecutive failed rounds, or at once when refused, the unit is DROPPED: not
  requested for `cooldownRounds = 20` (60 s), doubling at each new drop without a success in between, at most
  `maxCooldownRounds = 1200` (1 h). After the cooldown one probe is sent; a failed probe drops again.
- D3 one success resets the failure count and the cooldown.
- A dropped unit's sources are `fetch-failed: dropped` readings for every round of the cooldown. `minSources` and
  the strict-majority rule F5 do not change: dropping can make the feeder abstain, never lower its bar.

Batching: the four Kraken pairs go in ONE request (`Ticker?pair=USDTUSD,XBTUSD,ETHUSD,SOLUSD`): 22 requests per
round instead of 25, one of them to Kraken. Each Kraken source reads its own pair out of the shared body; a body
that misses the pair, carries a pair nobody asked for, or carries a pair under both of its names is refused whole.

## 7b. The deterministic replay source (`feeder/src/tape.rs`, `replay.rs`; development and rehearsals only)

`perps-oracle replay-feed` plays a TAPE into the node's ingest as rounds signed by a dev committee, with no network
access beyond the node's loopback socket. Two kinds of tape, one format:

- recorded venue data: `perps-oracle tape --recording <rounds dir | FILE.jsonl>` runs the recorded venue responses
  through the real pipeline (adapters, F2 to F6, drop marks) and writes one row per round; `tapes/` holds the tape
  of the live hour of 2026-11-18 (section 8a);
- an authored script (a rehearsal's price path).

Format: one JSON object per line (`feeder/src/tape.rs` is normative, unknown fields refused). Prices are USD decimal
strings by market id, exact at 1e-8 USD (more fraction digits are refused, never rounded).

| Row | Meaning |
|---|---|
| `{"p":{"1":"67250.5","2":"3480.12"}}` | one round; a market not named HOLDS its last price; `"absent":[3]` removes market 3 until a later row prices it; `"repeat":N` plays the row N rounds |
| `{"ramp":{"1":"59180"},"rounds":N}` | N rounds in a straight line from the current price to the target: round k has `from + (to - from) * k / N`, the division truncated toward `from`; round N is exactly the target |
| `{"silent":N}` | N rounds with no submission (markets go STALE after `maxStalenessMs`) |
| on `p` and `ramp` rows | `"src":{"1":5}` (the signed `sources`, default 5), `"signers":[0,1,2,3,4]` (only these indices sign), `"override":{"6":{"1":"70000"}}` (signer 6 signs another price: a lying signer), `"note"` |

What is deterministic: the sequence of prices, sources, signers and overrides (a pure function of the tape and the
options; RFC 6979 signatures, so the same tape, keys and clock give the same bytes: tested). What is NOT: `round`
and `timeMs` are the wall clock of the run, because the engine accepts an observation only inside its own round
and near the block time (C2c). One tape step is played per 3 s round; `--speed K` plays every K-th step (the
engine's round cannot be compressed); `--loop` plays forward then backward for ever (no jump where the tape wraps).
A node that is down or refuses a round does not stop the feed (a sequencer restart in a rehearsal); the exit code is
0 at the end of the tape and the last line on stderr counts `accepted`, `refused`, `unreachable`, `silent` rounds
(`--strict`: exit 2 unless every round was accepted). A step whose round was lost (the process was stopped) is
played in the next round; never two updates in one round.

Keys: `perps-oracle committee-gen --n 7 --quorum 5 --out DIR` makes fresh random THROWAWAY keys at run time
(`/dev/urandom`, files 0600 in a new 0700 directory, a public `committee.json`); nothing is committed. For a node
started with its built-in dev genesis, `--node-dev-seed S` derives the node's own throwaway oracle keys (a public
derivation, pinned against Foundry's `cast` in the tests).

Ramps must stay under `jumpBps` per round or the market halts; that is also how a rehearsal makes a JUMP halt.
`tapes/rehearsal-example.jsonl` (a 12% BTC drop in 40 rounds, an ETH gap, two lying signers, a quorum-only round,
five silent rounds) is replayed through the engine rule round by round in `feeder/tests/r2.rs`.

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
| `dropAfterFails`, `cooldownRounds`, `maxCooldownRounds` | 5, 20, 1200 | feeder (section 7a) |
| `exitHaircutBps`, `exitHaircutPerMinBps`, `exitMaxHaircutBps`, `maxHaltMs` | `jumpBps`, 100, 5000, 3600000 | engine, per market (section 6a) |
| USDT/USD conversion: `minSources`, `maxDepegBps` | 3, 500 | feeder |

## 9. Not done / not proven

See the report `~/Desktop/POSTERNDEX-PERPS-20261117/agents/394-oracle.md` for the current list. In short: not
reviewed; parameters not calibrated on historical data; venue terms of use and most rate limits unconfirmed; the
feeder's submission to the sequencer and the node's ingest are not wired (389); the engine does not call `core` yet
(388); the price scale (10^priceDecimals per market vs a fixed 1e8) waits for SPEC.md.

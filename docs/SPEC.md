# PosternDEX Perpetuals: engine specification (`docs/SPEC.md`)

**UNAUDITED PROTOTYPE. Version 0.2.3, 2026-11-19. Owner: agent 430 (round 2; continues 350 and 388).** Nothing
here is deployed.
The perpetuals are not to be deployed before an external audit and a legal review.

This document is normative for `engine/` (the deterministic state machine), and is the interface that `node/`
(389, round 2: 431), `contracts/` (390; 433), `frontend/` (391; 434), the mock and the end-to-end stack (393; 435)
and the oracle (394; 432) build against. Changes are
requested in `~/Desktop/POSTERNDEX-PERPS-20261117/agents/CROSS-AREA.md`; every change bumps the version in the
table at the end. Rule identifiers (`S-…`, `E-…`, `I-…`) are stable: tests and reviews cite them.

Words: MUST / MUST NOT are requirements; "Decided (technical)" records a choice with its reason; "Open" is a
question not yet settled, with the v0.1 behaviour stated.

## 0. Principles

- S-0.1 Determinism. The engine is a pure function `(state, block) -> (state', outputs)`. No floating point, no
  wall clock, no randomness, no iteration over hash maps in an order that is not defined here, no dependence on the
  platform's word size or endianness. Two machines that apply the same ordered input log from the same genesis MUST
  reach byte-equal state and the same state root at every height.
- S-0.2 Integer arithmetic. Every quantity is an integer in a fixed scale (section 1). Every intermediate product
  is computed in `i128`/`u128` with checked operations; where a bound in section 1 does not guarantee that an
  operation fits, `U256`/`I256` is used. An arithmetic overflow while applying a USER input rejects that input
  (`OVERFLOW`) with no state change; an overflow in a SYSTEM step (funding, liquidation, mark) halts the market
  (`haltReason = ARITHMETIC`) and is a bug to be reported. The engine never wraps, saturates silently or panics on
  input data.
- S-0.3 Rounding toward the protocol. Every division whose result moves value to or from a user rounds so that the
  user receives at most and pays at least the exact rational amount. The residual is credited to the insurance
  fund's dust accumulator (section 8), so value is never created and never lost (invariant I-2).
- S-0.4 Fail closed. Unknown fields, unknown tags, trailing bytes, non-canonical encodings, out-of-range values and
  stale oracle data are rejected; they are never coerced.
- S-0.5 The sequencer is not trusted for content. Every user action carries the user's (or the user's approved
  agent's) signature, verified by the engine itself during `apply` (not only by the node's admission), so a
  validator replaying the log rejects a forged, altered or replayed action deterministically. The sequencer's
  remaining power (ordering, delay, censorship) is stated in section 12.

## 1. Numeric scales and bounds

Decided (technical): ONE fixed scale per kind of quantity, the same for every market. Reason: a signature over a
price or a size must mean the same thing whatever an API says about a market; a per-market `priceDecimals` read
from a server could make a user sign a price ten times off (391's signing-safety rule 2). Per-market granularity is
expressed by tick and step sizes in these fixed scales, not by different decimals. `priceDecimals` and
`sizeDecimals` in `markets.json` remain DISPLAY precision only and MUST equal the precision implied by the tick and
step (checked at genesis).

| Quantity | Type | Scale (1 unit =) | Bound (checked) |
|---|---|---|---|
| price (USD per 1 whole base unit) | u64 | 1e-8 USD (`PRICE_SCALE = 10^8`) | `1 <= price <= MAX_PRICE = 10^15` (10,000,000 USD) |
| size (base units) | u64 / position i64 | 1e-8 base (`SIZE_SCALE = 10^8`) | `size <= MAX_SIZE = 10^15` per order and per position |
| collateral (USDC, bUSDC) | u64 balances / i64 signed | 1e-6 USDC (`USDC_SCALE = 10^6`) | account balance `<= 2^62` |
| notional (price x size) | i128 | 1e-16 USD (`NOTIONAL_SCALE = 10^16`) | `|notional| <= 10^30` |
| fee and margin rates | u32 | 1e-6 (ppm) | `<= 10^6` |
| funding rate, premium | i64 | 1e-9 (`RATE_SCALE = 10^9`) | `|rate| <= maxFundingRate` |
| time | u64 | 1 ms (Unix milliseconds, engine time = block time) | strictly increasing per block |

- S-1.1 Conversion notional -> USDC: `usdc = notional / 10^10` (`NOTIONAL_SCALE / USDC_SCALE`), rounded per S-0.3.
- S-1.2 Human decimal strings (API, `markets.json`) are converted with exact decimal parsing: a value that is not
  exactly representable in the scale (more fractional digits than the scale) is REJECTED, never rounded.
- S-1.3 Worked bound: `MAX_PRICE * MAX_SIZE = 10^30 < 2^127 ~ 1.7e38`; genesis bounds `maxFundingRate <= 10^7`
  (1% per interval), so funding's `size * mark * rate <= 10^37` also fits i128 (section 7).

## 2. Signing: EIP-712 domain and action types

### 2.1 Domain

Decided (technical), a change to BRIEF-PERPS's provisional domain (reason below; coordinator and 391/389 notified
in the cross-area log):

```
EIP712Domain(string name,string version,uint256 chainId,address verifyingContract)
name = "PosternPerps", version = "1", chainId = 700770, verifyingContract = the PerpsVault address on 700770
```

Reasons: (a) browser wallets (MetaMask's `eth_signTypedData_v4` among them) refuse typed data whose domain
`chainId` differs from the wallet's active chain, so a domain on the non-existent chain 7007701 would force every
user to add a fake network; 700770 is the chain the user is already on to deposit. (b) `verifyingContract` = the
vault binds every signature to ONE deployment: an action signed for a test or staging engine (different vault)
cannot be replayed on production, and vice versa; with `chainId` alone every deployment would share one domain.
(c) The vault's own domain is `PosternPerpsVault` (VAULT.md), so no engine action is a valid vault message and
no vault message is a valid engine action (different `name`, hence different domain separator).

The engine takes `verifyingContract` from genesis (section 10). In development the vault address of the local
deployment is used; fixtures state theirs.

### 2.2 Signature format (S-2.2)

65 bytes `r || s || v`; `v` in `{27, 28}`; `1 <= r < n`, `1 <= s <= n/2` (low-s; `n` the secp256k1 order);
digest `keccak256(0x19 0x01 || domainSeparator || hashStruct(message))`; the recovered address MUST be
`message.account` or an agent of `message.account` that is approved, unexpired at the block time and allowed for
the type (2.4). Any other form is `BAD_SIGNATURE`. No EIP-1271 (contract) signers in v0.1; no EIP-2098 compact
signatures.

`actionHash = keccak256(digest || signer)` (32 + 20 bytes), the identifier of an applied action (an account and
its agent may sign identical messages; the signer separates them).

### 2.3 Types (v0.1, field order is normative)

```
PlaceOrder(address account,uint64 nonce,uint32 marketId,bool isBuy,uint8 orderType,uint8 timeInForce,uint64 price,uint64 size,bool reduceOnly,uint64 triggerPrice,bytes32 clientOrderId)
CancelOrder(address account,uint64 nonce,uint32 marketId,uint64 orderId)
CancelAll(address account,uint64 nonce,uint32 marketId)
UpdateLeverage(address account,uint64 nonce,uint32 marketId,uint32 leverage,bool isolated)
UpdateMargin(address account,uint64 nonce,uint32 marketId,uint64 amount,bool add)
Withdraw(address account,uint64 nonce,uint64 amount,address recipient)
ApproveAgent(address account,uint64 nonce,address agent,string name,uint64 expiresAt)
RevokeAgent(address account,uint64 nonce,address agent)
```

Field rules:

| Field | Rule |
|---|---|
| `nonce` | Unix ms chosen by the signer; section 2.5 |
| `marketId` | a genesis market id `>= 1`; in `CancelAll`, `0` = every market |
| `orderType` | `0` limit, `1` market, `2` stopMarket, `3` stopLimit, `4` takeProfitMarket, `5` takeProfitLimit; other values `MALFORMED` |
| `timeInForce` | `0` GTC, `1` IOC, `2` POST_ONLY (ALO); market and *Market types MUST be IOC |
| `price` | MANDATORY, `>= 1`, a multiple of the market's `tick`: the limit price for limit types; for market types the WORST acceptable price (a buy never fills above it, a sell never below it). There is no "any price" order: `0` is `MALFORMED`. (Agreed with 391 and 389.) |
| `size` | `>= minSize`, `<= maxSize`, a multiple of `step` |
| `triggerPrice` | `0` for types 0 and 1; `>= 1` and a multiple of `tick` for types 2 to 5 |
| `clientOrderId` | opaque, echoed in events; `0x00…00` allowed; not unique-checked in v0.1 |
| `leverage` | integer `1 <= leverage <= maxLeverage` of the market's first tier |
| `UpdateMargin.amount` | USDC units; moves collateral into (`add = true`) or out of (`false`) an ISOLATED position's margin. Decided (technical): `uint64` + `bool` rather than `int64`, so a wallet shows a direction word and an unsigned amount (no two's-complement reading) |
| `Withdraw.amount` | USDC units, gross; the flat `withdrawalFee` is deducted from it (section 9.2); `recipient` any address the vault can pay: not the zero address (`MALFORMED`), and none of the addresses of S-9.2.1 (`BAD_RECIPIENT`, R395-22); shown by the wallet |
| `ApproveAgent.name` | UTF-8, 0 to 32 bytes; `expiresAt` Unix ms, `T < expiresAt <= T + 180 days` |

Reserved type names (S-2.3.1): `Attestation` (the user attestation of 392/397, `compliance/legal/notices.json`,
signed under THIS domain), `Checkpoint` and `OracleObservation`/`OraclePrice` (oracle, section 5) are not
actions: no input `typeId` maps to them (an unknown `typeId` is `MALFORMED` at decode), and a signature over any
other type never verifies as an action because the digest covers the type hash (test
`attestation_signature_is_never_an_action`). Any new type in this domain MUST get a name distinct from all of
these and be added to this list.

### 2.4 Who may sign what (S-2.4)

| Type | Main key | Agent |
|---|---|---|
| PlaceOrder, CancelOrder, CancelAll, UpdateLeverage, UpdateMargin | yes | yes |
| Withdraw, ApproveAgent, RevokeAgent | yes | NO (`AGENT_NOT_ALLOWED`) |

At most `MAX_AGENTS = 4` unexpired agents per account. An agent address MUST NOT be an existing account (an
address with a ledger entry), MUST NOT be an agent of another account, and MUST NOT equal `account`
(`AGENT_INVALID`). Approving an existing agent address of the same account replaces its name and expiry.
Expired agents are removed lazily (at the next action of the account) and count for nothing.

- S-2.4.1 (R395-21, the rule). Accounts appear by deposit and anyone may deposit to any address, so "an agent is not
  an account" is checked ONLY when an `ApproveAgent` is applied; it is not an invariant, and no code may assume it.
  A deposit to an approved agent's address makes that address an account as well: the approval stays valid until
  it expires or is revoked, and nothing is confused, because the signer is always resolved through
  `message.account` (S-2.2): the address acts for itself with `account` = itself (every type) and for its principal
  with `account` = the principal (agent types only; `Withdraw` for the principal stays `AGENT_NOT_ALLOWED`). The
  cost a third party can impose with such a deposit: a later approval OR RENEWAL of that address by anyone is
  `AGENT_INVALID`, so the user approves a fresh key; the existing approval is not cut short. Test
  `r395_21_deposit_to_an_agent_address`.
- S-2.4.2 (R395-20). Nonce sets (2.5) belong to the SIGNER address, not to the approval: they are never deleted
  or reset when an agent is revoked, expires, is removed lazily or is approved again (by the same or by another
  account). An action of an agent applied before its removal therefore stays single-use after any re-approval
  (`NONCE_USED`). v0.2 keeps every signer's set for the whole history (a stronger rule than "at least the nonce
  window"); the cost is state that only grows: at most 64 nonces per address that ever signed an applied action
  (O-11). Test `r395_20_agent_nonces_survive_revoke_expiry_and_reapproval`.

### 2.5 Nonces (S-2.5)

Decided (technical), extending 389's proposal: each signer (main key or agent, separately) has a set `used` of at
most `NONCE_SET = 64` of its highest applied nonces. An action with nonce `x` at block time `T` is valid only if
(a) `T - 120000 <= x <= T + 30000` (the nonce is a millisecond time inside the window), (b) `x` is not in `used`,
and (c) if `used` is full, `x > min(used)`. On success `x` is inserted (evicting the minimum when full).
Reasons: (a) bounds the sequencer's power to hold a signed order and execute it later to two minutes; (b)+(c) make
every signature single-use while allowing up to 64 actions in flight to arrive out of order (a strictly increasing
rule would reject a valid order merely overtaken by the next one). A client that uses `max(clock_ms, last + 1)`
(391's rule) always satisfies the rule. Errors: `NONCE_WINDOW`, `NONCE_USED`. The nonce is CONSUMED as soon as
the signature and the nonce are valid, whatever the action's outcome (a failed order cannot be re-played later by
the sequencer when it would succeed).

## 3. Blocks, inputs and the log

### 3.1 Block (agreed with 389's proposal A)

`Block { height: u64, time: u64, inputs: [Input] }`; `height` starts at 1 and increases by exactly one; `time` (ms)
strictly increases. The engine never checks `time` against a clock (validators refuse to sign a checkpoint for a
block whose time is implausible against their own clock: node's rule). At most `MAX_INPUTS = 10000` inputs and
`MAX_BLOCK_BYTES = 4 MiB`.

```
logHash(0) = keccak256("PosternPerps/genesis/v1" || genesisEncoding)
logHash(h) = keccak256(logHash(h-1) || be64(h) || be64(time) || keccak256(inputsEncoded))
```

### 3.2 Inputs and canonical encoding

`inputsEncoded = be32(count) || Input{count}`; `Input = tag:u8 || be32(len) || payload[len]`. Inputs MUST appear in
tag order: all `0x01` (at most one), then `0x02`, then `0x03`, then `0x04`; violation rejects the BLOCK
(`BLOCK_ORDER`). An invalid individual input (bad signature, bad nonce, insufficient margin…) does NOT reject the
block: it is recorded as a failed outcome with its error and has no other effect.

- S-3.2.0 (R436-4, 0.2.3) Two kinds of defect, and only two. (1) Bytes that are not a canonical encoding at all
  (short input, trailing bytes, a length that runs past the payload, a boolean that is not 0 or 1, an unknown tag
  or action type, a string that is not UTF-8, a re-encoding that differs): the input LIST does not decode and such
  a block cannot exist; a sequencer must never produce one, so the node MUST run the engine's own decoder
  (`codec::decode_payload`) on every action BEFORE it admits it and drop what it refuses. (2) Everything else is a
  rule on a decoded input and fails THAT input only: in particular an `ApproveAgent.name` longer than 32 bytes
  decodes (the wire carries any `be16` length) and fails `MALFORMED`. At `f0e3967` the decoder refused the whole
  list for a 33-byte name: one signed request from any key halted the sequencer (test
  `r436_4_a_33_byte_agent_name_fails_alone_and_never_rejects_the_block` fails there).

| Tag | Kind | Payload |
|---|---|---|
| `0x01` | oracle | `OracleUpdate` exactly as 394's `node/oracle/README.md` section 4 (canonical bytes) |
| `0x02` | deposit | `be64(depositIndex) || account:20 || be64(amount)` from the vault's `Deposit` event |
| `0x03` | forced | `be64(forcedIndex) || account:20 || kind:u8 || be64(deadline) || be64(arg)` = the fields of VAULT.md 0.2's `ForcedRequest(uint64 index, address account, uint8 kind, uint64 deadline, uint64 arg, bytes32 head)` (`deadline` in L2 `block.timestamp` seconds) |
| `0x04` | action | `typeId:u8 || fields || signature:65`; `typeId` = 1 PlaceOrder, 2 CancelOrder, 3 CancelAll, 4 UpdateLeverage, 5 UpdateMargin, 6 Withdraw, 7 ApproveAgent, 8 RevokeAgent; `fields` = the message's fields in the type's order, each fixed width big-endian (`address` 20, `uint64` 8, `uint32` 4, `uint8` 1, `bool` 1 byte 0 or 1 only, `bytes32` 32, `string` `be16(len) || utf8`); the engine recomputes the EIP-712 digest from the decoded fields |

- S-3.2.1 Deposits MUST be consecutive: `depositIndex == nextDepositIndex` (starting at 0), else the input fails
  `DEPOSIT_ORDER` and the node must re-submit (the vault index is the single order). Likewise forced requests with
  `nextForcedIndex` (a forced request is RECEIVED in index order; when it is PROCESSED is section 14).
  Deposits and forced requests come from L2 events at the `finalized` tag agreed by two endpoints (node's rule), but
  their correctness does NOT rest on those reads (R395-1): the engine itself computes the vault's chains (VAULT.md
  5.4) from the inputs it APPLIES,
  `depositsHead(0) = keccak256(abi.encode(bytes32("PosternPerps.deposit.v1"), uint256(700770), address(vault)))`,
  `depositsHead(i+1) = keccak256(abi.encode(depositsHead(i), uint256(i), address(account), uint256(amount)))` per
  credited deposit, and `forcedHead` likewise over PROCESSED forced requests (`abi.encode(head, index, account,
  kind, deadline, arg)`, each `uint256`/`address` word, tag `"PosternPerps.forced.v1"`; VAULT.md 0.2 5.4). Validators sign `depositsCredited`,
  `depositsHead`, `forcedProcessed`, `forcedHead` as the engine computed them (`Engine::checkpoint_fields`), NEVER
  a value copied from an L2 read; the vault refuses a checkpoint whose head differs from its own stored head at
  that count. A fabricated, altered, reordered or skipped deposit therefore yields a head the vault refuses, even if
  every validator's L2 view was wrong (test `r395_1_…`).
- S-3.2.2 JSON form (API, mock, fixtures): the same fields, integers as decimal strings, bytes as `0x` hex.

### 3.3 Order of processing inside a block (normative)

1. Header checks (3.1). `T = block.time`.
2. For each market in ascending id: oracle staleness rule H1 (394 section 6).
3. Oracle input, if present: 394 rules C1 to C5 and H2/H3.
4. Mark price of every market (section 5) from index and the book as it stands.
   (5b, after funding: X3 settlement of markets halted for `maxHaltMs`, section 14.)
5. Funding (section 7) for every market whose funding boundary was crossed (`T / fundingIntervalMs > prevT / fundingIntervalMs`).
6. Premium sample for every active market if an oracle round was accepted in step 3.
7. Deposits, then forced requests, then actions, each in input order.
8. Mark price recomputed (section 5).
9. Triggers (section 4.6), then liquidations (section 6), repeated until no trigger fires and no account is
   liquidatable, bounded by `MAX_SYSTEM_PASSES = 8` (remaining work carries to the next block). Property P-3
   (section 17): after a block whose step 9 ran to completion, no account whose markets are all active is below its
   maintenance requirement; an account can end a block below maintenance only when the pass bound was hit (the
   engine exposes this per block as the diagnostic `last_passes`, not state) or while one of its markets is halted
   (6.3).
   (9b, 0.2: whole units of dust move into `INSURANCE`, THEN bad debt of flat accounts is paid by `INSURANCE`,
   section 8. The dust sweep comes first so that no flat account stays negative at the end of a block while the
   fund holds anything.)
10. Protocol fee sweep (section 9.3).
11. State root (section 11).

## 4. Order book and matching

- S-4.1 One book per market; price-time priority; `orderId` is a global u64 counter starting at 1, assigned when an
  order is accepted (triggers get theirs when stored).
- S-4.2 Limit GTC rests the remainder; IOC cancels the remainder; POST_ONLY is rejected (`WOULD_CROSS`) if it would
  match on arrival (it never matches as taker).
- S-4.3 Market orders are IOC with the signed worst price; the remainder is cancelled (`NO_LIQUIDITY` in the event
  when nothing filled). A market order also never fills beyond `marketSlippageBps` from the mark (default 500 bps
  = 5%): the effective bound is the tighter of the two.
- S-4.4 Fill price = the resting (maker) order's price. Self-trade prevention: when the taker meets a resting order
  of the SAME account, the resting order is cancelled (`SELF_TRADE`) and matching continues.
- S-4.5 Reduce-only: the order's executable size is clipped to the size that reduces the current position
  (never flips or increases it); a reduce-only order with nothing to reduce is rejected (`REDUCE_ONLY`); resting
  reduce-only orders are clipped or cancelled when the position shrinks.
- S-4.6 Triggers (types 2 to 5): stored outside the book. Stop (2, 3): a sell triggers when `mark <= triggerPrice`,
  a buy when `mark >= triggerPrice`. Take-profit (4, 5): a sell triggers when `mark >= triggerPrice`, a buy when
  `mark <= triggerPrice`. When triggered (step 9), the order enters the book as market IOC (2, 4) or limit GTC (3,
  5) with its signed `price`, in order: market id ascending, then `orderId` ascending. Triggers on a halted market do
  not fire.
- S-4.7 Limits per account: `MAX_OPEN_ORDERS = 200` resting plus triggers per market, `1000` in total
  (`TOO_MANY_ORDERS`).
- S-4.8 Open interest cap: `OI(m) = sum of long sizes`. A fill that would increase `OI(m)` above `oiCap(m)` is not
  executed; the taker's remainder is cancelled (`OI_CAP`).
- S-4.9 Margin at placement (cross): an order that can rest (GTC, POST_ONLY) is accepted only if, counting it as
  resting, the account's initial requirement including open orders (section 6.2) is covered by its equity MINUS
  what a fill of the whole order at its own price would lose against the mark (`max(0, price - mark) * size` for
  a buy, `max(0, mark - price) * size` for a sell; 0.2.3: an order priced through the mark was reserved on the
  mark's notional only, with nothing for the loss its fill realises); a reduce-only order that can rest is accepted
  only if a fill of its reducible size at its price passes S-4.9.1; else `INSUFFICIENT_MARGIN`. A fill as taker
  re-checks per fill: the taker stops (remainder cancelled, `INSUFFICIENT_MARGIN`) before a fill that would leave
  its equity below its initial requirement; a fill that only reduces is checked by S-4.9.1 instead.
- S-4.9.1 Maintenance at every fill (R436-1 High, 0.2.3; replaces "a fill as maker is never refused for margin").
  A book fill is executed only if EACH side, after it, holds its maintenance requirement at the mark:
  `E_after >= MR_after`, with `E_after = E + d * (mark - price) - fee - 1 unit` for the side's signed size change
  `d`. Exception, so that an account below maintenance can still get out: a fill that only REDUCES the side's
  position is also allowed when it does not worsen the margin deficit (`E_after - MR_after >= E - MR`). A taker
  that fails stops (`INSUFFICIENT_MARGIN`); a MAKER order that fails is cancelled (`INSUFFICIENT_MARGIN`) and
  matching continues with the next order. Applies to makers in every mode (ordinary orders, liquidations, forced
  closes) and to takers of ordinary orders; liquidation and forced-close takers are bounded by their own price
  rules, which never pass the bankruptcy price (6.3, section 14). Consequence: no fill can by itself create a
  deficit, so the insurance fund pays only for moves of the MARK (bounded by the oracle rules and the mark band),
  never for a price two accounts agree on. At `f0e3967` one maker fill at half the mark moved 31,630 USDC of the
  fund to a colluding taker, a reducing taker fill did the same through bad debt, and so did a forced close into a
  colluder's bid (tests `r436_1_*`, four, all fail there). Residual, stated: an order that was backed when placed
  can lose its backing when the mark moves; it then sits in the book until a taker reaches it and is cancelled at
  that moment, never filled (it can be counted by one premium sample or one mark before that: section 12).
- S-4.12 Price band for limit orders (R436-1, 0.2.3). A limit order (types 0, 3, 5; whatever its time in force;
  type 3 and 5 when they fire) whose price is through the mark by more than `marketSlippageBps` (a buy above
  `mark + band`, a sell below `mark - band`, `band = floor(mark * marketSlippageBps / 10^4)`) is refused
  `PRICE_BAND`: the bound a market order is clamped to (S-4.3) holds for every order. The passive side is not
  limited (a bid far below or an ask far above the mark may rest). A client that wants to cross sends a market
  order or a limit inside the band. Test `s_4_12_price_band_and_backed_resting_orders`.
- S-4.10 A halted market accepts only CancelOrder, CancelAll and nothing else (`MARKET_HALTED`); no liquidation,
  no triggers, no funding sample, and no matching EXCEPT a forced close's reduce-only take inside the exit band
  (section 14; makers fill at their own prices). Withdrawals during a halt: S-9.2. Genesis per market `oracle`
  adds `exitHaircutBps` (`>= jumpBps`), `exitHaircutPerMinBps`, `exitMaxHaircutBps` (`< 10000`), `maxHaltMs`
  (`>= maxStalenessMs`); the converter uses 394's defaults (jumpBps, 100, 5000, 3600000) when absent. The oracle
  state committed in `auxHash` adds `haltLow`, `haltHigh`. The engine's copy of the rules reproduces 394's shared
  vectors `node/oracle/vectors/halt.json` (17 blocks: every H1, outcome and exit band) and `eip712.json` (domain,
  struct hashes, digests, signers, canonical OracleUpdate bytes): tests `tests/r394.rs`.
- S-4.11 Launch gate (392's request): no PlaceOrder is accepted (`NOT_LAUNCHED`) until `INSURANCE.balance >=
  launchInsuranceMin` and `BACKSTOP.balance >= launchBackstopMin` (genesis; seeds are deposits to the system
  addresses `0x…02` and `0x…03` through the vault). The check runs at each PlaceOrder and LATCHES open once
  (event `Launched`): a later loss of the insurance fund does not close trading (closing it would trap users in
  positions; the fund's level after launch is an operations alert, not a rule). Cancels, withdrawals and deposits
  are never gated.

## 5. Index and mark price

- S-5.1 Index = the committee oracle price of 394's rules (median of validator-signed observations, agreement band,
  jump halt and confirmation). The engine applies those rules exactly as 394's README sections 4 to 6 state them
  (normative by reference for v0.1; the vectors in `node/oracle/vectors/` are shared test vectors).
- S-5.1.1 (R430-2, 0.2; an engine bound on top of 394's rules, fail closed). A committee value above `MAX_PRICE`
  (section 1) is not a price of this engine: that observation does not count for the market (exactly as an
  observation with too few sources), so a round in which fewer than the quorum report a price `<= MAX_PRICE` gives
  no committee price for the market, and the market goes stale and halts (H1). Reason: every product of section 1
  is bounded through `MAX_PRICE`; at `f0e3967` the mark was clamped but the INDEX, the exit band and the settle
  price were not. The update itself is still accepted (its form and signatures are valid; 394's `max-values`
  vector decodes and is accepted). Test `r430_2_committee_price_above_max_price_never_becomes_the_index` (fails on
  `f0e3967`).
- S-5.1.2 (R436-12, 0.2.3; a change to 394's README rule that one invalid observation refuses the update; 432
  notified). An `OracleUpdate` has two kinds of rule. UPDATE rules, on what the sequencer assembles: the round is
  new, the observation count is between the quorum and `n`, signer indexes are valid and strictly ascending; a
  defect refuses the update (`ORACLE_ROUND_NOT_NEW`, `ORACLE_MALFORMED`). OBSERVATION rules, on what ONE signer
  authors: its price list (1 to 256 entries, market ids strictly ascending, no zero price), its signature, its
  `timeMs` inside the round and inside the block-time window; an observation that fails its own rule is DROPPED and
  the update is judged on the rest. The update is refused only when fewer than the quorum remain, with the first
  dropped observation's error. Reason: the quorum is sized to tolerate `n - q` faulty signers; at `f0e3967` one
  signer (or one feeder clock a second fast) refused every round and every market went stale (test
  `r436_12_one_bad_observation_does_not_veto_the_round` fails there). A dropped observation leaves no trace in the
  state; the round counts as consumed only when the update is accepted.
- S-5.2 Mark (bounded book component):
  `bookRef = median(bestBid, bestAsk, lastTradePrice)` using, for a missing value, the index (and for
  `lastTradePrice` older than `markTradeMaxAgeMs = 60000`, the index);
  `band = index * markBandBps / 10000` (rounded down);
  `mark = index + clamp(bookRef - index, -band, +band)`. Default `markBandBps = 50` (0.5%).
- S-5.3 Manipulation bound (stated): whoever controls the book can move the mark at most `markBandBps` from the
  index; therefore every tier's maintenance fraction MUST exceed `markBandBps` (checked at genesis), and an account
  whose equity is within `markBandBps` of its maintenance requirement can be pushed into liquidation by book
  activity. The index itself is bounded only by the oracle rules.

## 6. Accounts, margin and liquidation

### 6.1 State

`Account { index: u32 (0 to 2 the system accounts of section 8; users from 3, assigned in order of first credit),
address, balance: i128 (USDC units, cross collateral; signed: a negative balance is bad debt, section 8),
positions: map marketId -> Position (iterated in ascending marketId), orders, triggers, agents, nonce sets }`.

`Position { size: i64 (long > 0), openNotional: i128 (signed, notional units: sum of signed size x price of the
open part), leverage: u32, isolated: bool, isoMargin: u64, lastFundingIndex: i128 }`.

### 6.2 Value and requirements (cross)

- `U(p) = size * mark - openNotional` (notional units), per position.
- Equity `E = balance * 10^10 + sum over cross positions of U` (isolated positions are excluded; their margin is
  not part of `balance`).
- Maintenance `MR = sum over cross positions of mmr(|size| * mark)` with the CONTINUOUS piecewise schedule
  `mmr(n) = sum over tiers t of (part of n inside [floor_t, floor_{t+1})) * mmf_t / 10^6`, rounded UP. (Decided:
  continuous, so the requirement never jumps when a position crosses a tier floor.)
- Initial `IR` the same with `imf_t' = max(imf_t, ceil(10^6 / leverage))` (R395-15: the leverage fraction rounds
  UP, toward the protocol: leverage 3 is 333334 ppm, never 333333; in the code since `647a401`, pinned by
  `r395_15_leverage_fraction_rounds_up`), rounded up, and including resting
  orders: per market on `max(|size| * mark, n_buy, n_sell)` with `n_buy = |size + bids| * max(mark, highest bid
  price)` and `n_sell = |size - asks| * max(mark, highest ask price)`. Reduce-only orders and untriggered trigger
  orders reserve nothing (a trigger is margin-checked when it fires). `leverage` is the account's setting for the
  market, default the market's genesis `defaultLeverage`.
- Liquidatable: `E < MR` (strict). A withdrawal, or a margin move out of the cross balance, of `a` USDC units is
  allowed only if `balance >= a` and, after the debit, `E >= IR` (unrealised profit counts toward `E` but can never
  be withdrawn beyond `balance`).
- Isolated: the same formulas per position with `isoMargin * 10^10` in place of the balance term.

### 6.3 Liquidation (order, normative for v0.1)

For a liquidatable cross account (accounts in ascending `index`; isolated positions are liquidated alone the same
way). v0.1: an account with a position in a HALTED market is not liquidated until the market resumes (fail closed:
no liquidation on a price the oracle rules do not stand behind; the insurance fund carries the gap). Liquidation
fills pay no taker fee (step 4's fee replaces it); makers pay their normal fee.

1. Cancel all its resting orders and triggers (all markets).
2. For each position (ascending market id): bankruptcy price `bp` = the price at which `E` would be 0 holding the
   other positions at mark (rounded toward the account: up for a long, down for a short). R395-17 (b), stated:
   `bp` is RECOMPUTED for each position, in ascending market id, from the account as it stands after the previous
   positions were closed: its balance then includes the realised result of those closes (at their fill prices,
   floored, S-0.3), the insurance cover of step 5 and MINUS the liquidation fee of step 4 already taken for them;
   it is never the value computed at the start of the liquidation. `bp = ceil((openNotional - rest) / size)` for a
   long, `floor` for a short, `rest = balance * 10^10 + sum of U of the OTHER open positions at mark`, clamped to
   `[1, MAX_PRICE]`. Test `r395_17_bankruptcy_price_is_recomputed_between_positions` rebuilds the second price
   from the block's events. Liquidation price bound (R395-13) `lp = max(bp, mark - mark *
   mmf_0 / 2)` for a long, `min(bp, mark + mark * mmf_0 / 2)` for a short (`mmf_0` = tier-0 maintenance fraction).
   Send a reduce-only IOC to the book for the whole position with limit `lp`, which never fills against an order
   placed in the SAME block (the sequencer knows which accounts step 9 liquidates; its own last-moment order is
   not liquidity): such orders are SKIPPED and the older orders behind them, inside `lp`, are still used in
   price-time order (R436-2, 0.2.3; at `f0e3967` matching stopped at the first one, so one fresh minimum-size order
   at the top of the book hid all older liquidity from the block's liquidations; test `r436_2_*`). Stated bound: a liquidated account loses at most
   `mmf_0 / 2` of the notional beyond mark plus the liquidation fee; the rest of its equity stays its own.
3. The remainder is taken by the backstop vault (system account `BACKSTOP`) at `bp` if the backstop has a positive
   balance and its equity after it covers its own maintenance requirement, and only up to its position limit
   `|backstop position| <= oiCap * backstopOiPpm / 10^6` (genesis; 392's proposal 25% = 250000; closing an
   opposite backstop position is not limited), at `lp` (not at `bp`); whatever remains goes to auto-deleveraging
   (6.4) at `lp` (at `bp` in the gap case of step 5). The backstop is never liquidated, never its own
   counterparty, stops taking when its equity after would fall below its maintenance requirement (R395-14), and
   is the FIRST ADL counterparty on the side opposite to its position (and a forced close meets it through step
   3's rule), which is how its positions are reduced in 0.1.5 (resting unwind orders: O-9).
4. Liquidation fee: `liquidationFeePpm` of the liquidated notional, taken from what equity remains (never below 0),
   credited to the insurance fund (`INSURANCE`).
5. If `E < 0` at the start (a gap: the oracle moved past bankruptcy), `INSURANCE` first pays the deficit AS FAR AS
   ITS BALANCE GOES (R395-31: partial cover too); if that restores `E >= 0` the liquidation continues with steps 2
   and 3; otherwise the positions are closed by ADL at the bankruptcy price (no deficit is ever socialised outside
   ADL). R395-17 (c), stated: `INSURANCE` pays `min(its balance, the deficit)` and NEVER goes below zero (I-3); the
   same holds for bad debt (section 8) and for the isolated-funding shortfall of S-7.3 (when isolated margin
   exists, O-6): what the fund cannot pay is deleveraged (here) or stays a recorded negative balance (section 8),
   and shows in the escape values (section 11). Test `r395_17_insurance_never_goes_below_zero`.

### 6.4 Auto-deleveraging (last resort)

Counterparties are ranked by `profitRatio * leverage` (`U / (|size| * mark)` times `|size| * mark / E_account`, in
ppm, integer, ties by ascending account index) on the opposite side of the same market; their positions are
reduced at the bankruptcy price in that order until the remainder is gone. Every ADL fill is an event naming both
accounts.

The rank, exactly (R395-17 (a), `engine::adl_score`): `score = U * 10^6 / E` rounded toward zero, with `U` the
position's unrealised result at mark and `E` the account's whole equity at mark, DEFINED ONLY FOR `E > 0`. An
account with `E <= 0` has no profit to give up: it is ranked LAST, after every account with positive equity
whatever its `U` (score = minus infinity), and several such accounts go by ascending index. `BACKSTOP` is ranked
FIRST whatever its equity (R395-14). Higher score first; equal scores by ascending account index. `PROTOCOL_FEE`
and `INSURANCE` hold no positions and are never candidates. Within section 1's bounds `|U| * 10^6 <= 2 * 10^36`
fits i128. Tests `r395_17_adl_score_rule`, `r395_17_adl_ranks_non_positive_equity_last`. Not proven: that this
ranking is fair in any stronger sense than "most profitable and most leveraged first".

## 7. Funding

- S-7.1 Premium sample (taken at step 8 of 3.3, on the book AFTER the block's actions, when an oracle round was
  accepted in the block; R395-16): `impactBid/impactAsk` = the average fill price of a `impactNotional`
  market order on each side (default 10,000 USD), index when the side is too thin; `premium = (max(0, impactBid -
  index) - max(0, index - impactAsk)) * RATE_SCALE / index` (rounded toward zero).
- S-7.2 At each funding boundary (`fundingIntervalMs = 3600000`): `P = the MEDIAN of the samples` of the interval
  (lower middle; no samples -> no funding for that interval; R395-16: a minority of manipulated samples cannot
  move it); `rate = clamp(P + clamp(interest - P, -c, +c),
  -maxFundingRate, +maxFundingRate)` where `c` is the market's genesis `fundingClamp` (per market, 392's request;
  `markets.json` 0.2.1: 62500 = 0.00625% per hour = the benchmark's 0.05% per 8 h, tighter than the 0.1 default of
  500000). Values of 0.2.1: `interest = 12500` (0.00125% per hour, i.e. 0.01% per 8 h), `maxFundingRate = 500000`
  (0.05% per hour; raising it waits for R395-16, the premium sample a sequencer can quote into).
- S-7.3 Payment per position: `f = size * mark * rate / RATE_SCALE` in notional units, computed in checked i128
  (S-1.3; an overflow anyway halts the market with `ARITHMETIC`); a long pays
  when `rate > 0`. Payers' amounts rounded up, receivers' down (S-0.3); converted to USDC per S-1.1 with the same
  direction; the sum of the residuals goes to dust. Funding is settled into the balance (cross) or `isoMargin`
  (isolated) immediately; an isolated margin that would go below 0 is floored at 0 and the shortfall is a loss of
  the insurance fund (recorded).

## 8. System accounts and conservation

- System accounts (no key, never sign): `PROTOCOL_FEE` (address `0x…0001`), `INSURANCE` (`0x…0002`), `BACKSTOP`
  (`0x…0003`). They have `index` 0, 1, 2 in the account tree. `INSURANCE` also holds `dust: u128` in notional units;
  whenever `dust >= 10^10`, `floor(dust / 10^10)` moves into its balance.
- Bad debt: after the system passes (and after the dust sweep, 3.3 step 9b), an account with no position and a
  negative balance is paid up to zero by `INSURANCE` as far as its balance goes; the rest stays as the account's
  negative balance and is reported as a `BadDebt` event in EVERY block in which it remains (never hidden; I-2
  stays exact because it counts signed balances), and is paid in a later block as soon as the fund holds anything.
- `BACKSTOP` and bad debt (R430-1, 0.2). `BACKSTOP` has no key and is never liquidated (6.3), so a position it took
  can lose more than its balance; when ADL, a forced close or X3 then closes that position, its balance goes
  NEGATIVE. This is bad debt, and because `BACKSTOP` is never liquidated a loss it has realised is final at once:
  its negative balance is paid by `INSURANCE` in the same block as far as the fund goes, WHETHER OR NOT it still
  holds positions (0.2.3; 0.2.0 waited until it was flat, which could leave the debt unreported for as long as a
  position remained), with a `BadDebt` event for the rest in every block it remains; it takes no new position
  while its balance is not positive (6.3 step 3), and its negative equity is charged to `INSURANCE` in the escape
  values (section 11). The fuzz suite reaches this state on purpose (review 436's request with R436-5): test
  `backstop_takes_positions_goes_under_water_and_its_debt_is_reported` and a random run with a small backstop. At `f0e3967` this
  reachable state broke the engine's own assertion "system accounts are never negative" (test
  `r430_1_backstop_loss_beyond_its_balance_is_bad_debt_paid_by_insurance` fails there). Not solved: an unwind rule
  that would cut the backstop's loss earlier (O-9).
- I-1 (size balance): for every market, the sum of `size` over all positions is 0.
- I-2 (conservation): `10^10 * (sum of all balances + all isoMargins) + dust - sum of all openNotional
  == 10^10 * (credited deposits - withdrawal records)` exactly, at every height.
- I-3 (restated in 0.2; the 0.1 text "no balance is negative" was false as written, R395-17 and R430-1):
  `PROTOCOL_FEE` and `INSURANCE` are never negative and never hold a position or an order; a user's or `BACKSTOP`'s
  balance is negative only as bad debt (above), and at the end of a block no FLAT account is negative while
  `INSURANCE` holds anything; `BACKSTOP` never holds an order; every resting order satisfies tick, step and the
  book's price-time order; no book is crossed after a block; an account holds at most `MAX_OPEN_ORDERS` per market
  and 1000 in total, at most `MAX_AGENTS` agents, and every index kept beside the primary data (address to
  account, agent to owner, account to orders, market to triggers) is exactly what the primary data implies.
- I-4 every applied action's `(signer, nonce)` is unique over the whole history.
- I-5 determinism: the state root is a function of the log only (replay on two machines; `engine replay`).

## 9. Deposits, withdrawals, fees

- S-9.1 Deposit: credit `amount` to `account` (created with the next index if new). Deposits are never refused by
  the engine (compliance freezes are an Open item: v0.1 engine has none; the node refuses actions of sanctioned
  addresses, which leaves their funds in place).
- S-9.2 Withdraw: requires the main key; `amount > withdrawalFee`; `amount <= freeCollateral` where
  `freeCollateral = min(balance, floor((E' - IR') / 10^10))`, clamped at 0, and `E'`, `IR'` are `E` and `IR` with
  every position in a HALTED market valued, and its requirement computed, at the side of the oracle's EXIT BAND
  worse for its holder (a long at `low`, a short at `high`; 394's README 6a X1/X2: the band covers the last good
  index and every committee price since the halt, widened by `exitHaircutBps >= jumpBps` plus
  `exitHaircutPerMinBps` per minute of darkness up to `exitMaxHaircutBps`, and only widens). Decided (technical,
  R395-5 with 394): a halt never blocks free collateral; the band is the bound the protocol stands behind for a
  price it cannot observe (it replaces 0.1.3's "loss only, IR x2" rule, which had no tie to the prices the committee
  saw). With no halted market `E' = E`, `IR' = IR` (the 0.1 rule). Cost: during a long halt users can withdraw less
  than their mark-to-market equity. Effect: `balance -= amount`; `withdrawalFee` to `PROTOCOL_FEE`; a withdrawal record
  `{withdrawalId (u64, consecutive from 0), account, recipient, amount - withdrawalFee, height}` is appended to the
  withdrawal tree.
- S-9.2.1 Recipients (R395-22, 0.2). A signed `Withdraw` is refused with `BAD_RECIPIENT`, before any state
  change, when `recipient` is (a) the vault (`verifyingContract`), (b) the collateral token contract (genesis
  `collateralToken`, section 10), or (c) any address `<= 0x000000000000000000000000000000000000ffff`: VAULT.md
  pays no record to such a keyless recipient, and the range holds the system accounts `0x…01` to `0x…03` and the
  EVM precompiles. Tokens sent to (a) or (b) would be stranded; a record to (c) would debit the account and never
  be paid. The zero address stays `MALFORMED`. As for every failed action the nonce is consumed (S-2.5). A forced
  withdrawal pays the account itself and the fee sweep pays `feeRecipient`: neither can name such an address
  (genesis refuses a `collateralToken` equal to the vault or the fee recipient or inside range (c)). Tests
  `r395_22_withdraw_to_the_vault_or_a_keyless_address_is_refused` (fails on `f0e3967`),
  `r395_22_recipient_rule_and_collateral_token`.
- S-9.3 Trading fees: `fee = ceil(|size| * price * feePpm / 10^6 / 10^10)` USDC units per fill, `feePpm` from the
  account's volume tier (S-9.4); no rebates (every tier's maker fee `>= 0`).
- S-9.4 Volume tiers (392's `risk.json` `feeTiers`, genesis `feeTiers[{minVolume14d (USDC units), takerFeePpm,
  makerFeePpm}]`): tier 0 has `minVolume14d = 0` and MUST equal every market's `takerFeePpm`/`makerFeePpm`;
  volumes ascend, fees never increase, `0 <= maker <= taker <= 1%`. An account's volume is the floor in USDC units of
  the notional of each of its book and liquidation fills (maker and taker; agents trade as their account), kept
  per UTC day. The tier of a fill is the highest whose `minVolume14d` is `<=` the sum over the 14 COMPLETE UTC days
  before the fill's day. Decided (technical): today's volume counts from tomorrow, so the fee of a fill is known
  for the whole day and an account cannot change its own tier inside a block by wash volume (wash volume still
  pays fees at today's tier; self-trades never fill, S-4.4). Backstop and ADL transfers count no volume and pay no
  fee. Fees go to
  `PROTOCOL_FEE`. Sweep: at each UTC day boundary (`T / 86400000` changes), if `PROTOCOL_FEE.balance >=
  minFeeSweep`, the whole balance becomes a withdrawal record with `recipient = feeRecipient` (genesis; MUST be
  `0x775a993F09Bd80aeCF8F8c2ADe0C84158E3a0544`, the vault checks the same constant).

## 10. Genesis

The reference is `engine/src/genesis.rs` (`deny_unknown_fields`, every integer a decimal string without leading
zeros, in engine units): `{specVersion "0.1", chainId "700770", verifyingContract, feeRecipient (MUST be
0x775a…0544), oracleSigners[n], oracleQuorum (odd, strict majority), oracleMaxFutureMs, oracleMaxAgeMs,
withdrawalFee, minFeeSweep, feeTiers[], launchInsuranceMin, launchBackstopMin, backstopOiPpm, markets[],
collateralToken? }` (0.2: `collateralToken` = the bUSDC contract of the vault's chain, S-9.2.1; OPTIONAL and LAST,
and omitted from `genesisEncoding` when absent, so a genesis written before 0.2 keeps its `logHash(0)` and every
state root: the round-1 demo log still replays to its committed root, test
`demo_log_of_round_1_replays_to_its_committed_root`. A genesis bound to a REAL vault MUST set it; the converter
never guesses it: `engine genesis … --collateral-token 0x…`, and a caller of `from_compliance` sets the field
before `check()`. Without it rule S-9.2.1 (b) is not enforced; (a) and (c) always are), per market `{marketId (ascending, 1..255), symbol, tick, step, minSize,
maxSize, oiCap, maxLeverage, defaultLeverage, tiers[{floor (USDC units), imfPpm, mmfPpm}], takerFeePpm,
makerFeePpm, liquidationFeePpm, markBandBps, marketSlippageBps, impactNotional (USDC units), fundingIntervalMs,
interest, fundingClamp, maxFundingRate, oracle{minSources, agreeBps, jumpBps, maxStalenessMs, confirmRounds}}`.
The checkpoint committee is NOT engine state (vault and node). `genesisEncoding` = the engine's serialization of
this object (fields in the order above, no whitespace, integers as decimal strings). The engine reads 392's `markets.json`/`risk.json` through a converter that applies S-1.2
and refuses any inconsistency (display decimals vs tick, MMF `<=` markBand, tiers not ascending, IMF `<` MMF).
0.2.3 (R436-8): `impactNotional` MUST be at least 1 (the premium sample divides by the size it buys; `f0e3967`
accepted 0 and divided by zero at the first sample), and the converter REFUSES a JSON integer that does not fit
its field (`imfPpm`, `minSources`, `maxLeverage`, every bps and ms value, optional ones included) instead of
narrowing it (`f0e3967` read 4295017296 as 50000). Not done (436's Info): a per-market reference range for the
FIRST index a market accepts.

## 11. State root and proofs (what the vault verifies)

Hashing: `H = keccak256`. Leaves `H(0x00 || data)`, inner nodes `H(0x01 || left || right)` (domain separation of
leaves and nodes). Empty subtree of height 0 is `bytes32(0)`; `Z(i+1) = H(0x01 || Z(i) || Z(i))`.

- Account tree: fixed depth 32, leaf position = account `index`. Account leaf data (abi.encodePacked, all
  big-endian):
  `"PPv1acct" (8 bytes) || address:20 || be32(index) || be128(balance, two's complement) || be64(escapeValue) ||
  positionsHash:32` where `escapeValue` = `max(0, floor((E + sum of isolated (isoMargin*10^10 + U)) / 10^10))` at the
  block's mark prices (USDC units: what the account is owed if every position is closed at mark), EXCEPT that the
  total negative equity of all accounts (`sum of ceil(-E / 10^10)` over accounts with `E < 0`) is first charged to
  `INSURANCE`'s escape value (R395-23: the order of 6.3; the sum of escape values then never exceeds custody while
  the insurance fund covers the deficits, invariant checked after every block; only the remainder becomes the
  vault's pro-rata haircut), and
  `positionsHash = keccak256(be32(count) || [be32(marketId) || be64(size, two's complement) ||
  be128(openNotional, two's complement) || be64(isoMargin) || u8(isolated)]...)` in ascending market id.
- Market tree: depth 8, leaf position = marketId; data `"PPv1mrkt" || be32(marketId) || be64(index) || be64(mark) ||
  be64(longOI) || i128 fundingIndex || u8 status`.
- Withdrawal tree: depth 32, leaf position = `withdrawalId`, append-only over the whole history; data
  `"PPv1wdrl" || be64(withdrawalId) || account:20 || recipient:20 || be64(amount) || be64(height)`.
- `systemHash = keccak256("PPv1sys" || be64(nextDepositIndex) || be64(nextForcedIndex) || be64(nextOrderId) ||
  be128(totalEscape) || be128(dust) || auxHash || be64(withdrawalCount) || be64(withdrawalsTotal))` (0.1.5: the
  last two at VAULT.md 5.2's request, so the vault's recomputed `stateRoot` covers them; `nextForcedIndex` = the
  number of PROCESSED forced requests, section 14; received-but-pending requests, the received count and both
  chain heads are in `auxHash`), `totalEscape` = sum of all `escapeValue`; `auxHash` = keccak256
  of every remaining piece of state (orders and book queues, triggers, oracle internals, last trade, premium
  accumulators, leverage settings, agents, nonce sets, last oracle round) in the fixed order of
  `Engine::aux_hash` (engine/src/engine.rs), so that validators agree on the WHOLE state, not only on balances.
  These hashes (all but the three tree roots) are plain keccak256 without the leaf/node prefix.
- `stateRoot = H("PosternPerps/state/v1" || be64(height) || be64(time) || accountsRoot || marketsRoot ||
  withdrawalsRoot || systemHash)`.
- Proof: `{leafData, index, siblings[depth]}`; sibling order bottom-up; bit `i` of `index` = 1 means the node is a
  right child at level `i`. The vault receives `accountsRoot`, `withdrawalsRoot` and `totalEscape` in the committee
  checkpoint (VAULT.md, 390) together with `stateRoot`, and checks the root equation.
- Bound on the escape total (0.2, checked after every block, unconditional form of R395-23's invariant): from I-1
  and I-2 the signed equities of all accounts sum to `10^10 * custody - dust`, hence
  `totalEscape <= custody + max(0, D - floor(E_INSURANCE / 10^10))` where `custody` = credited deposits minus
  withdrawal records and `D` = the sum of `ceil(-E / 10^10)` over the accounts with `E < 0` (`BACKSTOP` included):
  the escape values exceed custody by at most the deficit the insurance fund could not absorb, and not at all
  while it covers `D`.
- Escape solvency (input to 390): in escape mode the vault pays `escapeValue * min(1, vaultBalance / totalEscape)`
  per account (pro-rata if the last root shows less in the vault than is owed), plus deposits with
  `index >= nextDepositIndex` refunded to their depositors. 390 decides the exact rule in VAULT.md; the engine
  provides `escapeValue` and `totalEscape`.

## 12. Trust and what the sequencer can do (stated)

Cannot: forge or alter an action (S-0.5), replay one (S-2.5), move funds (withdrawal records only from signed
Withdraw or the fixed fee sweep; payout needs the committee's checkpoint), invent a deposit that validators do not
see finalized on the L2, or move the index outside the honest oracle range (394). Can: reorder actions inside a
block and choose which arrive (front-running within the 2-minute nonce window and the mark band), delay or censor
(bounded by forced requests and escape mode in the vault), withhold oracle updates (markets halt, fail closed).

What a halt it causes gives the sequencer, after 0.2.3 (R436-10 High): NOT a free exit. A single account that
leaves during a halt (forced close, withdrawal) is priced at the side of the exit band worse for it (section 14,
S-9.2), and X3 settles a market only at a price the committee signed since the halt, never at the frozen
pre-halt index. Residual, stated: (a) among the committee rounds of the halt it may include, the sequencer chooses
which one is the `settle` price of X3 (a real committee price of that hour, not one it invents); (b) the band is a
parameter: an exit priced at the band is a loss for the leaver only if the real move stays inside it (432's
calibration: the default band is too narrow for SOL's and marginal for ETH's worst recorded minutes; wider values
are 432's proposal and an owner input); (c) with no committee price since the halt nothing settles, so a silent
sequencer ends in the vault's escape after `forcedDelay`, as designed.

Oracle selection (R395-9, 432's bound, stated here at its request): by choosing which `>= q` valid observations of
a round to include, the sequencer moves the committee median between the order statistics `x[q/2]` and
`x[n - q + q/2]` of ALL valid observations of the round (n = 7, q = 5: between the third and the fifth lowest; the
honest full median is the fourth), every round and to the same side if it wants; with up to `n - q` colluding
signers, over the whole range of the included honest values; never outside the honest range. S-5.1.2 does not
widen this: a dropped observation is one the sequencer could have left out anyway.

Fills (R436-1, S-4.9.1): two accounts can still trade with each other at any price inside the band and move their
OWN equity between them down to the loser's maintenance requirement; they cannot move the insurance fund's, the
backstop's or a third account's. An order whose backing was lost to a mark move rests until a taker reaches it;
the sequencer, which orders the takers, can keep such an order of its own in the book for one sample or one mark
(bounded by `markBandBps` and the funding cap, as R395-16 below).

Data availability of the exit (R436-11, stated; the rule belongs to VAULT.md and the node): an escape claim needs
the account's leaf and its 32 siblings against the vault's last accepted `accountsRoot`, and the siblings depend
on every account's state at that height. The engine produces them for anyone who holds the input log up to that
height (`engine prove`, section 15) or a snapshot of it (section 16); it cannot produce them from the vault's
data alone. The escape hatch therefore assumes that at least one party other than a silent operator publishes
the log, a snapshot, or the proofs themselves.
Residual (R395-16, stated): a sequencer that places a quote as the LAST action of EVERY block and cancels it as the
first action of the next is never hit (it orders every other action) and is sampled every time; post-action
sampling and the median then do not help, and it can set the premium up to `maxFundingRate` (0.05% per hour in
0.2.1) on every position. The bound is the funding cap and the clamp; a sampling point the sequencer cannot
predict (e.g. derived from the next oracle round) is the planned fix (O-10).

## 13. Engine interface (for 389) and replay

`genesis(params) -> Engine`; `apply_block(&Block) -> Result<BlockOutcome, BlockError>` (outcome per input with
error name, events: fills, book deltas, funding, liquidations, ADL, deposits credited, withdrawal records, halts);
`state_root()`; `Engine::snapshot(&self) -> bytes` and `Engine::restore(genesis, bytes, expectedRoot) -> Engine`
(section 16; verifies); queries (account, book, orders, market); `account_proof(address)`, `withdrawal_proof(id)`.
CLI: `engine replay <genesis.json> <log> [--expect-root <hex> --at <height>]` prints the root per height;
`engine snapshot <genesis.json> <log> --at <height> --out <file>`; `engine resume <genesis.json> <snapshot> <log>
--expect-root <hex> [--to-root <hex> --at <height>]` (the expected root is mandatory: section 16).

Error names (the API passes them through): `BAD_SIGNATURE`, `AGENT_NOT_ALLOWED`, `AGENT_INVALID`,
`NONCE_WINDOW`, `NONCE_USED`, `UNKNOWN_ACCOUNT` (actions only from addresses with a ledger entry, created by a
deposit), `SELF_TRADE`, `CANCELLED`, `ISOLATED_UNSUPPORTED`, `MALFORMED`, `UNKNOWN_MARKET`, `MARKET_HALTED`, `TICK`, `STEP`, `SIZE_BOUNDS`,
`WOULD_CROSS`, `REDUCE_ONLY`, `INSUFFICIENT_MARGIN`, `TOO_MANY_ORDERS`, `OI_CAP`, `NO_LIQUIDITY`, `SELF_TRADE`,
`ORDER_NOT_FOUND`, `LEVERAGE`, `NOT_ISOLATED`, `WITHDRAW_TOO_SMALL`, `BAD_RECIPIENT` (0.2, S-9.2.1),
`NOT_LAUNCHED` (S-4.11), `PRICE_BAND` (0.2.3, S-4.12), `POSITION_IN_HALTED_MARKET`, `OVERFLOW`,
`DEPOSIT_ORDER`, `FORCED_ORDER`, oracle errors of 394 (`ORACLE_ROUND_NOT_NEW`, `ORACLE_MALFORMED`,
`ORACLE_BAD_SIGNATURE`, `ORACLE_TIME`).

Events and queries added for the node's views (0.2.3, 431's requests; none is state): `Accepted { order, account }`
is pushed where a PlaceOrder input gets its order id (resting, IOC and trigger alike), in input order and before
any fill of that order; the ids of liquidation and forced takes are not announced (they belong to no input).
`Fill` carries `taker_realized` and `maker_realized` (notional units, signed: what the fill realised on the part
of each side's position it closed, before the S-0.3 conversion; 0 for a side that only opened). `Funding` carries
the `mark` its payments used and the interval's median `premium`. Read-only and public: `account_req(account,
None) -> (IR, MR)`, `market_req`, `leverage_of`, `bankruptcy_price`, `fill_keeps_maintenance`, `free_collateral`,
`equity`, and the formulas of `calc`. A node view MUST use these, not a copy of the arithmetic.

## 14. Forced requests (from the vault)

Kinds = VAULT.md 0.2 `requestForced(uint8 kind, uint64 arg)`: `0` cancel all orders, `1` close every position
(`arg` must be 0 for both: otherwise rejected `MALFORMED`, consumed), `2` withdraw `arg` gross (0 = all free
collateral) to the account itself, rules of S-9.2 without a signature. Shared vector (test `tests/r390.rs`,
independent abi encoding): requests `(account = key(1)'s address, 0, deadline 1794966400, 0)` and `(same, 2,
1794966401, 5000000)` under vault `0x…0ba417` give the forcedHead printed by that test (also in the cross-area log).

Received requests form a queue in index order. Every block (step 7, after deposits and forced inputs, before
actions) the engine serves the queue FROM ITS HEAD with the current state; each request ends in one of:

- served (event `ForcedServed`): cancel all always; close: each position, in ascending market id, has a PRICE
  BOUND = the side worse for the closer of the band the protocol stands behind, and NEVER past the account's
  bankruptcy price (6.3 step 2, recomputed per position): on an ACTIVE market `mark - band` for a long and
  `mark + band` for a short (`band` from `marketSlippageBps`); on a HALTED market the exit band, `low` for a long
  and `high` for a short (below). The position is closed reduce-only through the book inside the bound (makers
  fill at their own, better, prices, each under S-4.9.1), then on an active market by the backstop (within its
  limit and margin), then by the opposite positions in ADL order, the last two AT the bound, with the account's
  taker fee, or with no fee when the bound is the bankruptcy price (as a liquidation). A forced close is therefore
  always served, never cheaper for the closer than the book, and (0.2.3, R436-1) can never leave a deficit: at
  `f0e3967` it sold down to `mark - 5%` whatever the account's equity (test
  `r436_1_a_forced_close_never_goes_past_the_bankruptcy_price`); withdraw: when `amount` (or all) is within free
  collateral;
- rejected and CONSUMED (event `ForcedRejected`) only for a reason the user caused and that is provable from
  finalized state on live prices: an unknown market id, a withdrawal above free collateral or not above the fee
  while the account has NO POSITION in a halted market, an unknown kind. Before a withdrawal that cannot be served
  is judged, the engine cancels the account's resting orders that reserve margin in HALTED markets (R395-39,
  restated in 0.2.3; event `Cancelled` with `MARKET_HALTED`) and computes free collateral again: such orders
  cannot fill and hold margin only through a price the oracle rules do not stand behind, so they may neither turn
  the request into a consumed rejection (`f0e3967`) nor keep it waiting (0.2.0 left it pending until the user
  cancelled, which with X3 now conditional could last until the vault's deadline). Trigger and reduce-only orders
  reserve nothing (6.2) and are left alone (test
  `r395_39_trigger_in_a_halted_market_does_not_make_an_overlarge_request_pending`);
- a close of a position in a HALTED market is also SERVED, never refused for a halt (0.1.4, README 6a of the
  oracle): reduce-only through the book only inside the exit band (`low` for a sell, `high` for a buy), the rest
  against the opposite positions in ADL order AT THAT SAME SIDE OF THE BAND (0.2.3, R436-10 High; 432's corrected
  6a: every exit of ONE account during a halt is priced at the side of the band worse for that account, and the
  difference to `settle` belongs to the deleveraged counterparties; never past the bankruptcy price). Until 0.2.2
  the rest closed at `settle`, which with no committee price since the halt is the FROZEN index: whoever decides
  when the index freezes held a free stop at the last price, paid by the counterparty (436's demonstration: the
  short avoided 2,660 USDC of a 4% move on 1 BTC). Tests `r436_10_a_forced_close_in_a_halt_is_priced_at_the_band_
  side_worse_for_the_closer` (fails on `f0e3967`) and the restated `r395_5_forced_close_in_a_halt_…`;
- PENDING, NOT consumed (event `ForcedPending`): a withdrawal that cannot be served (above free collateral, or
  not above the fee, after the cancellation above) while the account has a position in a halted market, or a
  request from an address whose deposit is not yet credited (the sequencer's delay). A pending head blocks
  everything behind it (strict index order). At `f0e3967` a request that failed only because an order in a halted
  market held margin was rejected and consumed (R395-39; test
  `r395_39_order_in_a_halted_market_makes_a_forced_withdraw_pending_not_rejected` fails there; since 0.2.3 it is
  served in the block that processes it). `amount = 0` (all free collateral) is the recommended form of a forced
  withdrawal: it is never rejected for an amount.
- X3 settlement: when a market has been halted for `maxHaltMs` (default one hour, inside the vault's one-day
  `forcedDelay`) AND the committee has signed at least one price for it since the halt (0.2.3, R436-10, 432's rule:
  `settle` is then that latest committee price), step 5b of 3.3 cancels its orders and triggers and settles EVERY
  position of the market at `settle` (event `MarketSettled`; exact: the sizes sum to zero, I-1, so I-2 holds); its
  collateral is then free under S-9.2 and a pending withdrawal is served. With NO committee price since the halt
  nothing settles, however long it lasts: `settle` would be the frozen pre-halt index, a price chosen by whoever
  withholds the oracle input (at `f0e3967` an hour of silence closed the whole market there; test
  `r436_10_silence_alone_never_settles_a_market`). The market stays halted, single accounts still exit at the
  band (forced close, S-9.2), and a silence that outlasts a pending request's deadline ends in the vault's escape.
  Trade-offs (stated): a genuine outage of an hour with a committee price closes the market's positions at the
  last price the committee signed; a genuine TOTAL oracle outage no longer settles by itself.

`forcedProcessed` counts served plus rejected only. The vault (VAULT.md 5.3 rule 4) refuses a checkpoint that leaves
a request unprocessed past its deadline and enters escape; the deadline counts from the request in L2
`block.timestamp` seconds, so a halt the sequencer causes by withholding oracle inputs can delay exits by at most
`forcedDelay` before escape (R395-5). Decided (technical): consuming user-caused rejections is required, otherwise
any depositor could stall the queue with an unservable request (for example a withdrawal of more than it owns) and
force a global escape. Trade-off: a genuine oracle outage longer than `forcedDelay` with a pending close or
withdrawal also ends in escape.

## 15. Worked example (S-0.3, S-1, S-6.2, S-9.3)

BTC at index = mark = 67,250.0 USD: `price = 6725000000000`. Buy 0.5 BTC: `size = 50000000`. Notional =
`6725000000000 * 50000000 = 336250000000000000000` notional units (= 33,625 USD * 10^16). Taker fee 450 ppm:
`ceil(336250000000000000000 * 450 / 10^6 / 10^10) = 15131250` USDC units = 15.13125 USDC. With leverage 10 and
tier-1 IMF 20000 ppm: `imf' = max(20000, 100000) = 100000`; IR = 3,362.5 USDC. MMF 10000 ppm: MR = 336.25 USDC.
(Those tiers are the engine's test market; with the `compliance/markets.json` 0.2.1 BTC-USD tiers the same position
needs IMF max(5%, 10%) = 3,362.5 USDC and MMF 2.5% = 840.625 USDC.)

Machine-readable vectors (0.2.2), `engine/vectors/worked-examples.json`, generated by `engine examples` from
`engine/src/calc.rs`, the single definition of each formula that the state machine itself calls. 175 cases:
`fees` (S-9.3), `margin` (6.2: notional, leverage fraction, initial and maintenance requirement in notional and USDC
units, both tier sets, tier crossings and a notional exactly on a tier floor), `liquidationPrice`,
`liquidationExecution` (6.3: bankruptcy price, price bound, liquidation fee before its cap), `fundingRate` and
`fundingPayment` (S-7.2, S-7.3), `premium` (S-7.1), `mark` (S-5.2), `pnl`. Every integer is a decimal string in
engine units; the file's `rounding` object restates each rounding direction.

- S-15.1 Liquidation price shown to a user (new in 0.2.2; a DISPLAY definition, the engine itself only evaluates
  `E < MR`). For a cross account holding ONE position and nothing else it is, for a long, the HIGHEST mark at
  which `E < MR` (6.2), for a short the LOWEST; `null` when no mark in `[1, MAX_PRICE]` liquidates the account.
  One price unit on the safe side the account is not liquidatable; each case carries both evaluations
  (`atLiquidationPrice`, `oneUnitOnTheSafeSide`). With several positions or resting orders there is no single
  liquidation price: the frontend shows it per position holding the others at their current marks, and says so.
  The liquidation EXECUTES at the book inside the bound of 6.3 step 2, not at this price.
- Three independent checks: (1) `engine/vectors/check-worked-examples.py` recomputes all 755 values from this
  document's text with Python integers (the liquidation price in closed form per tier segment, not by the
  engine's bisection); (2) test `worked_examples_file_is_current` regenerates the file and compares it byte for
  byte, so it cannot drift from the engine; (3) test `state_machine_matches_the_formulas` runs the STATE MACHINE:
  the fee of a fill, the initial margin behind free collateral, the funding paid and received at a boundary, and a
  long and a short that are NOT liquidated one price unit before their liquidation price and ARE liquidated at it.
- `engine/vectors/proofs-example.json` (`engine proofs-example`; for any log `engine prove <genesis> <log>
  [--at h]`): the 16 checkpoint fields of VAULT.md 5.2 under their names there, `systemHash`, `stateRoot`,
  `custody`, every account leaf (fields, leaf bytes, hash, 32 siblings) and every withdrawal record with its proof,
  from a 15-block rehearsal that ends with a market halted and an account under water, so that the escape values
  show the insurance-first rule (section 11): the vector for 433's `withdraw`, `claimEscape` and
  `proveSystemShares` tests. Test `proofs_example_file_is_current_and_verifies_independently` rebuilds every leaf,
  proof, `systemHash` and `stateRoot` from the text of section 11 without `merkle.rs`.

## 16. Snapshots (format version 1)

A snapshot is the canonical serialization of the WHOLE engine state after a block, so that a node recovers without
replaying its log from genesis. `engine/src/snapshot.rs` is the reference; the layout below is normative for
`formatVersion = 1`. All integers big-endian and fixed width; `i64`/`i128` two's complement; `flag` = one byte 0
or 1; `count` = `be32`; every list in the stated order, keys strictly ascending.

```
snapshot = "PPSNAP" 0x00 0x00 (8) || be16 formatVersion (= 1) || genesisHash (32) || body || keccak256(all previous bytes) (32)
genesisHash = keccak256(genesisEncoding)                        (section 10)

body =
  be64 height || be64 time || logHash (32)                      (offsets 42, 50, 58)
  be64 nextOrderId || be64 nextDepositIndex || be64 forcedReceived || be64 forcedProcessed
  depositsHead (32) || forcedHead (32) || i128 dust
  flag hasOracleRound [ be64 lastOracleRound ] || i128 creditedDeposits || i128 withdrawnTotal || flag launched
  count accounts, each in index order (0, 1, 2 = the system accounts):
      address (20) || i128 balance
      count positions  [ be32 marketId || i64 size || i128 openNotional ]      ascending marketId
      count leverage   [ be32 marketId || be32 leverage ]                      ascending marketId
      count agents     [ address (20) || be16 nameLen || name || be64 expiresAt ]   ascending address
      count volumeDays [ be64 utcDay || be128 volume ]                         ascending day
  count markets (= genesis), each in ascending marketId:
      be32 marketId || be64 index || be64 indexTime || u8 status (0 active, 1 stale, 2 jump, 3 arithmetic)
      flag hasRun [ be64 lastPrice || be32 count || be64 lastTimeMs ] || be64 haltLow || be64 haltHigh
      be64 mark || flag hasLastTrade [ be64 price || be64 time ] || be64 longOI
      count premiumSamples [ i128 ] || i128 cumulativeFunding
      bids: count levels [ be64 price || count ids [ be64 orderId ] ]          ascending price, queue order
      asks: the same
  count orders (resting and triggers), ascending orderId:
      be64 orderId || be32 accountIndex || be32 marketId || flag isBuy || be64 price || be64 remaining
      flag reduceOnly || clientOrderId (32) || flag isTrigger [ u8 orderType || be64 triggerPrice ] || be64 placedHeight
  count withdrawals, in id order:   account (20) || recipient (20) || be64 amount || be64 height
  count forcedQueue, in index order: be64 index || account (20) || u8 kind || be64 deadline || be64 arg
  count nonceSets, ascending signer: address (20) || count nonces [ be64 ]     ascending nonce, 1 to 64
```

- S-16.1 Complete. The body holds every field of state that can change a later block's outcome: everything the
  state root commits to (section 11, including all of `auxHash`'s inputs) and the two it does not: `logHash` and
  the credited-deposit total. NOT stored, rebuilt at load from the primary data: the genesis (bound by its hash),
  the domain separator, the address to account index, the agent to owner index, each account's order list, each
  market's trigger set, withdrawal ids (their positions), the last block's events and pass diagnostic. The queue
  order inside a book level is stored because it cannot be rebuilt from the order ids (a fired trigger keeps its
  id and joins its level later).
- S-16.2 One encoding per state: equal states give equal bytes, and a loader accepts bytes only if re-encoding the
  loaded state reproduces them exactly.
- S-16.3 Strict load (S-0.4), in this order: length and magic (`not a snapshot`); `formatVersion` (an unknown
  version is refused, never guessed); checksum; genesis hash; then the body: a short input, trailing bytes, a flag
  that is not 0 or 1, an unknown status or order type, a count larger than the remaining input (checked before
  anything is allocated), keys out of order or repeated, an unknown market, a system-account address that is not
  `0x…01..03`, an order of a system or unknown account, an order id `>= nextOrderId`, a zero-size position, a
  leverage outside the market's range, more agents or orders than the limits of 2.4 and S-4.7, an agent listed by
  two accounts, an id twice in a book; every magnitude bound of section 1 plus the load bounds of
  `snapshot.rs` (counters `<= 2^62`; `0 <= dust < 10^10`; totals and sums of balances `<= 10^27` units; sums of
  open notional `<= 10^36`; prices `<= MAX_PRICE`; sizes `<= MAX_SIZE`) BEFORE any arithmetic uses the value
  (S-0.2: the loader never panics on input data; release builds keep overflow checks); then every invariant of
  section 8 on the rebuilt state (`invariants::check`: I-1, I-2 against the stored totals, I-3, cross-references,
  the forced queue, the escape bound); then S-16.2.
- S-16.4 The checksum detects DAMAGE, not forgery (anyone can recompute it). `restore` therefore takes the state
  root the caller already trusts (its own log's root at that height, or a root finalized by the committee) and
  refuses the snapshot when the rebuilt state's root differs. With that root every byte of the body is bound
  EXCEPT the 32 bytes of `logHash`, which the state root does not cover: the node MUST compare the loaded
  `logHash` with its own log's hash at that height (the checkpoint of VAULT.md 5.2 carries `logHash` beside the
  root's inputs). Test `every_forged_byte_is_refused_against_the_expected_root_except_log_hash` changes every
  byte (two single-bit changes each, and every 8- and 16-byte window set to extreme values) with the checksum
  recomputed: all refused, none panics, and exactly the `logHash` bytes are accepted. Loading without an expected
  root is possible in the library (`None`) for data the caller produced itself; the CLI requires the root.
- S-16.5 Property (P-4, section 17): for every reachable state `s`,
  `state_root(restore(snapshot(s))) == state_root(s)`, `snapshot(restore(snapshot(s))) == snapshot(s)`, and an
  engine restored at height `h` that applies the log's blocks above `h` produces the same state root, log hash and
  outcomes at every later height as the engine that never stopped. Checked after EVERY block of every fuzz run
  (`tests/fuzz.rs`), on the round-1 demo log, and at every height of the rehearsal of `engine/vectors/
  snapshot-v1.json` (genesis, log, snapshot bytes, root: the shared vector for 431).
- S-16.6 Versioning. A change of the layout, of any derived-at-load rule or of the meaning of a field takes a new
  `formatVersion`; an engine MUST refuse versions it does not know and MAY keep readers for older ones. A snapshot
  is bound to one genesis and is only meaningful to an engine version that computes the same state root for it
  (the root check of S-16.4 enforces this: after an engine change that alters `auxHash` or a leaf, an old
  snapshot is refused and the node replays its log).
- Not stated by the engine (node's rules, 431): when snapshots are taken, how many are kept, the file wrapper and
  its atomic write, and which root a restarted node trusts.

## 17. Properties checked after every block (0.2.2)

`engine/tests/fuzz.rs` (random traders, orders of every type, cancels, deposits, withdrawals, leverage changes,
oracle moves, halts, hour gaps; 64 random runs of up to 250 operations by default, `FUZZ_CASES` for more, plus a
deterministic crash sequence and a path-coverage run) checks after EVERY block, beside `invariants::check`:

- P-1 Conservation from an INDEPENDENT ledger: deposits are counted from the inputs the engine accepted and
  withdrawals from the records it emitted, never from the engine's own totals; I-1 (sizes of each market sum to
  zero) and I-2 (`10^10 * sum(balances) + dust - sum(openNotional) == 10^10 * (deposits - withdrawals)`) are
  recomputed in the test; `0 <= dust < 10^10`; the withdrawal tree sums to the emitted records.
- P-2 `sum(escapeValue) <= custody + uncovered deficit` always, and `<= custody` whenever the insurance fund covers
  the deficits (section 11).
- P-3 No account whose markets are all active ends a block below its maintenance requirement unless step 9 stopped
  at its pass bound (3.3); the requirement is recomputed in the test from the genesis tiers.
- P-4 Snapshots (S-16.5).
- P-5 Replay determinism through the codec: the whole log re-encoded to canonical bytes, decoded (decode then
  encode is the identity) and replayed on a fresh engine gives the same state root, log hash and per-input
  outcomes at every height, and a byte-equal final snapshot.

The independent review's fuzzer (436, written from this document only) is a separate check and not a substitute.
What these properties do not show: anything about states the generator does not reach (the coverage test names
the paths it does reach: book fills, liquidations, ADL or backstop, funding, triggers, halts and resumes).

## Open (v0.1)

- O-1 Compliance freeze of a sanctioned account's funds in the engine (needs an authority input: who signs it).
- O-2 Partial liquidation (v0.1 liquidates whole positions).
- O-3 Backstop vault deposits and shares (v0.1: anyone may deposit to the `BACKSTOP` address through the vault; it
  never signs, so nothing is withdrawable from it in v0.1; who funds it and with how much is an owner input).
- O-4 Volume fee tiers: CLOSED in 0.1.2 (S-9.4).
- O-5 The checkpoint digest and escape rule (390, VAULT.md).
- O-6 Isolated margin is specified (6.2) but NOT implemented in engine 0.1: `UpdateLeverage{isolated:true}` returns
  `ISOLATED_UNSUPPORTED` and `UpdateMargin` returns `NOT_ISOLATED`; leaves carry `isoMargin = 0, isolated = 0`.
- O-7 Oracle signing domain: the engine verifies `OracleObservation` under the engine domain of 2.1 (chainId
  700770 + vault); 394's README v0.1 says 7007701 without `verifyingContract`: one of the two must change (asked
  in the cross-area log; the engine follows 2.1 until agreed).
- O-8 `snapshot()/restore()` of section 13: CLOSED in 0.2.1 (section 16).
- O-9 Backstop unwind (392's proposal: reduce-only limit orders at mark +/- markBandBps): not in 0.1.2; the
  backstop holds what it takes until ADL or a later engine version (it has no key and cannot trade).
- O-10 R395-16 (premium samples from a book the sequencer can quote into): open; the funding cap stays 0.05%/h.
- O-11 (0.2) Nonce sets are kept for every address that ever signed an applied action (S-2.4.2): state that only
  grows, bounded by 64 nonces per signer; a sound pruning rule exists (a set whose highest nonce is below
  `T - 120000` can no longer reject anything the window rule (a) does not already reject) and is NOT implemented:
  it changes `auxHash` and belongs to a version that changes the state format.
- O-12 (0.2) `collateralToken` optional (section 10): make it mandatory in the next genesis `specVersion`.
- Reserved primary type names: `Attestation` (392/397's geoblock attestation) is NOT an engine action and has no
  input type id; the engine refuses it by construction (an unknown `typeId` is `MALFORMED` at decode, S-0.4).
- Genesis from 392's files: `engine genesis <markets.json> <risk.json> --vault 0x.. --oracle-signers .. --quorum q
  --backstop-seed USDC [--backstop-oi-ppm 250000]` converts human decimals with S-1.2 (exact or refused) and never
  guesses an owner input (`TBD-owner` is refused); `defaultLeverage` = `min(10, maxLeverage)` when absent.

## Version history

| Version | Date | Change |
|---|---|---|
| 0.1 | 2026-11-17 | first version (388): scales, domain (700770 + vault), types, nonces, blocks, matching, mark, margin, liquidation, funding, conservation, genesis, state root and proofs |
| 0.1.1 | 2026-11-17 | aligned with engine 0.1 (`647a401`): signed balance (be128) in the account leaf and no height in it; `auxHash` in `systemHash`; nonce consumed on valid signature; IR formula exact; halted-market liquidation rule; bad debt; funding bound; genesis fields; O-6 to O-8 |
| 0.1.2 | 2026-11-17 | 392's requests: per-market `fundingClamp` (S-7.2), volume fee tiers (S-9.4, closes O-4), launch gate (S-4.11), backstop position limit (6.3 step 3), `Attestation` reserved, compliance-file converter; genesis fields `feeTiers`, `launchInsuranceMin`, `launchBackstopMin`, `backstopOiPpm`, `fundingClamp` (was `interestClamp`) |
| 0.1.3 | 2026-11-17 | R395-1 (engine computes `depositsHead`/`forcedHead`, VAULT.md 5.4 formulas; signed values never copied from L2), R395-5 (free collateral withdrawable during a halt, S-9.2; forced queue served from its head every block, halt-caused failures pending, not consumed; forced kinds and input fields = VAULT.md), R395-31 (partial insurance cover) |
| 0.1.4 | 2026-11-17 | 394's exit band (README 6a) used for free collateral during a halt (replaces 0.1.3's x2 rule) and for forced closes in a halt (served at `settle`, never pending); X3 market settlement after `maxHaltMs`; genesis oracle exit fields; `Attestation` and other reserved type names (S-2.3.1) |
| 0.1.5 | 2026-11-17 | systemHash adds withdrawalCount/withdrawalsTotal, nextForcedIndex = processed (VAULT 5.2; 390's vectors reproduced, new vector in tests/r395b.rs); R395-13 liquidation bound `lp` + no same-block makers; R395-14 backstop rules; R395-16 post-action samples + median; R395-23 insurance-first escape values |
| 0.1.6 | 2026-11-17 | forced-request input and forcedHead = VAULT.md 0.2 fields (index, account, kind 0/1/2, deadline, arg), close = every market (390's report on 64072cb) |
| 0.2.3 | 2026-11-19 | review 436's findings, each with a test that fails on `f0e3967`: R436-1 High (S-4.9.1 maintenance at every fill for makers and reducing takers; S-4.9 backing at placement; S-4.12 price band, error `PRICE_BAND`; forced close never past the bankruptcy price, section 14); R436-10 High (forced close in a halt at the band side worse for the closer; X3 only with a committee price since the halt); R436-4 High, engine half (S-3.2.0: a long agent name fails its input, never the block); R436-12 (S-5.1.2: an observation that fails its own rule is dropped); R436-2 (liquidations skip same-block orders instead of stopping); R436-8 (`impactNotional >= 1`, strict integers in the converter); R436-5 (BACKSTOP's realised loss settled and reported every block). R395-39 restated (orders in halted markets are cancelled before a forced withdrawal is judged). Section 12 states the residuals, 432's R395-9 bound and the escape's data-availability assumption (R436-11). 431's requests: events `Accepted`, `Fill` realised results, `Funding` mark and premium; public requirement queries. BEHAVIOUR CHANGES for clients: `PRICE_BAND`; under-backed orders refused at placement or cancelled when reached; forced-close prices |
| 0.2.2 | 2026-11-18 | worked-example vectors (section 15: `engine/vectors/worked-examples.json`, 175 cases, independent Python checker, S-15.1 displayed liquidation price), proof vectors (`proofs-example.json`, CLI `prove`), formulas in one module (`calc.rs`) called by the state machine, properties P-1 to P-5 (section 17), 394's oracle vectors and 392's compliance files read at their source by tests. No state, root or encoding change |
| 0.2.1 | 2026-11-18 | snapshots (section 16, format version 1; closes O-8): canonical state serialization, strict loader with magnitude bounds, mandatory expected root in the CLI, `logHash` stated as not covered by the root; `Engine::snapshot` / `Engine::restore`, CLI `snapshot` and `resume`; shared vector `engine/vectors/snapshot-v1.json`. No state, root or input encoding changes |
| 0.2.0 | 2026-11-18 | round 2 (430): R395-39 (orders in a halted market make a forced withdrawal pending, section 14); R395-22 (S-9.2.1 recipients, error `BAD_RECIPIENT`, optional genesis `collateralToken`); R395-17 (ADL rank for `E <= 0`, bankruptcy price recomputed per position, INSURANCE never below zero; I-3 restated); R395-20 (S-2.4.2 nonce sets outlive agent removal); R395-21 (S-2.4.1 rule); R395-15 pinned; NEW R430-1 (BACKSTOP bad debt paid by INSURANCE; dust sweep before bad-debt settlement, 3.3 step 9b) and R430-2 (S-5.1.1 committee price above MAX_PRICE never becomes the index); unconditional escape bound (section 11); `NOT_LAUNCHED` listed in section 13. No change to any encoding, hash or root of a pre-0.2 log that does not exercise these rules |

## Not done / not proven

`engine/` implements this document except O-6 (isolated margin); its tests check the
invariants of section 8 after every block of random and adversarial sequences, but tests are not proofs. Not
proven: the liquidation order's solvency under gaps, the ADL ranking's fairness, the funding formula against
benchmarks, the mark band against manipulation, gas of the vault's proofs. Not done: a rule that unwinds the
backstop before its loss exceeds its balance (O-9; R430-1 only settles the debt afterwards); pruning of nonce sets
(O-11); a mandatory `collateralToken` (O-12); a freeze input (O-1); partial liquidation (O-2). The findings closed
in 0.2 were demonstrated by tests that fail on `f0e3967` where the defect was in code (R395-39, R395-22, R430-1,
R430-2) and by pins where the code already behaved as now specified (R395-15, R395-17, R395-20, R395-21): a pin
shows the behaviour, not its failing-first history. 0.2.3: S-4.9.1 is argued (no fill creates a deficit) and tested
on the paths 436 and this round found, not proven for every path; the exit band's width is not calibrated (432);
the residuals of section 12 stand; a rule that caps the backstop's loss before it is realised is not built (O-9).
Snapshots (section 16): the round-trip property is tested, not
proven; the format has had no external review; a snapshot loaded with no expected root (library only) is bound by
nothing but its own consistency; `logHash` is outside the state root (S-16.4). UNAUDITED.

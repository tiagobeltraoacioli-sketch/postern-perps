# PosternDEX Perpetuals: engine specification (`docs/SPEC.md`)

**UNAUDITED PROTOTYPE. Version 0.1, 2026-11-17. Owner: agent 388 (continues 350).** Nothing here is deployed.
The perpetuals are not to be deployed before an external audit and a legal review.

This document is normative for `engine/` (the deterministic state machine), and is the interface that `node/`
(389), `contracts/` (390), `frontend/` (391), the mock (393) and the oracle (394) build against. Changes are
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
- S-1.3 Worked bound: `MAX_PRICE * MAX_SIZE = 10^30 < 2^127 ~ 1.7e38`; times a rate `<= 10^9` it may exceed i128,
  so funding (section 7) multiplies by the rate in `I256` and divides before narrowing.

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
| `Withdraw.amount` | USDC units, gross; the flat `withdrawalFee` is deducted from it (section 9.2); `recipient` any address except the zero address; shown by the wallet |
| `ApproveAgent.name` | UTF-8, 0 to 32 bytes; `expiresAt` Unix ms, `T < expiresAt <= T + 180 days` |

### 2.4 Who may sign what (S-2.4)

| Type | Main key | Agent |
|---|---|---|
| PlaceOrder, CancelOrder, CancelAll, UpdateLeverage, UpdateMargin | yes | yes |
| Withdraw, ApproveAgent, RevokeAgent | yes | NO (`AGENT_NOT_ALLOWED`) |

At most `MAX_AGENTS = 4` unexpired agents per account. An agent address MUST NOT be an existing account (an
address with a ledger entry), MUST NOT be an agent of another account, and MUST NOT equal `account`
(`AGENT_INVALID`). Approving an existing agent address of the same account replaces its name and expiry.
Expired agents are removed lazily (at the next action of the account) and count for nothing.

### 2.5 Nonces (S-2.5)

Decided (technical), extending 389's proposal: each signer (main key or agent, separately) has a set `used` of at
most `NONCE_SET = 64` of its highest applied nonces. An action with nonce `x` at block time `T` is valid only if
(a) `T - 120000 <= x <= T + 30000` (the nonce is a millisecond time inside the window), (b) `x` is not in `used`,
and (c) if `used` is full, `x > min(used)`. On success `x` is inserted (evicting the minimum when full).
Reasons: (a) bounds the sequencer's power to hold a signed order and execute it later to two minutes; (b)+(c) make
every signature single-use while allowing up to 64 actions in flight to arrive out of order (a strictly increasing
rule would reject a valid order merely overtaken by the next one). A client that uses `max(clock_ms, last + 1)`
(391's rule) always satisfies the rule. Errors: `NONCE_WINDOW`, `NONCE_USED`.

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

| Tag | Kind | Payload |
|---|---|---|
| `0x01` | oracle | `OracleUpdate` exactly as 394's `node/oracle/README.md` section 4 (canonical bytes) |
| `0x02` | deposit | `be64(depositIndex) || account:20 || be64(amount)` from the vault's `Deposit` event |
| `0x03` | forced | `be64(forcedIndex) || account:20 || kind:u8 || be64(deadline) || be64(arg)` from the vault's `ForcedRequest` event |
| `0x04` | action | `typeId:u8 || fields || signature:65`; `typeId` = 1 PlaceOrder, 2 CancelOrder, 3 CancelAll, 4 UpdateLeverage, 5 UpdateMargin, 6 Withdraw, 7 ApproveAgent, 8 RevokeAgent; `fields` = the message's fields in the type's order, each fixed width big-endian (`address` 20, `uint64` 8, `uint32` 4, `uint8` 1, `bool` 1 byte 0 or 1 only, `bytes32` 32, `string` `be16(len) || utf8`); the engine recomputes the EIP-712 digest from the decoded fields |

- S-3.2.1 Deposits MUST be consecutive: `depositIndex == nextDepositIndex` (starting at 0), else the input fails
  `DEPOSIT_ORDER` and the node must re-submit (the vault index is the single order). Likewise forced requests with
  `nextForcedIndex`. Deposits and forced requests come only from L2 events at the `finalized` tag agreed by two
  endpoints (node's rule); the engine cannot check this and trusts the validator replay (each validator checks the
  events against its own L2 view before signing; node's rule).
- S-3.2.2 JSON form (API, mock, fixtures): the same fields, integers as decimal strings, bytes as `0x` hex.

### 3.3 Order of processing inside a block (normative)

1. Header checks (3.1). `T = block.time`.
2. For each market in ascending id: oracle staleness rule H1 (394 section 6).
3. Oracle input, if present: 394 rules C1 to C5 and H2/H3.
4. Mark price of every market (section 5) from index and the book as it stands.
5. Funding (section 7) for every market whose funding boundary was crossed (`T / fundingIntervalMs > prevT / fundingIntervalMs`).
6. Premium sample for every active market if an oracle round was accepted in step 3.
7. Deposits, then forced requests, then actions, each in input order.
8. Mark price recomputed (section 5).
9. Triggers (section 4.6), then liquidations (section 6), repeated until no trigger fires and no account is
   liquidatable, bounded by `MAX_SYSTEM_PASSES = 8` (remaining work carries to the next block).
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
- S-4.9 Margin at placement (cross): the order is accepted only if, counting it as resting, the account's initial
  requirement including open orders (section 6.2) is covered; else `INSUFFICIENT_MARGIN`. A fill as maker is never
  refused for margin (its margin was reserved; an account that becomes undercollateralised is handled by
  liquidation). A fill as taker re-checks per fill: the taker stops (remainder cancelled, `INSUFFICIENT_MARGIN`)
  before a fill that would leave its equity below its initial requirement, except for a fill that only reduces.
- S-4.10 A halted market accepts only CancelOrder, CancelAll and nothing else (`MARKET_HALTED`); no matching, no
  liquidation, no triggers, no funding sample.

## 5. Index and mark price

- S-5.1 Index = the committee oracle price of 394's rules (median of validator-signed observations, agreement band,
  jump halt and confirmation). The engine applies those rules exactly as 394's README sections 4 to 6 state them
  (normative by reference for v0.1; the vectors in `node/oracle/vectors/` are shared test vectors).
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

`Account { index: u32 (0 to 2 the system accounts of section 8; users from 3, assigned in order of first credit), address, balance: u64 (USDC units, cross collateral),
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
- Initial `IR` the same with `imf_t' = max(imf_t, 10^6 / leverage)`, rounded up, and including resting orders:
  per market `max(n(size + bids), n(size - asks))` where `n(x) = |x| * max(mark, worst order price on that side)`.
- Liquidatable: `E < MR` (strict). A withdrawal, or a margin move out of the cross balance, of `a` USDC units is
  allowed only if `balance >= a` and, after the debit, `E >= IR` (unrealised profit counts toward `E` but can never
  be withdrawn beyond `balance`).
- Isolated: the same formulas per position with `isoMargin * 10^10` in place of the balance term.

### 6.3 Liquidation (order, normative for v0.1)

For a liquidatable cross account (accounts in ascending `index`; isolated positions are liquidated alone the same
way):

1. Cancel all its resting orders and triggers (all markets).
2. For each position (ascending market id): bankruptcy price `bp` = the price at which `E` would be 0 holding the
   other positions at mark (rounded toward the account: up for a long, down for a short). Send a reduce-only IOC
   to the book for the whole position with limit `bp` (never fills worse than bankruptcy) and with the S-4.3
   slippage bound waived.
3. The remainder is taken by the backstop vault (system account `BACKSTOP`) at `bp` if the backstop's equity after
   it covers its own maintenance requirement; else by auto-deleveraging (6.4).
4. Liquidation fee: `liquidationFeePpm` of the liquidated notional, taken from what equity remains (never below 0),
   credited to the insurance fund (`INSURANCE`).
5. If `E < 0` at the start (a gap: the oracle moved past bankruptcy), the position is closed at mark through steps
   2 and 3 with `bp` replaced by mark, and the deficit is paid by `INSURANCE`; if the insurance balance does not
   cover it, the remainder is closed by ADL at the bankruptcy price instead (no deficit is ever socialised outside
   ADL).

### 6.4 Auto-deleveraging (last resort)

Counterparties are ranked by `profitRatio * leverage` (`U / (|size| * mark)` times `|size| * mark / E_account`, in
ppm, integer, ties by ascending account index) on the opposite side of the same market; their positions are
reduced at the bankruptcy price in that order until the remainder is gone. Every ADL fill is an event naming both
accounts.

## 7. Funding

- S-7.1 Premium sample (step 6 of 3.3): `impactBid/impactAsk` = the average fill price of a `impactNotional`
  market order on each side (default 10,000 USD), index when the side is too thin; `premium = (max(0, impactBid -
  index) - max(0, index - impactAsk)) * RATE_SCALE / index` (rounded toward zero).
- S-7.2 At each funding boundary (`fundingIntervalMs = 3600000`): `P = average of the samples` of the interval
  (toward zero; no samples -> no funding for that interval); `rate = clamp(P + clamp(interest - P, -c, +c),
  -maxFundingRate, +maxFundingRate)`. v0.1 defaults (392's `risk.json` supersedes): `interest = 12500`
  (0.00125% per hour in `RATE_SCALE`, i.e. 0.01% per 8 h), `c = 500000` (0.05%), `maxFundingRate = 500000`
  (0.05% per hour).
- S-7.3 Payment per position: `f = size * mark * rate / RATE_SCALE` in notional units, computed in I256; a long pays
  when `rate > 0`. Payers' amounts rounded up, receivers' down (S-0.3); converted to USDC per S-1.1 with the same
  direction; the sum of the residuals goes to dust. Funding is settled into the balance (cross) or `isoMargin`
  (isolated) immediately; an isolated margin that would go below 0 is floored at 0 and the shortfall is a loss of
  the insurance fund (recorded).

## 8. System accounts and conservation

- System accounts (no key, never sign): `PROTOCOL_FEE` (address `0x…0001`), `INSURANCE` (`0x…0002`), `BACKSTOP`
  (`0x…0003`). They have `index` 0, 1, 2 in the account tree. `INSURANCE` also holds `dust: u128` in notional units;
  whenever `dust >= 10^10`, `floor(dust / 10^10)` moves into its balance.
- I-1 (size balance): for every market, the sum of `size` over all positions is 0.
- I-2 (conservation): `10^10 * (sum of all balances + all isoMargins) + dust - sum of all openNotional
  == 10^10 * (credited deposits - withdrawal records)` exactly, at every height.
- I-3 no balance is negative; every resting order satisfies tick, step and the book's price-time order; no book is
  crossed after a block.
- I-4 every applied action's `(signer, nonce)` is unique over the whole history.
- I-5 determinism: the state root is a function of the log only (replay on two machines; `engine replay`).

## 9. Deposits, withdrawals, fees

- S-9.1 Deposit: credit `amount` to `account` (created with the next index if new). Deposits are never refused by
  the engine (compliance freezes are an Open item: v0.1 engine has none; the node refuses actions of sanctioned
  addresses, which leaves their funds in place).
- S-9.2 Withdraw: requires the main key; no position in a halted market; after the debit `E >= IR`; `amount >
  withdrawalFee`. Effect: `balance -= amount`; `withdrawalFee` to `PROTOCOL_FEE`; a withdrawal record
  `{withdrawalId (u64, consecutive from 0), account, recipient, amount - withdrawalFee, height}` is appended to the
  withdrawal tree.
- S-9.3 Trading fees: `fee = ceil(|size| * price * feePpm / 10^6 / 10^10)` USDC units per fill, taker `450`, maker
  `150` ppm at the base tier (BRIEF-PERPS; 392's final table supersedes); no rebates in v0.1. Fees go to
  `PROTOCOL_FEE`. Sweep: at each UTC day boundary (`T / 86400000` changes), if `PROTOCOL_FEE.balance >=
  minFeeSweep`, the whole balance becomes a withdrawal record with `recipient = feeRecipient` (genesis; MUST be
  `0x775a993F09Bd80aeCF8F8c2ADe0C84158E3a0544`, the vault checks the same constant).

## 10. Genesis

`Genesis { specVersion "0.1", verifyingContract, feeRecipient, oracleSigners[n], oracleQuorum, checkpoint
committee is NOT engine state (vault and node), markets[] }` with per market `{marketId, symbol, tick, step,
minSize, maxSize, oiCap, maxLeverage, tiers[{floor (USDC units), imfPpm, mmfPpm}], takerFeePpm, makerFeePpm,
liquidationFeePpm, markBandBps, marketSlippageBps, impactNotional, fundingIntervalMs, interest, maxFundingRate,
oracle{minSources, agreeBps, jumpBps, maxStalenessMs, confirmRounds}}` and global `{withdrawalFee, minFeeSweep,
maxFutureMs, maxAgeMs}`. `genesisEncoding` = the canonical JSON (keys sorted, no whitespace, integers as decimal
strings) of this object. The engine reads 392's `markets.json`/`risk.json` through a converter that applies S-1.2
and refuses any inconsistency (display decimals vs tick, MMF `<=` markBand, tiers not ascending, IMF `<` MMF).

## 11. State root and proofs (what the vault verifies)

Hashing: `H = keccak256`. Leaves `H(0x00 || data)`, inner nodes `H(0x01 || left || right)` (domain separation of
leaves and nodes). Empty subtree of height 0 is `bytes32(0)`; `Z(i+1) = H(0x01 || Z(i) || Z(i))`.

- Account tree: fixed depth 32, leaf position = account `index`. Account leaf data (abi.encodePacked, all
  big-endian):
  `"PPv1acct" (8 bytes) || address:20 || be32(index) || be64(balance) || be64(escapeValue) || positionsHash:32 ||
  be64(height)` where `escapeValue` = `max(0, floor((E + sum of isolated (isoMargin*10^10 + U)) / 10^10))` at the
  block's mark prices (USDC units: what the account is owed if every position is closed at mark) and
  `positionsHash = H(be32(count) || [be32(marketId) || be64(size as two's complement) || be128(openNotional) ||
  be64(isoMargin) || u8(isolated)]...)` in ascending market id.
- Market tree: depth 8, leaf position = marketId; data `"PPv1mrkt" || be32(marketId) || be64(index) || be64(mark) ||
  be64(longOI) || i128 fundingIndex || u8 status`.
- Withdrawal tree: depth 32, leaf position = `withdrawalId`, append-only over the whole history; data
  `"PPv1wdrl" || be64(withdrawalId) || account:20 || recipient:20 || be64(amount) || be64(height)`.
- `systemHash = H("PPv1sys" || be64(nextDepositIndex) || be64(nextForcedIndex) || be64(nextOrderId) ||
  be64(totalEscape) || be128(dust))`, `totalEscape` = sum of all `escapeValue`.
- `stateRoot = H("PosternPerps/state/v1" || be64(height) || be64(time) || accountsRoot || marketsRoot ||
  withdrawalsRoot || systemHash)`.
- Proof: `{leafData, index, siblings[depth]}`; sibling order bottom-up; bit `i` of `index` = 1 means the node is a
  right child at level `i`. The vault receives `accountsRoot`, `withdrawalsRoot` and `totalEscape` in the committee
  checkpoint (VAULT.md, 390) together with `stateRoot`, and checks the root equation.
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

## 13. Engine interface (for 389) and replay

`genesis(params) -> Engine`; `apply_block(&Block) -> Result<BlockOutcome, BlockError>` (outcome per input with
error name, events: fills, book deltas, funding, liquidations, ADL, deposits credited, withdrawal records, halts);
`state_root()`; `snapshot() -> bytes` and `restore(bytes, expectedRoot)` (verifies); queries (account, book,
orders, market); `account_proof(address)`, `withdrawal_proof(id)`. CLI: `engine replay <genesis.json> <log>
[--expect-root <hex> --at <height>]` prints the root per height.

Error names (the API passes them through): `BAD_SIGNATURE`, `AGENT_NOT_ALLOWED`, `AGENT_INVALID`,
`NONCE_WINDOW`, `NONCE_USED`, `MALFORMED`, `UNKNOWN_MARKET`, `MARKET_HALTED`, `TICK`, `STEP`, `SIZE_BOUNDS`,
`WOULD_CROSS`, `REDUCE_ONLY`, `INSUFFICIENT_MARGIN`, `TOO_MANY_ORDERS`, `OI_CAP`, `NO_LIQUIDITY`, `SELF_TRADE`,
`ORDER_NOT_FOUND`, `LEVERAGE`, `NOT_ISOLATED`, `WITHDRAW_TOO_SMALL`, `POSITION_IN_HALTED_MARKET`, `OVERFLOW`,
`DEPOSIT_ORDER`, `FORCED_ORDER`, oracle errors of 394 (`ORACLE_ROUND_NOT_NEW`, `ORACLE_MALFORMED`,
`ORACLE_BAD_SIGNATURE`, `ORACLE_TIME`).

## 14. Forced requests (from the vault)

`kind`: `0` CancelAll (all markets), `1` CloseAll (every position closed by reduce-only IOC at the mark +/-
`marketSlippageBps`), `2` Withdraw (`arg` = gross amount; same rules as S-9.2 except the signature; recipient = the
account itself). A forced request that fails its rules is still CONSUMED (`nextForcedIndex` advances) with its
error recorded, so the vault's deadline check sees it processed. The vault enforces the deadline (VAULT.md).

## 15. Worked example (S-0.3, S-1, S-6.2, S-9.3)

BTC at index = mark = 67,250.0 USD: `price = 6725000000000`. Buy 0.5 BTC: `size = 50000000`. Notional =
`6725000000000 * 50000000 = 336250000000000000000` notional units (= 33,625 USD * 10^16). Taker fee 450 ppm:
`ceil(336250000000000000000 * 450 / 10^6 / 10^10) = 15131250` USDC units = 15.13125 USDC. With leverage 10 and
tier-1 IMF 20000 ppm: `imf' = max(20000, 100000) = 100000`; IR = 3,362.5 USDC. MMF 10000 ppm: MR = 336.25 USDC.
Machine-readable vectors (margin, fee, funding, liquidation price, rounding) follow in `engine/vectors/` with v0.2,
generated by the engine and checked by an independent Python implementation.

## Open (v0.1)

- O-1 Compliance freeze of a sanctioned account's funds in the engine (needs an authority input: who signs it).
- O-2 Partial liquidation (v0.1 liquidates whole positions).
- O-3 Backstop vault deposits and shares (v0.1: anyone may deposit to the `BACKSTOP` address through the vault; it
  never signs, so nothing is withdrawable from it in v0.1; who funds it and with how much is an owner input).
- O-4 Volume fee tiers (392's table).
- O-5 The checkpoint digest and escape rule (390, VAULT.md).

## Version history

| Version | Date | Change |
|---|---|---|
| 0.1 | 2026-11-17 | first version (388): scales, domain (700770 + vault), types, nonces, blocks, matching, mark, margin, liquidation, funding, conservation, genesis, state root and proofs |

## Not done / not proven

Everything in this document is a design; no engine code implements it yet at v0.1. Not proven: the liquidation
order's solvency under gaps, the ADL ranking's fairness, the funding formula against benchmarks, the mark band
against manipulation, gas of the vault's proofs. UNAUDITED.

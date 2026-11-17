# PosternDEX Perpetuals: public API (v0.2)

UNAUDITED PROTOTYPE. Owned by agent 351 (`node/`). Changes are announced in the cross-area log.
Machine-readable schemas: `node/api/schema.json` (JSON Schema, one `$defs` entry per type named below) and
`node/api/endpoints.json` (endpoint to response type). The Rust node and the Node.js mock server are both tested
against these two files; where this text and the schema differ, the schema wins and this text is the bug.

Canonical data rules (action types, EIP-712 typed data, numeric scales, state root, proofs) are in `docs/SPEC.md`
(agent 350). Vault digests and events are in `docs/VAULT.md` (agent 352). Markets and risk parameters are in
`docs/MARKETS.md` and `compliance/*.json` (agent 354). This document only says how they travel over HTTP and WebSocket.

## 1. Conventions

- Base URL: `http://127.0.0.1:45100` in development. REST under `/v1`, WebSocket at `/v1/ws`. JSON, UTF-8.
- **Numbers.** Every price, size, amount, rate and fee is a **decimal string** in human units (`"67250.5"`,
  `"0.125"`, `"1250.000000"`), never a JSON float. The integer scales behind them are those of `docs/SPEC.md`;
  each market publishes `priceDecimals` and `sizeDecimals`; USDC amounts have 6 decimals. Signed values carry a
  leading `-`. Rates are fractions (`"0.0000125"` = 0.00125 %).
- **Integers** that fit in 2^53 (heights, sequence numbers, millisecond timestamps, counts) are JSON numbers.
  64-bit identifiers (`orderId`, `tradeId`, `fillId`, `withdrawalId`, nonces) are decimal strings.
- **Time** is Unix milliseconds. Engine time is the block time proposed by the sequencer and accepted by the
  validators (monotonic, bounded drift), not the wall clock of the API host.
- **Addresses and hashes** are `0x` lower-case hex (20 and 32 bytes); addresses are accepted in any case.
- **Symbols** are `BASE-USD` (`BTC-USD`); every market also has the integer `marketId` used inside signed actions.
- **Sides** are `"buy"` / `"sell"`. Position `size` is signed (negative = short).
- **Finality.** Every state-derived object carries the `height` of the block it reflects. A block is *final* when
  k-of-n validators signed its checkpoint (section 3.2). The API serves the sequencer's latest (soft) state by
  default; `?finalized=true` on account and market reads returns the state at the latest finalized checkpoint.
  Soft state can only be discarded if the sequencer misbehaves or crashes before the block is durably logged.
- **Errors.** Non-2xx responses and WebSocket errors carry
  `{"error":{"code":"SNAKE_CASE","message":"human text","details":{}}}` (`Error`). Codes: section 7.
- **Pagination.** Lists are newest first. `limit` (default 100, max 1000) and `before` (exclusive cursor taken
  from `nextCursor` of the previous page; absent when the list is exhausted).
- **Rate limits.** Per IP and per account; `429 RATE_LIMITED` with `Retry-After` (seconds). Defaults in section 8.
- **CORS.** `Access-Control-Allow-Origin` is the configured frontend origin (`*` in mock mode). No cookies, no
  sessions: authority comes only from EIP-712 signatures.

## 2. Market data (public, not geoblocked)

| Method and path | Returns | Notes |
|---|---|---|
| `GET /v1/markets` | `{"markets":[Market]}` | all markets and their parameters |
| `GET /v1/markets/{symbol}` | `Market` | |
| `GET /v1/tickers` | `{"tickers":[Ticker]}` | mark, index, funding, 24 h statistics for every market |
| `GET /v1/markets/{symbol}/ticker` | `Ticker` | |
| `GET /v1/markets/{symbol}/book?depth=50` | `Book` | aggregated by price level; depth 1 to 500 |
| `GET /v1/markets/{symbol}/trades?limit=&before=` | `{"trades":[Trade],"nextCursor"?}` | |
| `GET /v1/markets/{symbol}/candles?interval=1m&from=&to=&limit=` | `{"candles":[Candle]}` | oldest first; intervals `1m 5m 15m 1h 4h 1d`; max 1500 |
| `GET /v1/markets/{symbol}/funding?limit=&before=` | `{"funding":[FundingRate],"nextCursor"?}` | settled hourly rates |
| `GET /v1/oracle/{symbol}` | `OracleStatus` | per-source prices, the median, staleness, halt state |

`Market`:

```json
{"symbol":"BTC-USD","marketId":1,"base":"BTC","quote":"USD","status":"active",
 "priceDecimals":1,"sizeDecimals":4,"tickSize":"0.5","stepSize":"0.0001",
 "minOrderSize":"0.0001","maxOrderSize":"50","maxLeverage":"50",
 "marginTiers":[{"notionalFloor":"0","initialMarginFraction":"0.02","maintenanceMarginFraction":"0.01","maxLeverage":"50"}],
 "makerFeeBps":"1.5","takerFeeBps":"4.5","liquidationFeeBps":"50",
 "fundingIntervalSec":3600,"maxFundingRate":"0.0005",
 "oracle":{"sources":["sourceA","sourceB","sourceC"],"minSources":2,"maxStalenessMs":10000,"maxDeviationBps":"200"},
 "openInterestCap":"1000"}
```

`status`: `active`, `halted` (oracle unsafe or committee halt: no matching, cancels allowed), `closeOnly`
(reduce-only orders and cancels), `settled`.

`Ticker`:

```json
{"symbol":"BTC-USD","markPrice":"67251.2","indexPrice":"67249.8","lastPrice":"67250.5",
 "fundingRate":"0.0000125","nextFundingTime":1794880800000,
 "openInterest":"412.3312","volume24h":"18233412.55","high24h":"68010.0","low24h":"66120.5",
 "price24hAgo":"66800.0","change24h":"0.006744","height":120344,"time":1794878412200}
```

`fundingRate` is the rate that would be charged at `nextFundingTime` if the premium stayed as it is (longs pay
shorts when positive). `volume24h` is in USD.

`Book` (REST snapshot and the first WebSocket message of the `book` channel):

```json
{"symbol":"BTC-USD","seq":88231,"height":120344,"time":1794878412200,
 "bids":[["67250.0","1.2040"],["67249.5","0.3000"]],
 "asks":[["67251.0","0.8000"],["67251.5","2.1150"]]}
```

Levels are `[price, totalSize]`, bids descending, asks ascending. `seq` increases by one per book change batch
(one batch per block that touched the book).

`Trade`: `{"tradeId":"771203","symbol":"BTC-USD","price":"67250.5","size":"0.0120","side":"buy","time":1794878412200,"height":120344,"liquidation":false}`
(`side` is the taker's side).

`Candle`: `{"t":1794878400000,"o":"67240.0","h":"67262.5","l":"67231.0","c":"67250.5","v":"12.4410","q":"836612.11","n":211}`
(`t` open time, `v` base volume, `q` USD volume, `n` trades). The last candle of a response may be still open.

`FundingRate`: `{"symbol":"BTC-USD","time":1794877200000,"rate":"0.0000125","premium":"0.0000110","markPrice":"67190.0","indexPrice":"67188.2"}`

`OracleStatus`:

```json
{"symbol":"BTC-USD","status":"ok","price":"67249.8","time":1794878412000,"round":"553120",
 "sources":[{"name":"sourceA","price":"67250.1","time":1794878411700,"used":true},
            {"name":"sourceB","price":"67249.8","time":1794878411900,"used":true},
            {"name":"sourceC","price":"66100.0","time":1794878411000,"used":false,"reason":"outlier"}],
 "signers":5,"threshold":5,"haltReason":null}
```

`status`: `ok`, `stale`, `insufficientSources`, `deviation`, `halted`. `sources` is what THIS node's adapters see;
`price` is the committee-agreed price last applied by the engine (section 6 of `node/README.md` explains the
difference).

## 3. Node, finality, input log

### 3.1 `GET /v1/status` returns `Status`

```json
{"name":"postern-perps-node","version":"0.1.0","apiVersion":"0.1","mode":"mock","unaudited":true,
 "network":{"eip712":{"name":"PosternPerps","version":"1","chainId":700770,"verifyingContract":"0x…vault"},
            "collateralChainId":700770,"vault":"0x0000000000000000000000000000000000000000",
            "collateralToken":"0x0000000000000000000000000000000000000000","collateralSymbol":"bUSDC","collateralDecimals":6},
 "time":1794878412345,
 "latestBlock":{"height":120344,"time":1794878412200,"hash":"0x…","stateRoot":"0x…","logHash":"0x…"},
 "latestFinalized":{"height":120340,"time":1794878411400,"hash":"0x…","stateRoot":"0x…","logHash":"0x…"},
 "sequencer":{"state":"running","blockIntervalMs":200,"pendingActions":3},
 "committee":{"n":7,"threshold":5,"online":7},
 "vaultMode":"normal","l2":{"finalizedBlock":18233,"lastDepositBlock":18230,"endpointsAgree":true}}
```

`mode`: `mock`, `dev` (single process, throwaway keys), `live`. `sequencer.state`: `running`, `recovering`,
`halted`. `vaultMode`: `normal`, `paused`, `escape`, `unknown`.

### 3.2 Finality

| Method and path | Returns |
|---|---|
| `GET /v1/finality/latest` | `Checkpoint` (the newest with at least `threshold` signatures) |
| `GET /v1/finality/{height}` | `Checkpoint` at or before that height; `404 NOT_FOUND` if none |
| `GET /v1/committee` | `{"threshold":5,"validators":["0x…",…],"setHash":"0x…"}` |

`Checkpoint`:

```json
{"height":120340,"time":1794878411400,"blockHash":"0x…","stateRoot":"0x…","logHash":"0x…",
 "digest":"0x…","threshold":5,"final":true,
 "signatures":[{"validator":"0x…","signature":"0x…(65 bytes r||s||v)"}]}
```

`digest` is the value each validator signs: `docs/VAULT.md` 5.2 `Checkpoint(uint64 height,uint64 time,bytes32 stateRoot,bytes32 logHash,uint64 depositsCredited,bytes32 depositsHead,uint64 forcedProcessed,bytes32 forcedHead,uint64 withdrawalCount,uint64 withdrawalsTotal,bytes32 withdrawalsRoot,uint64 escapeTotal,bytes32 escapeRoot)`
under the vault domain; the checkpoint object carries those 13 fields (u64 totals as decimal strings) plus `digest`,
`threshold`, `final`, `blockHash` and `signatures`. The vault accepts at least `threshold` signatures; the node always
publishes exactly `threshold`. Signatures are sorted by validator address ascending, as the vault requires. Anyone can verify a
checkpoint from `GET /v1/committee` plus this object without trusting the API host.

### 3.3 Input log (published so anyone can replay with the engine CLI)

| Method and path | Returns |
|---|---|
| `GET /v1/blocks/{height}` | `Block` |
| `GET /v1/blocks?from=H&limit=N` | `{"blocks":[Block]}` oldest first, max 500 |
| `GET /v1/blocks/stream?from=H` | `application/x-ndjson`, one `Block` per line (bulk replay) |

`Block`:

```json
{"height":120344,"time":1794878412200,"prevLogHash":"0x…","logHash":"0x…","stateRoot":"0x…",
 "inputs":[{"kind":"action","account":"0x…","type":"PlaceOrder","message":{},"signature":"0x…"},
           {"kind":"oracle","payload":{}},{"kind":"deposit","payload":{}},{"kind":"forced","payload":{}}],
 "inputsEncoded":"0x…","sequencerSignature":"0x…"}
```

`inputsEncoded` is the canonical byte encoding of the block's inputs as `docs/SPEC.md` defines it (what the engine
consumes and what `logHash` commits to: `logHash = keccak256(prevLogHash || height || time || keccak256(inputsEncoded))`);
`inputs` is the same content decoded for people. Heights with no input do not exist (empty blocks are skipped), so
`height` values are consecutive but times are not evenly spaced.

## 4. Accounts (public reads; the exchange is transparent like its model)

`{address}` is the main account (the EIP-712 signer that owns the funds), never an agent wallet.

| Method and path | Returns |
|---|---|
| `GET /v1/accounts/{address}` | `Account` |
| `GET /v1/accounts/{address}/orders?status=open\|all&symbol=&limit=&before=` | `{"orders":[Order],"nextCursor"?}` |
| `GET /v1/accounts/{address}/fills?symbol=&limit=&before=` | `{"fills":[Fill],"nextCursor"?}` |
| `GET /v1/accounts/{address}/funding?symbol=&limit=&before=` | `{"payments":[FundingPayment],"nextCursor"?}` |
| `GET /v1/accounts/{address}/deposits` | `{"deposits":[Deposit]}` |
| `GET /v1/accounts/{address}/withdrawals` | `{"withdrawals":[Withdrawal]}` |
| `GET /v1/accounts/{address}/agents` | `{"agents":[Agent]}` |
| `GET /v1/accounts/{address}/nonce` | `{"account":"0x…","nextNonce":"17","agents":[{"agent":"0x…","nextNonce":"4"}]}` |
| `GET /v1/accounts/{address}/proof` | `AccountProof` against the latest finalized root (escape hatch input) |

`Account`:

```json
{"address":"0x…","height":120344,"time":1794878412200,
 "collateral":"25000.000000","unrealizedPnl":"312.450000","equity":"25312.450000",
 "initialMargin":"4100.000000","maintenanceMargin":"2050.000000",
 "freeCollateral":"21212.450000","withdrawable":"21212.450000","marginRatio":"0.080988",
 "feeTier":{"tier":0,"makerFeeBps":"1.5","takerFeeBps":"4.5","volume30d":"0.00"},
 "positions":[Position],"openOrders":2}
```

`marginRatio` = maintenanceMargin / equity (liquidation at 1 or above). An unknown address returns a zero account,
not 404.

`Position`:

```json
{"symbol":"BTC-USD","size":"0.5000","entryPrice":"66800.0","markPrice":"67251.2",
 "notional":"33625.600000","unrealizedPnl":"225.600000","realizedPnl":"0.000000",
 "liquidationPrice":"18420.5","marginMode":"cross","leverage":"10","isolatedMargin":null,
 "initialMargin":"3362.560000","maintenanceMargin":"336.256000","fundingSinceOpen":"-1.220000"}
```

`liquidationPrice` is an estimate holding every other position still; `null` when none exists.

`Order`:

```json
{"orderId":"9912044","clientOrderId":"0x…|null","account":"0x…","symbol":"BTC-USD","side":"buy",
 "type":"limit","timeInForce":"GTC","price":"67000.0","size":"0.2500","filledSize":"0.1000",
 "avgFillPrice":"67000.0","reduceOnly":false,"triggerPrice":null,"triggerCondition":null,
 "status":"open","createdAt":1794878000000,"updatedAt":1794878100000,"height":120000,"rejectReason":null}
```

`type`: `limit`, `market`, `stopMarket`, `stopLimit`, `takeProfitMarket`, `takeProfitLimit`. `timeInForce`: `GTC`,
`IOC`, `POST_ONLY`. `status`: `untriggered`, `open`, `filled`, `canceled`, `rejected`, `expired`.
`triggerCondition`: `markAbove`, `markBelow`, or null.

`Fill`:

```json
{"fillId":"5512001","tradeId":"771203","orderId":"9912044","account":"0x…","symbol":"BTC-USD","side":"buy",
 "price":"67000.0","size":"0.1000","fee":"3.015000","liquidity":"taker","realizedPnl":"0.000000",
 "liquidation":false,"time":1794878100000,"height":120010}
```

`fee` is in USDC, positive = paid by the account, negative = rebate.

`FundingPayment`: `{"symbol":"BTC-USD","time":1794877200000,"rate":"0.0000125","positionSize":"0.5000","markPrice":"67190.0","payment":"-0.419938"}`
(`payment` negative = paid by the account).

`Agent`: `{"agent":"0x…","name":"trading-bot","approvedAt":1794800000000,"expiresAt":1797392000000,"height":98000}`

`AccountProof`: `{"address":"0x…","checkpoint":Checkpoint,"leaf":"0x…","leafData":{},"proof":["0x…"],"index":"12"}`
(format of `leaf` and `proof`: `docs/SPEC.md`; what the vault does with it: `docs/VAULT.md`).

### 4.1 Deposits and withdrawals

| Method and path | Returns |
|---|---|
| `GET /v1/deposits/{l2TxHash}` | `{"deposits":[Deposit]}` (one transaction can carry several logs) |
| `GET /v1/withdrawals/{withdrawalId}` | `Withdrawal` |
| `GET /v1/forced?account=0x…` | `{"forced":[ForcedAction]}` requests read from the vault queue |

`Deposit`:

```json
{"depositId":"41","account":"0x…","amount":"5000.000000","l2TxHash":"0x…","logIndex":3,"l2Block":18230,
 "status":"credited","creditedHeight":119800,"time":1794878000000}
```

`status`: `seen` (in an L2 block not yet final), `l2Finalized` (final on the L2, both L2 endpoints agree, waiting
for inclusion), `credited` (applied by the engine at `creditedHeight`). A deposit is credited only from the L2's
`finalized` tag; the Bloch L2 finalizes a block per proof, minutes apart, so `seen` can last minutes.

`Withdrawal`:

```json
{"withdrawalId":"77","account":"0x…","recipient":"0x…","amount":"1000.000000","fee":"1.000000",
 "status":"certified","requestedHeight":120100,"checkpointHeight":120140,
 "certificate":{"digest":"0x…","record":{},"checkpoint":Checkpoint,
                "proof":["0x…"],"signatures":[{"validator":"0x…","signature":"0x…"}]},
 "l2TxHash":null,"availableAt":null,"time":1794878200000}
```

Certificate (v0.2 of this document, VAULT.md `withdraw` inputs): `{"record":{"index","account","recipient","amount"},
"proof":{"index","hashes":[…],"sums":[…]},"leaf","digest","checkpoint":Checkpoint,"signatures":[…]}`; `amount` is the
net amount (gross minus the flat fee, SPEC S-9.2) in USDC units.

`status`: `requested` (action accepted), `included` (debited by the engine), `finalized` (in a finalized
checkpoint), `certified` (k-of-n signatures collected: anyone may now submit the certificate to the vault; the
relayer does it by default), `submitted` (L2 transaction seen), `delayed` (the vault holds a large amount until
`availableAt`), `paid`, `rejected`. `certificate` is null before `certified`; its `record`, `digest` and `proof`
are exactly the vault's input (`docs/VAULT.md`), so a user can submit it without any relayer.

`ForcedAction`: `{"forcedId":"3","account":"0x…","kind":"cancelAll|close|withdraw","l2TxHash":"0x…","l2Block":18240,"deadlineTime":1794881800000,"status":"queued|included|expired","includedHeight":null}`

## 5. Actions (writes)

Every state change by a user is an **action**: an EIP-712 typed message signed by the account's main key or by an
agent wallet the main key approved. The typed-data definitions are owned by `docs/SPEC.md`; the node serves them
at `GET /v1/eip712` so clients do not hard-code them:

```json
{"domain":{"name":"PosternPerps","version":"1","chainId":700770,"verifyingContract":"0x…vault"},
 "types":{"PlaceOrder":[{"name":"account","type":"address"}, {"name":"nonce","type":"uint64"}]},
 "actions":["PlaceOrder","CancelOrder","CancelAll","ModifyOrder","UpdateLeverage","UpdateMargin","Withdraw","ApproveAgent","RevokeAgent"],
 "agentAllowed":["PlaceOrder","CancelOrder","CancelAll","ModifyOrder","UpdateLeverage","UpdateMargin"],
 "specVersion":"0.1"}
```

(The `types` shown are abbreviated; the action list above is v0.1 and follows SPEC.md as it changes. `Withdraw`,
`ApproveAgent` and `RevokeAgent` must be signed by the main key; an agent can never move funds out or approve agents.)

### 5.1 `POST /v1/actions`

Request (`ActionRequest`):

```json
{"type":"PlaceOrder",
 "message":{"account":"0x…","nonce":"17","marketId":1,"side":"buy","orderType":"limit","timeInForce":"GTC",
            "price":"67000.0","size":"0.2500","reduceOnly":false,"triggerPrice":"0","clientOrderId":"0x00…"},
 "signature":"0x…(65 bytes)"}
```

- `type` is the EIP-712 primary type; `message` is the typed message with the field names of SPEC.md. Decimal
  fields are given as decimal strings in human units and converted by the node to the scaled integers that are
  actually signed (the client signs the scaled integers: use `GET /v1/eip712` plus the market's `priceDecimals`
  / `sizeDecimals`; a ready-made helper is in `node/mock/sign.mjs`). `"wire":true` in the request declares that
  `message` already holds the scaled integers as decimal strings, and the node then converts nothing.
- The signer is recovered from the signature. It must be `message.account` or one of its approved, unexpired agents.
- `nonce`: per signer, strictly increasing; the engine keeps the highest used and rejects reuse (replay
  protection). Gaps are allowed, so a client may use the millisecond clock.
- Optional query `?wait=included` holds the response until the action is in a block (at most 2 s).

Response `200` (`ActionAck`):

```json
{"actionHash":"0x…","status":"accepted","receivedAt":1794878412300}
```

with `?wait=included`:

```json
{"actionHash":"0x…","status":"included","receivedAt":1794878412300,"height":120345,"index":2,
 "result":{"ok":true,"orderId":"9912050","orderStatus":"open","filledSize":"0","error":null}}
```

`status`: `accepted` (passed admission, queued), `included` (in a block; `result` is the engine's outcome, which
may still be a business rejection such as insufficient margin: `result.ok=false` with `result.error`), `rejected`
(never entered a block). An `accepted` action is a promise by the sequencer only; see the trust model in
`node/README.md`.

`GET /v1/actions/{actionHash}` returns the same object later (`404` once pruned; kept at least 24 h).

Admission refusals (HTTP 4xx, `Error`): `GEO_RESTRICTED` (451), `BAD_SIGNATURE`, `UNKNOWN_SIGNER`,
`AGENT_NOT_ALLOWED`, `BAD_NONCE`, `MALFORMED`, `TOO_LARGE` (413), `RATE_LIMITED` (429), `MARKET_HALTED`,
`SEQUENCER_UNAVAILABLE` (503).

### 5.2 Agent (API) wallets

An agent wallet is a second key that may trade for an account but can never withdraw or approve other agents.

1. The frontend generates a key pair locally (the node never sees a private key).
2. The MAIN key signs `ApproveAgent{account, agent, name, expiresAt, nonce}` and posts it to `POST /v1/actions`.
3. From inclusion on, actions signed by the agent with `message.account` = the main account are admitted.
4. `RevokeAgent{account, agent, nonce}` (main key) ends it; expiry ends it too. `GET /v1/accounts/{address}/agents` lists them.

### 5.3 Withdrawals

`Withdraw{account, amount, recipient, nonce}` (main key only) through `POST /v1/actions`. Follow it at
`GET /v1/withdrawals/{withdrawalId}` (the id is in the action's `result.withdrawalId`) or on the `account` channel.

## 6. Geoblocking

`GET /v1/geo` returns `GeoStatus`:

```json
{"country":"BR","restricted":true,"policy":"actions_refused","listVersion":"0.1","reasonCode":"JURISDICTION",
 "message":"PosternDEX Perpetuals is not available in your jurisdiction.","sanctioned":false}
```

- The list is `compliance/geoblock.json` (agent 354); the node only enforces it. Everything legal about it is for
  counsel, not decided here.
- `POST /v1/actions` (and the WebSocket `action` op) from a restricted country is refused with HTTP `451` and
  `{"error":{"code":"GEO_RESTRICTED","message":"…","details":{"country":"BR","listVersion":"0.1"}}}`.
  **v0.1 rule: every action type is refused**, except `CancelOrder`, `CancelAll`, reduce-only `PlaceOrder` and
  `Withdraw`, which stay open so that a user who becomes restricted can always leave ("exit-only";
  `policy` says which rule is active: `actions_refused` | `exit_only` | `open`). 354 may change this rule.
- Public market data, node status and account reads are NOT geoblocked in v0.1.
- The country comes from a header set by the operator's trusted reverse proxy (configured name, accepted only
  from configured proxy addresses) or from a local IP-country database; if the country cannot be determined the
  node treats the request as restricted in `live` mode (fail closed) and reports `country: null`.
- Mock and dev mode: the header `X-Mock-Country: BR` or the query `?country=BR` sets the country for a request
  (for frontend tests). Ignored in `live` mode.
- Geoblocking at this API is a control of the operator's access point. The L2 forced-action path (cancel, close,
  withdraw) is a contract anyone can call and is not, and cannot be, geoblocked by the node.

## 7. Error codes

| HTTP | code | Meaning |
|---|---|---|
| 400 | `MALFORMED` | body or parameter does not parse or violates the schema (`details.path`) |
| 400 | `BAD_SIGNATURE` | signature does not recover / wrong length / high-s |
| 400 | `BAD_NONCE` | nonce not above the signer's highest used |
| 403 | `UNKNOWN_SIGNER` | signer is neither the account nor an approved agent |
| 403 | `AGENT_NOT_ALLOWED` | action type requires the main key |
| 404 | `NOT_FOUND` | unknown market, height, id |
| 409 | `MARKET_HALTED` | market does not accept this action now |
| 413 | `TOO_LARGE` | body over 16 KiB |
| 429 | `RATE_LIMITED` | see `Retry-After` |
| 451 | `GEO_RESTRICTED` | section 6 |
| 451 | `SANCTIONED` | address on the operator's sanctions list (354) |
| 500 | `INTERNAL` | |
| 503 | `SEQUENCER_UNAVAILABLE` | recovering or halted |

Engine-level outcomes (inside `result.error`, HTTP 200): the engine's own error names from SPEC.md, passed through
unchanged (for example `INSUFFICIENT_MARGIN`, `POST_ONLY_WOULD_CROSS`, `REDUCE_ONLY_VIOLATION`, `ORDER_NOT_FOUND`).

## 8. Default limits

| Limit | Default |
|---|---|
| Actions per account | 20 per second, burst 40 |
| Actions per IP | 50 per second, burst 100 |
| REST reads per IP | 100 per second |
| WebSocket connections per IP | 20; subscriptions per connection 100 |
| Request body | 16 KiB |
| WebSocket message in | 16 KiB; out queue 1024 messages, then the connection is closed (`SLOW_CONSUMER`) |

## 9. WebSocket (`/v1/ws`)

Text frames, one JSON object each. The server pings every 15 s (protocol ping) and closes after 60 s without any
frame from the client; clients may send `{"op":"ping"}` and get `{"op":"pong","time":…}`.

Client to server:

```json
{"op":"subscribe","id":1,"channel":"book","symbol":"BTC-USD","depth":50}
{"op":"unsubscribe","id":2,"channel":"book","symbol":"BTC-USD"}
{"op":"action","id":3,"request":{"type":"PlaceOrder","message":{},"signature":"0x…"}}
{"op":"ping"}
```

Server to client:

```json
{"op":"subscribed","id":1,"channel":"book","symbol":"BTC-USD"}
{"op":"unsubscribed","id":2,"channel":"book","symbol":"BTC-USD"}
{"op":"actionResult","id":3,"ack":{"actionHash":"0x…","status":"accepted","receivedAt":1794878412300}}
{"op":"error","id":3,"error":{"code":"GEO_RESTRICTED","message":"…","details":{}}}
{"channel":"book","symbol":"BTC-USD","type":"snapshot","data":{}}
```

Every data message is `{"channel","type","data"}` plus `symbol`, `interval` or `account` where the channel has one
(`WsMessage`).

| Channel | Subscribe parameters | Messages |
|---|---|---|
| `book` | `symbol`, `depth` (default 50) | `snapshot`: `Book`. `delta`: `BookDelta` |
| `trades` | `symbol` | `snapshot`: `{"trades":[Trade]}` (last 50, newest first). `update`: `{"trades":[Trade]}` (the block's trades, oldest first) |
| `ticker` | `symbol` (or `"*"` for all) | `update`: `Ticker`, at most one per market per second, and at once on funding |
| `candles` | `symbol`, `interval` | `snapshot`: `{"candles":[Candle]}` (last 300). `update`: `Candle` (the open candle, or a newly closed one) |
| `account` | `account` (address) | `snapshot`: `AccountSnapshot`. `update`: `AccountUpdate` |
| `status` | none | `update`: `{"latestBlock":…,"latestFinalized":…}` on every block and every new checkpoint |

`BookDelta`:

```json
{"symbol":"BTC-USD","seq":88232,"prevSeq":88231,"height":120345,"time":1794878412400,
 "bids":[["67250.0","0.9040"]],"asks":[["67251.0","0"]]}
```

A level with size `"0"` is removed; any other size replaces the level. If `prevSeq` is not the `seq` the client
holds, it missed a message: resubscribe (the server sends a fresh snapshot). Deltas cover the whole book, not only
the subscribed depth.

`AccountSnapshot`: `{"account":Account,"orders":[Order],"agents":[Agent]}` (open orders only).

`AccountUpdate` (only the keys that changed are present):

```json
{"height":120345,"time":1794878412400,
 "account":Account,
 "orders":[Order],
 "fills":[Fill],
 "funding":[FundingPayment],
 "deposits":[Deposit],
 "withdrawals":[Withdrawal],
 "actions":[ActionAck]}
```

`orders` holds each order whose state changed in the block (including the final `filled` / `canceled` state).

## 10. Mock mode

Two servers implement this API from the same fixtures (`node/fixtures/`), for frontend development and tests:

- `node/mock/server.mjs`: Node.js 20 or newer, no dependencies, runs on the Mac. See `node/mock/README.md`.
- `postern-perps-node --mock`: the Rust node in one process with the same fixtures (built on the build host only).

Both are deterministic for a given `--seed` and start time (`--start-time`, default fixed), simulate market
activity (a random walk of the index, makers quoting around it, takers trading, hourly funding), hold a demo
account with positions, orders, fills, funding and a deposit and a withdrawal in each status, accept actions
(signature checked for real, simplified matching in the Node.js mock), and honour `X-Mock-Country`. In mock mode
`Status.mode` is `mock` and checkpoints are signed by seven throwaway keys derived from the seed.

## 11. Versioning

`apiVersion` is `major.minor`. Additive changes (new fields, endpoints, enum values in responses) raise the minor
version; clients must ignore unknown fields. Anything else raises the major version and the path prefix.

| Version | Date | Change |
|---|---|---|
| 0.1 | 2026-11-17 | first version, written before `docs/SPEC.md` exists: action names and typed fields are provisional and follow the spec |
| 0.2 | 2026-11-17 | follows SPEC 0.1.1 and VAULT.md 0.1: engine domain chainId 700770 + verifyingContract = vault; FIXED scales (price and size 1e-8, USDC 1e-6; per-market decimals are display only); nonce rule SPEC 2.5 (window and 64-set, errors NONCE_WINDOW / NONCE_USED inside `BAD_NONCE` admissions); checkpoint = VAULT 5.2 13 fields; certificate = VAULT `withdraw` inputs (sum-tree proof); block input tags 0x01-0x04 (SPEC 3.2) |

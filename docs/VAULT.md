# VAULT.md: PerpsVault, the collateral custody of PosternDEX Perpetuals (v0.2)

**STATUS: PROTOTYPE, UNAUDITED. Not deployed, not for deployment.** The perpetuals are not deployed this round (legal
review first). Owner: agent 390 (continues 352). Source: `contracts/src/PerpsVault.sol`; tests: `contracts/test/`.
v0.2 (2026-11-17) adopts SPEC.md 0.1.1 section 11 byte for byte (trees, leaves, state root) and fixes reviewer 395's
R395-23..27 (section 11 below). Shared vectors: `contracts/test/vectors/vault-vectors.json` (pinned by
`contracts/test/Vectors.t.sol`; the withdrawal leaf and the empty depth-32 root were re-derived independently with
`cast keccak`). The engine (388) and the node (389) must reproduce every output there.

## 1. What it is

One contract on the Bloch L2 (EVM, chain id 700770) holding the collateral token bUSDC (6 decimals; the bridge's
`BridgedStablecoinV3`). The exchange engine runs off-chain (sequencer + validator committee, BRIEF-PERPS item 2). The
vault never trusts the sequencer. Funds leave only:

1. against a **checkpoint** signed by `threshold` of a validator committee fixed at deployment, through a **withdrawal
   record** of the engine's withdrawal tree proven by a depth-32 Merkle proof, or
2. in **escape mode**, against the last accepted checkpoint's account tree (`escapeValue`, pro rata), or as a refund
   of a deposit the last checkpoint did not credit (from the vault's own records, no committee input).

There is no owner, admin, upgrade, setter, signer replacement, rescue or administrator mint. Every limit is fixed in
the constructor. Protocol fees are not taken by the vault: they accrue in the engine to the keyless system account
`PROTOCOL_FEE` (`0x…0001`, SPEC 8), whose records and escape share the vault pays only to the owner's address
`0x775a993F09Bd80aeCF8F8c2ADe0C84158E3a0544` (immutable `feeRecipient`).

## 2. Reused from the bridge's reviewed vault (`ProvenanceSourceVault`, bloch-l2-bridge `cc2511a`)

| Pattern | Bridge source | Here |
|---|---|---|
| Certificate check: `threshold..n` signatures, 65 bytes, `s <= n/2`, `v` 27/28, recovered signers strictly ascending and in the set | `_checkCertificate` | `_checkCertificate`, byte-identical logic; digest is EIP-712 instead of eth_sign (389's request) |
| Set-once committee: sorted, unique, no replacement | constructor | constructor; additionally `2k > n` (two quorums always share a signer) |
| Token bucket, continuous refill, no window boundary that lets twice the limit through | `availableToRelease` | same function |
| Guardians: fixed, disjoint from signers, pause only, one pause per guardian per chain of pauses, chain <= g * `pauseDuration`, then >= `pauseCooldown` open; early unpause by a quorum above the release quorum | `guardianPause`, `unpause` | same; `unpauseThreshold >= threshold` (launch: 6 of 7 > 5 of 7, so the keys that can certify cannot also end the pause meant to stop them) |
| Balance-delta checks on every token transfer (fee-on-transfer, false return, wrong amount refused) | `deposit`, `_transferOut` | same, and `_call` refuses a `false` return |
| One reentrancy guard on every state-changing entry point | `nonReentrant` | same |
| Immutable fee recipient, never a guardian, never the token or the vault | constructor, R310-2 | constructor |
| Recipient never zero, the vault or the token (F171-7 class) | `_checkRelease` | `withdraw`, `claimEscape` |
| Ids bound to their position so one item cannot take another's slot (F111-F1 class) | deposit id top bytes = nonce | withdrawal leaf binds `withdrawalId`, the proof's position is that id; account leaf binds `index`, its position |
| Deposit hash chain checked against what the other side credited (D6 / mint chain) | `depositHead`, `provenanceMatches` | `depositHead`; a checkpoint must name the vault's own head after the deposits it credited |

Not reused, with the reason: the light-client switch (no proof system for the engine exists; a later step), the
`blockhash` context anchor (the vault is on the chain it pays on; a checkpoint is bound by strictly increasing height
and time, the domain, and the vault's own chain heads), `setControls` (no committee switch of deposits or payouts:
only bounded guardian pauses and escape; strictly fewer powers), the release queue with forged-mint freeze (replaced
by the large-amount delay with guardian pause; see section 9, gap 3).

## 3. Constructor (all immutable)

`constructor(Config c)`, `Config = (address token, address feeRecipient, address[] validators, uint8 threshold,
uint8 unpauseThreshold, uint64 cap, uint64 minDeposit, uint64 windowLimit, uint64 windowSeconds, uint64
largeThreshold, uint64 largeDelay, uint64 forcedDelay, uint64 escapeDelay, address[] guardians, uint64
pauseDuration, uint64 pauseCooldown)`.

Refused (InvalidConfiguration) unless: token has code and `decimals() == 6`; `feeRecipient` nonzero, not the token,
not the vault, not a guardian; 1 <= n <= 16 validators, sorted strictly ascending; `2 <= threshold <= n` and
`2 * threshold > n`; `threshold <= unpauseThreshold <= n`; `cap > 0`; `0 < minDeposit <= cap`;
`0 < windowLimit <= cap`; `windowSeconds > 0`; `0 < largeThreshold <= windowLimit`; `0 < largeDelay <= 14 days`;
`1 day <= forcedDelay <= 7 days` (R395-25: it must exceed the worst L2 finality lag accepted); `forcedDelay <=
escapeDelay <= 30 days`; `feeRecipient` above `0xffff`; guardians:
at most 7, sorted, not validators; no guardians means `pauseDuration == pauseCooldown == 0`; otherwise
`0 < pauseDuration <= 14 days` and `pauseCooldown >= pauseDuration`.

Proposed launch values (technical proposal; caps and windows are owner decisions, listed in section 10):
7 validators, threshold 5, unpauseThreshold 6, 3 guardians, pauseDuration 2 days, pauseCooldown 2 days,
cap 1,000,000 USDC, minDeposit 10 USDC, windowLimit 250,000 USDC per 1 day, largeThreshold 50,000 USDC,
largeDelay 1 day, forcedDelay 2 days, escapeDelay 7 days (`contracts/deploy/vault.config.json`).

## 4. ABI

| Function | Who | Effect |
|---|---|---|
| `deposit(uint64 amount, address account) returns (uint64 index)` | anyone, after `approve` | Pulls `amount` (exact balance delta), credits `account` in the engine after L2 finality. Refused in escape, while paused, below `minDeposit`, above `cap` of custody, for the vault, the token or any keyless account (`<= 0xffff`, so SPEC 8's system accounts cannot be funded through the vault in v0.2: nothing could refund them). |
| `requestForced(uint8 kind, uint64 arg) returns (uint64 index)` | an account that deposited, for itself | SPEC 14 kinds: 0 CancelAll, 1 CloseAll, 2 Withdraw (`arg` = gross amount, pays the account itself); `arg` must be 0 otherwise. `deadline = block.timestamp + forcedDelay` (L2 seconds, counted from the request's own block). One open request per account. Refused in escape. |
| `submitCheckpoint(Checkpoint cp, bytes[] signatures)` | anyone (a relayer) | Accepts the checkpoint (rules in 5.3). Refused in escape. Not stopped by a pause. |
| `withdraw(uint64 seq, Withdrawal w, bytes32[] siblings, uint64 want) returns (bytes32 key)` | anyone | `Withdrawal = (uint64 withdrawalId, address account, address recipient, uint64 amount, uint64 height)`. Registers the record proven against accepted checkpoint `seq` (1-based; `withdrawalId < withdrawalCountAt[seq]`) and pays up to `want` to `recipient` now if ready, not paused and the bucket allows; otherwise it stays registered. |
| `release(bytes32 key, uint64 want)` | anyone | Pays min(`want`, remainder, bucket) of a registered payout to its recipient. Reverts `NotRegistered`, `Paused`, `NotReady`, `RateLimited` (bucket empty), `InvalidWithdrawal` (nothing left or `want` 0). |
| `guardianPause()` | a guardian | Pauses payouts and deposits for `pauseDuration` (chain rules of section 2). |
| `unpause(uint64 count, uint256 validUntil, bytes[] signatures)` | anyone with `unpauseThreshold` signatures | Ends pause `count` early; guardians then wait `pauseCooldown`. |
| `triggerEscape()` | anyone | Enters escape (one way) if `escapeReason() != 0`. |
| `claimEscape(Account a, bytes32[] siblings, uint64 want) returns (bytes32 key)` | anyone, pays the account | `Account = (address account, uint32 index, int128 balance, uint64 escapeValue, bytes32 positionsHash)` (SPEC 0.1.1 leaf). In escape, after `proveSystemShares`: registers `escapeValue * escapePayable / senior` (rounded down; `senior = totalEscape - insurance - backstop`) and pays up to `want` now. PROTOCOL_FEE's share pays `feeRecipient`; INSURANCE and BACKSTOP cannot claim (their remainder is registered by `proveSystemShares`). |
| `proveSystemShares(Account insurance, bytes32[] s1, Account backstop, bytes32[] s2)` | anyone, once, in escape | Proves INSURANCE (`0x…02`, index 1) and BACKSTOP (`0x…03`, index 2) leaves; fixes the user payout ratio and registers the subordinated remainder to `feeRecipient`, ready 90 days after escape began (R395-33). Required before any `claimEscape` and before the surplus sweep. |
| `refundDeposit(uint64 index, address account, uint64 amount)` | anyone, pays `account` | In escape: refunds deposit `index >= latest.depositsCredited`, verified against the vault's own chain. No bucket, delay or pause (no committee input). |
| `sweepEscapeSurplus() returns (uint64)` | anyone, pays `feeRecipient` | In escape, 90 days after it began, once: pays what no claim can reach (rounding, donations). Every amount still claimable (including unproven escape shares) stays. |

Views: `custody()`, `reserved()`, `availableToRelease()`, `paused()`, `inEscape()`, `escapeReason()` (0 none,
1 silence, 2 overdue forced request), `depositHead(n)`, `forcedHead(n)`, `depositTotalAt(n)`, `forcedDeadline(i)`,
`latest()`, `checkpointCount()`, `withdrawalsRootAt(seq)`, `withdrawalCountAt(seq)`, `withdrawalsTotalAt(seq)`,
`payouts(key)`, `withdrawalKey(id)`, `escapeKey(account)`, `checkpointDigest(cp)`, `stateRootOf(cp)`,
`unpauseDigest(count, validUntil)`, `domainSeparator()`, `escapePayable()`. Leaf, node and chain-link formulas are
internal (contract size) and given exactly in section 5 and the vectors.

Events (exact signatures, for the node's watcher 389):

```
Deposit(uint64 indexed index, address indexed account, address indexed sender, uint64 amount, bytes32 head)
ForcedRequest(uint64 indexed index, address indexed account, uint8 kind, uint64 deadline, uint64 arg, bytes32 head)
CheckpointAccepted(uint64 indexed seq, uint64 indexed height, bytes32 stateRoot, bytes32 withdrawalsRoot)
PayoutRegistered(bytes32 indexed key, address indexed recipient, uint64 total, uint64 readyAt)
Paid(bytes32 indexed key, address indexed recipient, uint64 amount)
Refunded(uint64 indexed index, address indexed account, uint64 amount)
GuardianPaused(address indexed guardian, uint64 indexed pauseCount, uint64 pausedUntil)
Unpaused(uint64 indexed pauseCount, uint64 guardianBlockedUntil)
EscapeTriggered(uint64 at, uint8 reason, uint64 checkpointSeq, uint64 payable_, uint128 totalEscape)
SurplusSwept(address indexed recipient, uint64 amount)
```

Amounts are bUSDC base units (6 decimals) as `uint64` (max 1.8e13 USDC). Times are L2 `block.timestamp` seconds,
except `Checkpoint.time`, which is engine time in milliseconds (informational to the vault; only its strict increase
is checked, because the L2 clock can lag the engine by minutes).

## 5. Formats (the bytes validators sign and the engine commits)

### 5.1 EIP-712 domain

`EIP712Domain(string name,string version,uint256 chainId,address verifyingContract)` with name `PosternPerpsVault`,
version `1`, `chainId = block.chainid` (700770 in production; computed at every call, so a chain fork with another id
refuses old signatures), `verifyingContract` = the vault. This is a different domain from the user actions'
`PosternPerps`/`1`/7007701 (no verifyingContract): a user signature can never be a checkpoint signature.

### 5.2 Checkpoint

```
Checkpoint(uint64 height,uint64 time,bytes32 logHash,bytes32 accountsRoot,bytes32 marketsRoot,bytes32 withdrawalsRoot,uint64 nextDepositIndex,uint64 nextForcedIndex,uint64 nextOrderId,uint128 totalEscape,uint128 dust,bytes32 auxHash,bytes32 depositsHead,bytes32 forcedHead,uint64 withdrawalCount,uint64 withdrawalsTotal)
```

digest = `keccak256(0x19 0x01 || domainSeparator || keccak256(abi.encode(CHECKPOINT_TYPEHASH, 16 fields...)))`
(17 words). The first twelve fields are exactly the inputs of SPEC 11's `stateRoot`; the vault recomputes it
(`stateRootOf`) and stores and emits it, so the roots it pays against are the ones inside the state root the
validators replayed:

```
systemHash = keccak256("PPv1sys" || be64 nextDepositIndex || be64 nextForcedIndex || be64 nextOrderId || be128 totalEscape || be128 dust || auxHash)
stateRoot  = keccak256("PosternPerps/state/v1" || be64 height || be64 time || accountsRoot || marketsRoot || withdrawalsRoot || systemHash)
```

The last four fields are outside the state root: `depositsHead` / `forcedHead` are the vault's own chain heads after
`nextDepositIndex` / `nextForcedIndex` items (validators compute them from the vault's events at the L2 finalized
tag; the vault compares them with what it accumulated itself, R395-1, coordinator decision 2026-11-17);
`withdrawalCount` / `withdrawalsTotal` are the number and sum of records in the withdrawal tree (388: please add
both to `systemHash` in SPEC 0.2 so they are inside the state root too).

### 5.3 Acceptance rules (`submitCheckpoint`)

Refused (`InvalidCheckpoint`, `Insolvent` or `InvalidCertificate`) unless all hold:

1. not in escape; `height > 0`; after the first, `height` and `time` strictly above the latest's;
2. `latest.nextDepositIndex <= nextDepositIndex <= depositCount` and `depositsHead == depositHead(nextDepositIndex)`;
3. `latest.nextForcedIndex <= nextForcedIndex <= forcedCount` and `forcedHead == forcedHead(nextForcedIndex)`;
4. no request is left past its deadline: `nextForcedIndex == forcedCount` or
   `block.timestamp <= forcedDeadline[nextForcedIndex]`;
5. withdrawals append-only as far as the vault can see: count and total never decrease; after the first checkpoint
   the same count means the same root and the same total; count 0 means total 0;
6. solvency: `(withdrawalsTotal - withdrawnTotal) + (depositedTotal - depositTotalAt[nextDepositIndex]) <= custody()`
   (escape values are NOT in this test since v0.2, R395-23: they are paid pro rata, section 7);
7. `threshold` valid signatures (section 2) over the digest.

### 5.4 Chains

```
depositHead(0) = keccak256(abi.encode(bytes32("PosternPerps.deposit.v1"), chainid, vault))
depositHead(i+1) = keccak256(abi.encode(depositHead(i), uint256(i), address account, uint256(amount)))
forcedHead(0)  = keccak256(abi.encode(bytes32("PosternPerps.forced.v1"), chainid, vault))
forcedHead(i+1) = keccak256(abi.encode(forcedHead(i), uint256(i), address account, uint256(kind), uint256(deadline), uint256(arg)))
```

`bytes32("...")` is the ASCII text left-aligned and zero-padded. Each event carries the new head, so the watcher checks
its own computation per event; the engine credits `account` (never `sender`). SPEC 3.2's input payloads carry the same
fields (tag 0x02: index, account, amount; tag 0x03: index, account, kind, deadline, arg).

### 5.5 Merkle trees (SPEC 11, adopted)

```
leaf  = keccak256(0x00 || data)        node = keccak256(0x01 || left || right)      Z(0) = 0x00..00, Z(i+1) = node(Z(i), Z(i))
withdrawal data = "PPv1wdrl" || be64 withdrawalId || account:20 || recipient:20 || be64 amount || be64 height
account data    = "PPv1acct" || account:20 || be32 index || be128 balance (two's complement) || be64 escapeValue || positionsHash:32
```

Both trees have depth 32; a proof is exactly 32 siblings, bottom-up; bit `i` of the position (withdrawalId, account
index) set means the running node is the right child at level `i`; positions are below 2^32.

Why v0.2 dropped v0.1's Merkle sum trees (the argument the brief requires for removing a property): a sum tree
guaranteed that the leaves of a signed root add up to the signed total, so no claimant could find the money gone. That
only matters when the committee signs a dishonest tree, and a committee that does so can equally give itself every
leaf; the vault's real guarantees do not depend on it and are all kept: the sum of registered withdrawal records is
bounded by `withdrawalsTotal` (counter), escape payouts by `escapePayable <= pool` (counter), and `owed <= custody`
at every state (invariant test). What was gained: one tree format with the engine, roots bound into the replayed
state root (which v0.1 did not check at all), and proofs taken directly from the engine's `account_proof` /
`withdrawal_proof`. Reviewer 395 asked for exactly this alignment (R395-27).

### 5.6 Unpause

`Unpause(uint64 pauseCount,uint256 validUntil)` under the same domain; bound to the pause being ended.

## 6. Payout rules

- **Registration** (`withdraw`, `claimEscape`) proves the item, checks the totals (sum of registered withdrawal
  records <= the newest `withdrawalsTotal`; sum of escape payouts <= `escapePayable`), and records `Payout(recipient,
  total, paid 0, readyAt)`, `readyAt = now + largeDelay` if `total >= largeThreshold`, else now. A key registers once
  (`withdrawalKey(index)`, `escapeKey(account)`): a record paid against one checkpoint cannot be paid against a later
  one.
- **Payment** (`release`, or at registration) pays the smallest of the ask, the remainder and the bucket. A payout
  above the window limit is therefore paid in parts (no item is ever unpayable because of its size, unlike the
  bridge's burn above `bucketCapacity`); a record at or above `largeThreshold` waits its delay for every part
  (splitting does not bypass the delay).
- **Fee account**: a withdrawal record of `PROTOCOL_FEE` (`0x…0001`) must have `recipient == feeRecipient`; no
  record of another keyless account, and no record to a keyless recipient (`<= 0xffff`), is paid.
- A guardian pause stops payments and deposits only. It never stops checkpoint acceptance (a guardian could otherwise
  starve forced requests into escape), forced requests, the escape trigger, registration or refunds.

## 7. Forced actions and escape

**Forced actions.** The sequencer can censor or reorder user actions submitted to the API. A user who deposited can
post `requestForced` on the L2 (not geoblockable by the node). The engine must process every request, in index order,
before its deadline (`forcedDelay` after the L2 block; proposal 1 day, which covers L2 finality of minutes plus
inclusion and a checkpoint); the vault refuses any checkpoint that leaves an overdue request unprocessed, and once one
is overdue anyone may trigger escape. The vault checks only that the committee acknowledges processing (count and
chain head); that the engine actually cancelled, closed or withdrew is a property of the state root that every
validator checks by replay (SPEC.md must state the processing rule: 388). Spam bound: one open request per account and
only accounts that deposited at least `minDeposit` once.

**Escape** (one way) is due when a forced request is past its deadline unprocessed by the latest checkpoint, or,
once at least one checkpoint was accepted, when `block.timestamp >= lastCheckpointAt + escapeDelay` (R395-24: a
vault deployed before launch cannot be pushed into escape by silence; a depositor whose deposit the committee never
credits still exits through a forced request). At the trigger the vault fixes `pool = custody - (withdrawalsTotal -
withdrawnTotal) - (depositedTotal - depositTotalAt[nextDepositIndex])` and `escapePayable = min(pool,
totalEscape)`. In escape: checkpoints, deposits and forced requests stop; certified withdrawal records stay payable
in full; deposits with index >= `nextDepositIndex` are refunded in full from the vault's own chain; each account
claims from the latest account tree. **Subordination (coordinator decision on R395-33):** the INSURANCE and BACKSTOP
escape values cover user shortfalls first. Anyone proves their two leaves once (`proveSystemShares`); then
`senior = totalEscape - insurance - backstop`, `escapePayable = min(pool, senior)`, each user (and PROTOCOL_FEE)
receives `floor(escapeValue * escapePayable / senior)`, i.e. in full whenever the pool covers the users, pro rata
only for a shortfall beyond the system shares; the system accounts' remainder `min(insurance + backstop, pool -
escapePayable)` is one payout to `feeRecipient` that cannot be released before 90 days after escape began. After 90
days the surplus (rounding, donations, value the tree never assigned) can be swept once to `feeRecipient`; the sweep
is refused until the system shares are proven, and never touches an unclaimed user value.
Failing-first demonstration: on `27c1969`, 395's scenario (pool 2,000; users 1,500 + 400; insurance 250, backstop
50) paid alice 1,363.636363 instead of 1,500 (test run recorded in the commit); the three `R395_33` tests pass now. A forced request the engine could not execute must not
be counted as processed (coordinator decision on R395-5: not consumed), so it runs into its deadline and opens
escape; the vault sees only the count and head, so that rule lives in SPEC 14 and the validators (388/389).

**Solvency proof (the property the tests check).** Let `owed = (escapePayable - escapeClaimedTotal) +
(withdrawalsTotal - withdrawnTotal) + (depositedTotal - depositTotalAt[nextDepositIndex] - refundedTotal)` for the
latest checkpoint (`escapePayable` is 0 before escape). (a) At acceptance `owed <= custody()` (rule 6); at the
trigger `escapePayable <= pool` keeps it. (b) Deposits raise `custody` and the uncredited term equally;
registrations and refunds lower both equally; the surplus sweep lowers `custody` to exactly the unregistered
remainder. (c) `balance >= custody() + reserved` at every state (transfers are balance-checked). Hence every claim the
latest checkpoint admits can be registered and every registered payout paid; the escape payouts sum to at most the
pool (rounding down).

## 8. Trust model (honest list)

- **The committee (k of n) is trusted for safety.** A colluding quorum can sign a checkpoint whose trees pay itself
  (bounded by custody through rule 6, by the bucket per window and by the large-amount delay that gives guardians time
  to pause). It cannot exceed the limits, change them, pay a fee-account record elsewhere, or make the vault promise
  more than it holds. Validators replaying the engine (BRIEF-PERPS item 2) are the protection against a forged root.
- **The sequencer** cannot move funds (it signs nothing the vault reads) and is bounded in censorship by forced
  requests and escape.
- **Guardians** can delay payouts for at most g * `pauseDuration` per chain (proposal: 3 x 2 days), then at least
  `pauseCooldown` open; they can never move funds, stop checkpoints, stop forced requests or stop escape.
- **Liveness**: if the committee is offline for `escapeDelay`, or any depositor's forced request goes unprocessed past
  its deadline, the venue ends in escape for good (by design: the users' exit is the guarantee). An attacker who
  deposits `minDeposit` can force that outcome during any committee outage longer than `forcedDelay`.
- **Escape at a bad checkpoint**: the escape tree is the committee's; a colluding quorum that stops signing chooses
  the tree users escape with (same trust as above).
- **Underwater accounts** (R395-23): checkpoints stay acceptable; in escape everyone shares the shortfall pro rata
  unless the engine's escape values already charge it to INSURANCE first (SPEC 6.3 order; 388).
- **A checkpoint with height or time at `2^64 - 1`** blocks every later checkpoint (they must be higher): the venue goes
  to escape after `escapeDelay`. Only a quorum can sign it; validators must refuse such fields (SPEC.md bound).

## 9. Gas and size (Foundry 1.7.1, solc 0.8.24, optimizer runs 1, shanghai; Mac, 2026-11-17)

Runtime 18,600 bytes (EIP-170 limit 24,576); creation 4,291,195 gas with 7 validators and 3 guardians (the L2's
per-transaction cap is 15,000,000, bloch-l2-evm `docs/L2-MAINNET-GO-LIVE-V21.md` K8; the test keeps the older
5,000,000 bound). Per call (gas report of the unit suite, median): `deposit` 186,746; `submitCheckpoint` 272,730 (5
signatures); `withdraw` 68,860 (register + pay, depth-32 proof; max 180,810); `release` 75,628; `claimEscape`
159,616; `refundDeposit` 39,074; `requestForced` 49,610; `triggerEscape` 66,346; `guardianPause` 54,294; `unpause`
55,641 (6 signatures). Not measured on the Bloch L2 itself.

## 10. Tests (contracts/, `forge test --offline`)

53 tests: 40 unit/fuzz, 3 R395-33 subordination (`PerpsVault.t.sol`: configuration refusals; deposits and hostile tokens; checkpoint
signatures, domain, fields, state-root equation, append-only, solvency, R395-23 pro rata; withdrawals: tamper,
positions, siblings, depth, old checkpoints, fee account, recipients, large delay, bucket parts; guardians;
forced requests; escape: overdue, silence rules, claims, refunds, certified records, parts, keyless shares,
unproven shares, surplus; re-entrancy; tree round-trip fuzz), 2 silence/keyless tests, 5 invariants (128 runs x 64
calls, handler with an honest engine model and a colluding committee), 1 escape end-to-end fuzz, 1 creation-gas test,
1 vector test.

## 11. Findings addressed

| ID | Status in v0.2 |
|---|---|
| R395-1 (vault half) | deposit and forced chain heads checked against the vault's own accumulation (since v0.1) |
| R395-18 | pool net of records and uncredited deposits; system accounts' shares defined |
| R395-23 | escape values out of the acceptance test; pro-rata pool; test `test_R395_23_...` |
| R395-24 | silence escape only after a first checkpoint; test `test_escape_after_silence_and_what_stops` |
| R395-25 | `forcedDelay >= 1 day`, proposal 2 days; finality-based start not measurable on chain (stated) |
| R395-26 | keyless addresses never paid; shares to `feeRecipient`; keyless deposits refused |
| R395-33 | insurance and backstop subordinated to users in escape; remainder to `feeRecipient` only after 90 days; tests `test_R395_33_*` |
| R395-27, R395-30 (vault side) | formats = SPEC 0.1.1; vectors file; node must adopt the event signatures and the 16-field checkpoint |

## 12. Not done / not proven

1. UNAUDITED; no external review yet (agent 395 is the internal reviewer).
2. The engine side must match the vectors (388) and the node must sign the 16-field checkpoint, decode these events
   and compare forced deadlines with L2 time in seconds (389); not yet checked end to end.
3. No on-chain cancel of a fraudulent queued large payout (guardians can only pause); a proof-of-fraud freeze (two
   conflicting checkpoints at one height) would be a strict strengthening, not built.
4. No validity proof of the engine; the committee is trusted (section 8).
5. Tested with a 6-decimal mock token that has bUSDC's transfer semantics, not with `BridgedStablecoinV3` bytecode on
   a private Bloch L2; the deploy script was not yet rehearsed on anvil.
6. INSURANCE/BACKSTOP escape shares and the surplus go to `feeRecipient` after 90 days: an owner decision.
7. Gas measured in Foundry only.
8. BACKSTOP depositors (SPEC O-3) cannot fund through the vault in v0.2.

## 13. Owner inputs

- Cap, window limit and window, large threshold and delay, forced and escape delays (section 3 proposals).
- The 7 validator addresses and 3 guardian addresses (`TBD-owner`; generated by their holders, never by an agent).
- Escape surplus (insurance fund) to `0x775a…0544` after 90 days, or kept locked: proposed sweep (middle ground: users
  are paid first by construction).
- Counsel: the perpetuals are not deployed until legal review.

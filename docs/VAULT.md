# VAULT.md: PerpsVault, the collateral custody of PosternDEX Perpetuals (v0.3)

**STATUS: PROTOTYPE, UNAUDITED. Not deployed, not for deployment.** The perpetuals are not deployed this round (legal
review first). Owner: agent 433 (round 2; continues 390 and 352). Source: `contracts/src/PerpsVault.sol`; tests:
`contracts/test/`.

**v0.3 (2026-11-18, round 2) in one paragraph.** One behaviour change: `deposit` accepts the engine's INSURANCE
(`0x…02`) and BACKSTOP (`0x…03`) accounts, records who paid, and refunds an uncredited system deposit to that payer in
escape (section 4; closes gap 8 of v0.2). One fix: the system remainder's ready time is the later of 90 days after
escape and the large-amount delay, and its event states it (R433-1, section 7). No other rule changed. New in this
document: the keeper duty (8a, R395-19), the data the escape hatch needs (8b, R436-11), the end-to-end vectors
produced by the engine (10a), the gas table measured in Foundry and on anvil (9), the local deployment (in
`contracts/DEPLOY.md`). Everything of v0.2 below stands unless a line says "v0.3".

v0.2 text follows.
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
| `deposit(uint64 amount, address account) returns (uint64 index)` | anyone, after `approve` | Pulls `amount` (exact balance delta), credits `account` in the engine after L2 finality. Refused in escape, while paused, below `minDeposit`, above `cap` of custody, for the vault, the token or any keyless account (`<= 0xffff`) **except, since v0.3, INSURANCE `0x…02` and BACKSTOP `0x…03`**: a deposit to one of them funds the engine's insurance fund or backstop (SPEC 4.11 launch seeds, SPEC O-3). It is the same `Deposit` event and the same chain link (the engine credits `account`); the vault stores the payer in `systemFunder(index)`. It is a contribution, not a claim: once credited it leaves only by the subordination rule of section 7; it gives neither the system account nor the payer the right to post forced requests (`hasDeposited` is not set). `PROTOCOL_FEE` `0x…01` and every other keyless address stay refused. |
| `requestForced(uint8 kind, uint64 arg) returns (uint64 index)` | an account that deposited, for itself | SPEC 14 kinds: 0 CancelAll, 1 CloseAll, 2 Withdraw (`arg` = gross amount, pays the account itself); `arg` must be 0 otherwise. `deadline = block.timestamp + forcedDelay` (L2 seconds, counted from the request's own block). One open request per account. Refused in escape. |
| `submitCheckpoint(Checkpoint cp, bytes[] signatures)` | anyone (a relayer) | Accepts the checkpoint (rules in 5.3). Refused in escape. Not stopped by a pause. |
| `withdraw(uint64 seq, Withdrawal w, bytes32[] siblings, uint64 want) returns (bytes32 key)` | anyone | `Withdrawal = (uint64 withdrawalId, address account, address recipient, uint64 amount, uint64 height)`. Registers the record proven against accepted checkpoint `seq` (1-based; `withdrawalId < withdrawalCountAt[seq]`) and pays up to `want` to `recipient` now if ready, not paused and the bucket allows; otherwise it stays registered. |
| `release(bytes32 key, uint64 want)` | anyone | Pays min(`want`, remainder, bucket) of a registered payout to its recipient. Reverts `NotRegistered`, `Paused`, `NotReady`, `RateLimited` (bucket empty), `InvalidWithdrawal` (nothing left or `want` 0). |
| `guardianPause()` | a guardian | Pauses payouts and deposits for `pauseDuration` (chain rules of section 2). |
| `unpause(uint64 count, uint256 validUntil, bytes[] signatures)` | anyone with `unpauseThreshold` signatures | Ends pause `count` early; guardians then wait `pauseCooldown`. |
| `triggerEscape()` | anyone | Enters escape (one way) if `escapeReason() != 0`. |
| `claimEscape(Account a, bytes32[] siblings, uint64 want) returns (bytes32 key)` | anyone, pays the account | `Account = (address account, uint32 index, int128 balance, uint64 escapeValue, bytes32 positionsHash)` (SPEC 0.1.1 leaf). In escape, after `proveSystemShares`: registers `escapeValue * escapePayable / senior` (rounded down; `senior = totalEscape - insurance - backstop`) and pays up to `want` now. PROTOCOL_FEE's share pays `feeRecipient`; INSURANCE and BACKSTOP cannot claim (their remainder is registered by `proveSystemShares`). |
| `proveSystemShares(Account insurance, bytes32[] s1, Account backstop, bytes32[] s2)` | anyone, once, in escape | Proves INSURANCE (`0x…02`, index 1) and BACKSTOP (`0x…03`, index 2) leaves; fixes the user payout ratio and registers the subordinated remainder to `feeRecipient`, ready 90 days after escape began (R395-33). Required before any `claimEscape` and before the surplus sweep. |
| `refundDeposit(uint64 index, address account, uint64 amount)` | anyone, pays `account` | In escape: refunds deposit `index >= latest.depositsCredited`, verified against the vault's own chain. No bucket, delay or pause (no committee input). v0.3: when `account` is INSURANCE or BACKSTOP the money goes to `systemFunder(index)`, the address that paid that deposit, never to the keyless account (the event `Refunded(index, account, amount)` is unchanged and names the credited account). |
| `sweepEscapeSurplus() returns (uint64)` | anyone, pays `feeRecipient` | In escape, 90 days after it began, once: pays what lies beyond every counted obligation (donations, value the tree never assigned). Every amount still claimable (including unproven escape shares) stays, and so does the rounding residue of pro-rata claims (v0.3 precision in section 7, R433-2). |

Views: `custody()`, `reserved()`, `availableToRelease()`, `paused()`, `inEscape()`, `escapeReason()` (0 none,
1 silence, 2 overdue forced request), `depositHead(n)`, `forcedHead(n)`, `depositTotalAt(n)`, `forcedDeadline(i)`,
`latest()`, `checkpointCount()`, `withdrawalsRootAt(seq)`, `withdrawalCountAt(seq)`, `withdrawalsTotalAt(seq)`,
`payouts(key)`, `withdrawalKey(id)`, `escapeKey(account)`, `checkpointDigest(cp)`, `stateRootOf(cp)`,
`unpauseDigest(count, validUntil)`, `domainSeparator()`, `escapePayable()`, `escapePool()`, `subordinatedTotal()`,
`systemSharesProven()`, `systemFunder(index)` (v0.3). The committed ABI is `contracts/abi/PerpsVault.abi.json`
(`python3 script/export_abi.py --check` compares it with the build). Leaf, node and chain-link formulas are
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
systemHash = keccak256("PPv1sys" || be64 nextDepositIndex || be64 nextForcedIndex || be64 nextOrderId || be128 totalEscape || be128 dust || auxHash || be64 withdrawalCount || be64 withdrawalsTotal)
stateRoot  = keccak256("PosternPerps/state/v1" || be64 height || be64 time || accountsRoot || marketsRoot || withdrawalsRoot || systemHash)
```

The last four fields are outside the state root: `depositsHead` / `forcedHead` are the vault's own chain heads after
`nextDepositIndex` / `nextForcedIndex` items (validators compute them from the vault's events at the L2 finalized
tag; the vault compares them with what it accumulated itself, R395-1, coordinator decision 2026-11-17);
`withdrawalCount` / `withdrawalsTotal` (the number and sum of records in the withdrawal tree) are also inside the
state root since SPEC 0.1.5 (388, `5a50d58`); the vault's recompute reproduces 388's vector byte for byte
(`0xa37422a4…dba9ac`, `contracts/test/vectors/vault-vectors.json`).

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
before its deadline (`forcedDelay` after the L2 block; proposal 2 days since R395-25, floor 1 day, which covers L2
finality of minutes plus inclusion and a checkpoint); the vault refuses any checkpoint that leaves an overdue request unprocessed, and once one
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
days the surplus (donations, value the tree never assigned; NOT the rounding residue of pro-rata claims, see the
v0.3 precisions below) can be swept once to `feeRecipient`; the sweep
is refused until the system shares are proven, and never touches an unclaimed user value.
Failing-first demonstration: on `27c1969`, 395's scenario (pool 2,000; users 1,500 + 400; insurance 250, backstop
50) paid alice 1,363.636363 instead of 1,500 (test run recorded in the commit); the three `R395_33` tests pass now. A forced request the engine could not execute must not
be counted as processed (coordinator decision on R395-5: not consumed), so it runs into its deadline and opens
escape; the vault sees only the count and head, so that rule lives in SPEC 14 and the validators (388/389).

**v0.3 precisions (each pinned by a test).**
- *Ready time of the system remainder (R433-1, fixed).* The remainder registered by `proveSystemShares` is ready at
  the LATER of `escapeAt + 90 days` and the usual rule (`now`, plus `largeDelay` when it is at least
  `largeThreshold`), and `PayoutRegistered.readyAt` states that stored time. Before the fix the event carried
  `now (+ largeDelay)` while the stored time was `escapeAt + 90 days`, and a proof made shortly before the 90 days
  ended left a large payout releasable at once (`test/R433.t.sol`).
- *Rounding residue (R433-2, documentation).* `floor(escapeValue * escapePayable / senior)` leaves less than one base
  unit per claimant unclaimed in a pro-rata escape. The vault counts `escapePayable - escapeClaimedTotal` as still
  claimable, so that residue is NOT surplus and is never swept: it stays in the vault for good (2 units in the
  engine story `shortfall`). The sweep reaches only what lies beyond every counted obligation (donations, value the
  tree never assigned).
- *Who is senior.* Users and `PROTOCOL_FEE` share the pool pro rata; INSURANCE and BACKSTOP come after both. That
  the protocol's accrued fees rank with users in a shortfall is a revenue choice listed for the owner (section 13).
- *Funded system accounts.* A credited deposit to INSURANCE or BACKSTOP is part of that account's escape value and
  is subordinated like the rest of it; its payer has no claim on it. An uncredited one is refunded to its payer.

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

## 8a. Keeper duty (R395-19): who submits checkpoints, how often, and what a quiet relayer costs

**The fact.** The vault knows only the checkpoints somebody SUBMITS. "No root finalized" (the committee is dead) and
"no checkpoint submitted" (the committee is alive, the relayer is quiet) look the same to it, and both end in escape,
which is irreversible: every position is valued at the last submitted checkpoint, and everything the engine did after
it is void. Tests `test/Keeper.t.sol` pin each statement below.

**Who.** Anyone. `submitCheckpoint` takes no privileged caller: any address holding a finalized checkpoint (16 fields
and 5 signatures; the node serves it at its finality endpoint, API.md) can submit it, and a pause does not stop it.
Duty holders, in this order: (1) the node's relayer (agent 431's process, the operator's hot key with gas only: it has
no power over the vault); (2) each of the seven validator operators SHOULD run the same relayer against its own
replay, so that one quiet operator is not a quiet venue; (3) the frontend offers "submit the latest checkpoint" to any
user when the vault's `latest` lags the node's finalized height (it costs the user about 175,000 to 275,000 gas).

**How often.** A relayer submits the newest finalized checkpoint when ANY of these holds:
1. its `withdrawalCount` exceeds the vault's (a user is waiting for a record): within minutes (UX, not safety);
2. its `nextForcedIndex` exceeds the vault's (a forced request was processed): at once, and in any case no later than
   `forcedDelay / 4` after the `ForcedRequest` event (12 hours with the 2-day proposal). THIS is the safety bound;
3. `forcedDelay / 4` passed since the vault's `lastCheckpointAt` (heartbeat). Not `escapeDelay / 4`: see below.
Retry rule: a failed or unconfirmed submission is retried with the same checkpoint or a newer one, alternating L2
endpoints, with gas set explicitly (section 9), until `CheckpointAccepted` is seen at L2 finality; an alarm fires at
`forcedDelay / 2` without an accepted checkpoint while a forced request is open, and at `escapeDelay / 2` otherwise.
A checkpoint is still accepted AFTER a bound has passed, as long as nobody has triggered escape (and, for an overdue
forced request, as long as the checkpoint processed it): lateness is a race against anyone's `triggerEscape`, not an
automatic loss.

**Why the bound is `forcedDelay`, not `escapeDelay`.** Any account that ever deposited `minDeposit` (10 USDC in the
proposal) can post a forced request. From that block the venue has `forcedDelay` (2 days) to get a checkpoint that
PROCESSED the request accepted; a checkpoint finalized but not relayed does not count. So a relayer that is quiet for
two days can be turned into an irreversible escape by anyone for 10 USDC plus gas, five days before the silence rule
would allow it (`test_R395_19_forced_request_makes_forcedDelay_the_keeper_bound`).

**What a quiet relayer costs.** (a) Quiet for `forcedDelay` with a forced request open, or for `escapeDelay`
otherwise, with a live committee: the venue ends; users receive the escape values of the last submitted checkpoint,
certified records and uncredited deposits in full; trades, funding and liquidations after that checkpoint never
happened for the vault; the committee's later checkpoints are refused (`EscapeActive`). Nobody can take funds by it,
but positions are closed at a stale mark, which is a loss for whoever was right since. (b) Quiet for less: withdrawals
wait; nothing else.

**Decided (technical): no on-chain rule separates the two cases, and none is added.** Argument. (1) The vault cannot
observe finalization; any proof of "a root was finalized after time T" IS a checkpoint submission, which is already
what restarts the clock. (2) Considered and rejected: a two-step silence trigger (announce, then trigger one day
later unless a checkpoint arrives). It is provably no weaker for users (announce at `escapeDelay - 1 day`, trigger at
`escapeDelay`: the same earliest exit) and it admits a subset of today's escapes, but it protects only the silence
path; the forced path, which any depositor can open and which is five days shorter, would stay one transaction, and
adding a notice THERE lengthens the exit the user is promised. A rule that guards the long path and leaves the short
one open is not strictly safer in any scenario an adversary would choose, and it costs every caller, test and screen
a second step; it was not built. (3) Considered and rejected: measuring silence from the checkpoint's signed engine
`time` instead of the submission time. It is closer to "no root finalized", but it makes escape EASIER (a relayer one
week behind a healthy engine would then be in escape), the opposite of the finding. (4) What does protect: permission-
less relaying by several independent parties, the cadence above, and the late-acceptance rule.

**R433-3 (Low; liveness of the silence path only; stated, not changed).** Because the clock restarts at every accepted
checkpoint whatever its age, whoever holds a backlog of finalized but unrelayed checkpoints of a committee that has
since died can postpone escape-by-silence by one `escapeDelay` per checkpoint. The exit guarantee does not rest on
that path: a forced request cannot have been processed by any old checkpoint and opens escape `forcedDelay` after it
is posted (`test_R433_3_backlog_postpones_silence_but_not_a_forced_request`). The user-facing rule is therefore: to
leave a silent venue, post a forced request; do not wait for the silence rule.

## 8b. Data the escape hatch needs (R436-11, reviewer 436)

`claimEscape` takes the account's leaf and 32 siblings against the vault's latest accepted `accountsRoot`, and no
claim is possible before `proveSystemShares` was called with the INSURANCE and BACKSTOP leaves and THEIR siblings. The
siblings depend on every account's state at that checkpoint. The vault holds only the root. **Assumption, now
stated: when escape begins, at least one party who is willing to publish holds the account tree of the vault's latest
accepted checkpoint (or the input log up to it, from which the engine rebuilds the tree).** If the sequencer is the
only holder and it is the party that went silent or hostile, escape values cannot be claimed (uncredited deposits and
already certified withdrawal records still can: they need no tree). What discharges the assumption is outside the
vault and is asked of its owners in the cross-area log: every validator serves the log and proofs from its own replay
(431); an engine CLI that produces a proof from genesis + log (430); the frontend stores, at every
`CheckpointAccepted`, the user's own proof AND the two system proofs for that checkpoint and offers them as a file
(434); the rehearsal performs escape with the sequencer's API off (435). A proof is valid only against the checkpoint
it was made for, so a stored proof is replaced at each accepted checkpoint. Not built in the vault: publishing the
tree on chain costs gas linear in the number of accounts per checkpoint; an on-chain data-availability commitment is a
later design, listed in section 12.

## 9. Gas and size (v0.3; Foundry 1.7.1, anvil 1.7.1, solc 0.8.24, optimizer runs 1, shanghai; Mac, 2026-11-18)

Runtime 20,167 bytes (EIP-170 limit 24,576; 4,409 to spare; v0.2 as merged was 19,930, the "18,600" of the v0.2
text predated R395-33). Creation with 7 validators and 3 guardians: 4,605,275 gas inside Foundry
(`test_creation_gas_below_l2_cap`, bound 5,000,000) and 4,986,144 gas as an anvil transaction (intrinsic and calldata
included); the L2's per-transaction cap is 15,000,000 (bloch-l2-evm `docs/L2-MAINNET-GO-LIVE-V21.md` K8).

| Call | Foundry min | Foundry median | Foundry max | n | anvil tx min | anvil tx median | anvil tx max | n |
|---|---|---|---|---|---|---|---|---|
| `deposit` | 30,058 | 186,964 | 196,324 | 351 | 132,372 | 132,583 | 191,297 | 21 |
| `requestForced` | 30,351 | 49,698 | 123,143 | 15 | 107,982 | 115,562 | 123,143 | 2 |
| `submitCheckpoint` (5 signatures) | 42,630 | 274,662 | 274,794 | 319 | 152,689 | 174,793 | 254,978 | 12 |
| `withdraw` (register + pay, depth 32) | 46,551 | 68,979 | 180,995 | 287 | 175,322 | 175,532 | 175,724 | 3 |
| `release` | 32,446 | 73,299 | 92,865 | 22 | 65,877 | 76,827 | 87,777 | 6 |
| `claimEscape` | 48,814 | 184,962 | 202,198 | 26 | 147,865 | 179,785 | 180,063 | 12 |
| `proveSystemShares` | 67,051 | 122,483 | 175,632 | 16 | 121,823 | 175,382 | 175,492 | 4 |
| `refundDeposit` | 32,616 | 39,881 | 135,056 | 13 | 110,907 | 110,907 | 110,907 | 1 |
| `triggerEscape` | 30,054 | 66,267 | 66,267 | 21 | 66,090 | 66,267 | 66,267 | 4 |
| `guardianPause` | 47,008 | 54,382 | 80,765 | 14 | not sent | | | |
| `unpause` (6 signatures) | 55,238 | 55,685 | 85,480 | 3 | not sent | | | |
| `sweepEscapeSurplus` | 31,806 | 50,044 | 96,859 | 7 | not sent | | | |

Sources and method. Foundry columns: `forge test --offline --gas-report` over the unit suites (execution gas of the
call as the test made it; minima are refused calls, so the median and maximum are the planning numbers). anvil
columns: `gasUsed` of the receipts of `script/e2e_anvil.py` (real transactions of the four engine stories on a fresh
anvil per story, chain id 700770, intrinsic gas and calldata included; evidence `agents/433-evidence/anvil-e2e.json`;
`release` merges the delayed escape payouts and the system remainder). The first call that writes a fresh slot is the
maximum of its row. Not measured on the Bloch L2 itself.

**Set gas explicitly (F434-1, agent 434).** An exact `eth_estimateGas` for a paying call (`withdraw`, `release`,
`claimEscape`) can be short one block later: the bucket refill and cold or warm token slots change the cost by a few
thousand gas, and the token call runs under the 63/64 rule. A transaction that runs out of gas reverts whole (no
partial effect), so the cost is a failed transaction, not a loss; relayers, keepers, the rehearsal and the page send
`1.5 x estimate + 40,000` or a fixed limit (`e2e_anvil.py` uses a fixed 3,000,000).

## 10. Tests (contracts/, `forge test --offline`)

**v0.3: 81 tests in 16 suites** (54 at the start of round 2, none removed; one restated,
`test_keyless_deposit_refused`, with the reason in the test). Added in round 2: `DevToken.t.sol` 3 (the local-stack
token against the real vault); `SystemFunding.t.sol` 4 (system deposits, refund to the payer, R395-26 kept,
insurance first); `Invariant.t.sol` +3 invariants and 1 probe test (below); `EngineVectors.t.sol` 4 (section 10a);
`R433.t.sol` 3; `DeployScript.t.sol` 4 (post gate: runtime code, R436-9; token code pin); `Keeper.t.sol` 4 (section
8a); `EngineCliProofs.t.sol` 1 (section 10a, last paragraph).

Invariants (stateful fuzz, 128 runs x 64 calls, handler with an honest engine model, a colluding committee, system
funding, guardians and escape; 8 invariants): the five of v0.2 plus
`invariant_balance_covers_every_unpaid_obligation` (token balance >= registered unpaid payouts + certified records
not yet registered + uncredited, unrefunded deposits + in escape the whole pool before the system proof or the users'
unclaimed part after it), `invariant_no_path_pays_twice` (after EVERY successful registration, refund, system proof
and sweep the handler repeats the call, against every accepted checkpoint for records, and a success raises a flag;
plus the totals' bounds) and `invariant_pause_blocks_only_deposits_and_payments` (a probe pauses and then tries every
entry point: deposits and releases must fail with `Paused`, a registration must register and pay nothing, and forced
requests, checkpoints, the escape trigger and refunds must never fail with `Paused`). The probes are not vacuous:
`test_round2_probes_run` reaches each one, and six mutants of the vault (pause check removed from `deposit` or
`_release`, added to `submitCheckpoint` or `refundDeposit`, refund flag removed, `AlreadyRegistered` removed) are each
caught (the `refundDeposit` mutant by the deterministic probe test only: a paused escape with an unrefunded deposit is
rare in the random runs).

v0.2 text: 53 tests: 40 unit/fuzz, 3 R395-33 subordination (`PerpsVault.t.sol`: configuration refusals; deposits and hostile tokens; checkpoint
signatures, domain, fields, state-root equation, append-only, solvency, R395-23 pro rata; withdrawals: tamper,
positions, siblings, depth, old checkpoints, fee account, recipients, large delay, bucket parts; guardians;
forced requests; escape: overdue, silence rules, claims, refunds, certified records, parts, keyless shares,
unproven shares, surplus; re-entrancy; tree round-trip fuzz), 2 silence/keyless tests, 5 invariants (128 runs x 64
calls, handler with an honest engine model and a colluding committee), 1 escape end-to-end fuzz, 1 creation-gas test,
1 vector test.

## 10a. End-to-end vectors produced by the engine (round 2, task 2)

`contracts/tools/engine-vectors` (Rust; built only on the build host, `generate.sh`) drives the REAL engine library
through four stories: L2 deposits and forced requests as the vault would emit them, signed user actions, oracle
rounds, withdrawals. It builds every checkpoint the way the node does (`state_parts` + `Engine::checkpoint_fields`),
asserts the SPEC 11 state-root equation, and signs with the node's own `checkpoint::sign_checkpoint` under the vault's
EIP-712 domain with a throwaway 5-of-7 committee (keys derived from public labels, never written). Proofs are the
engine's `withdrawal_proof` / `account_proof`, taken from the engine state of the last checkpoint the vault accepted.
For every step it writes the effect expected of the vault, computed by an independent restatement of sections 5 to 7
(not by calling the contract). The vectors (`contracts/test/vectors/engine/*.json`, schema
`postern-perps/engine-vault-vectors/1`, vault `0xe7f1725E7734CE288F8367e1Bb143E90bb3F0512`, chain 700770) are replayed

- in forge by `test/EngineVectors.t.sol` (the vault deployed at that address; around each engine item the tampered and
  repeated forms are refused: 5 per checkpoint, 3 per record, 2 per claim, refunds, the system proof), and
- on anvil by `script/e2e_anvil.py` as real transactions at the stories' L2 timestamps (one fresh anvil per story;
  178 checks and 36 refusals checked in total; evidence `agents/433-evidence/anvil-e2e.json`).

| Story | What it shows | Result (both replays) |
|---|---|---|
| `normal` (29 steps) | users deposit; an order is refused `NOT_LAUNCHED` until the owner seeds INSURANCE 60,000 and BACKSTOP 25,000 THROUGH THE VAULT; trade; signed withdrawal 1,000 -> record -> paid 999 (flat fee 1); forced withdrawal 5,000 posted on the L2 -> served -> paid 4,999; UTC day rollover -> the engine's fee sweep record 6.035 paid to `0x775a…0544`; a forced CloseAll the sequencer ignores -> the next checkpoint before the deadline accepted, the one after it refused `InvalidCheckpoint` -> escape (reason 2) -> three claims, the uncredited deposit refunded, the system remainder 85,000.000001 after 90 days | sum of the engine's escape values 130,995.965000 == the vault's pool, to the unit; vault ends at 0; conservation exact |
| `insurance` (19) | an account at 10x falls to -6,640.08 equity behind a halted market (no liquidation, SPEC S-4.10); the engine charges the deficit to INSURANCE in the escape values (60,000 -> 53,359.92); validators stop; escape by silence (reason 1); a 54,599.71 claim waits the large-amount delay | sum of escape values 136,000.000000 == custody; every user in full; the underwater account's claim refused `InvalidWithdrawal`; vault ends at 0 |
| `backstop` (19) | same with seeds 5,000 / 2,000: the insurance value is 0 and the engine's values exceed custody by 1,640.08 | the vault's subordination pays every user in full out of the BACKSTOP share; system remainder 359.92 |
| `shortfall` (18) | same with seeds 5,000 / 1,000: the deficit exceeds both system shares | users and PROTOCOL_FEE pro rata (57,000.000000 of 57,640.078160), INSURANCE and BACKSTOP nothing; 2 base units of rounding stay in the vault (R433-2) |

**Round-2 engine and node.** The vectors were generated with engine and node at `main` `f0e3967`. Rebuilt on the
build host against `codex/perps-engine-r2-20261118` `cd8e5ae` (SPEC 0.2.2) with `codex/perps-node-r2-20261118`
`b897344`, the generator gives the SAME BYTES for all four stories (`cmp` equal): the round-2 engine changes no root,
leaf or checkpoint field of these stories. The integrator reruns `generate.sh` on the merged tree; a difference
means an interface change to announce, never a file to overwrite silently.

**The engine CLI's own proofs (430).** `test/vectors/engine-cli/proofs-example.json` is an unchanged copy of
`engine/vectors/proofs-example.json` (`cd8e5ae`, output of `engine proofs-example`; sha256 `d9925150…682a9d`).
`test/EngineCliProofs.t.sol` checks it with the vault's own code: the vault's state-root equation gives the CLI's
state root; each of the 6 account leaves and 2 withdrawal leaves has the vault's byte format and verifies by the
vault's Merkle walk against the checkpoint's roots, and not at the neighbouring position; the escape values sum to
`totalEscape` and to custody, with the under-water account at 0 and INSURANCE below its balance. That checkpoint is
unsigned and made for the testkit vault address, so it shows format and proof agreement, not acceptance.

What the replays prove: the bytes the engine commits and the node code signs are the bytes the vault verifies
(checkpoint digest, state root, both chain heads, both leaf formats, proof direction), for the scenarios above, and
the vault's payouts equal an independent statement of its rules. What they do not prove is in section 12.

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
| R395-19 (v0.3) | keeper duty stated (8a) with the facts pinned by `test/Keeper.t.sol`; no on-chain rule change, with the argument; node cadence asked of 431 |
| v0.2 gap 8 (v0.3) | INSURANCE and BACKSTOP funded through the vault; payer refunded in escape if uncredited; failing first on `f0e3967` (3 tests `InvalidDeposit`) |
| R433-1 Low (v0.3; `PerpsVault.sol` `proveSystemShares`, `f0e3967` :729-730) | system remainder: event stated a wrong ready time and a late proof skipped the large-amount delay; fixed (later of the two, passed into the registration); failing first on `1c03ef0` |
| R433-2 Info (v0.3) | rounding residue of pro-rata claims is never surplus: documented (section 7), pinned by a test |
| R433-3 Low (v0.3) | a backlog of old checkpoints postpones escape-by-silence; stated in 8a, not changed (the forced request is the exit) |
| R436-9 Low (v0.3, reviewer 436) | the deploy script's post gate compares the runtime code with a twin built from the same config; token pinned by code hash; failing first on `17edfc8` |
| R436-11 Medium, design (v0.3, reviewer 436) | the escape hatch's data-availability assumption stated (8b); the discharge is asked of 430, 431, 434, 435 |
| F434-1 (agent 434) | explicit gas for paying calls (section 9) |

## 12. Not done / not proven (v0.3)

1. UNAUDITED. No external review. Internal reviewers: 395 (round 1), 436 (round 2; verdict on `2af91c9`: no
   regression, nothing above Low; later commits not yet re-read by 436 when this was written).
2. The end-to-end vectors come from the engine LIBRARY and the node's signing code driven by a generator, not from a
   running sequencer with seven validator processes and a relayer: that is 435's rehearsal. Engine and node were
   taken at `main` `f0e3967`; a round-2 change of the engine's state root or leaf formats needs the vectors
   regenerated (`generate.sh`) and will show as a failing replay, not silently.
3. No on-chain cancel of a fraudulent queued large payout (guardians can only pause); a proof-of-fraud freeze (two
   conflicting checkpoints at one height) would be a strict strengthening, not built.
4. No validity proof of the engine; the committee is trusted (section 8). A colluding quorum chooses the tree users
   escape with.
5. The keeper duty (8a) is a duty of processes outside the vault; nothing on chain enforces it, and a venue whose
   relayers are all quiet for `forcedDelay` can be ended by any depositor. No relayer was run by this agent.
6. The escape hatch needs the account tree to be available (8b); the vault does not make it available.
7. Tested with 6-decimal mock tokens that have bUSDC's transfer semantics, not with `BridgedStablecoinV3` bytecode on
   a private Bloch L2. Gas measured in Foundry and on anvil only.
8. The owner deploy script was rehearsed on anvil with anvil's unlocked account; not with a Ledger, not against the
   Bloch L2 node; the creation transaction's input is not pinned by hash (the bridge's preflight does that from a
   reviewed build manifest; here the post gate compares runtime code instead).
9. The rounding residue of a pro-rata escape stays in the vault for good (by design; at most one base unit per
   claimant).
10. The invariant suite's handler models an honest engine and one kind of colluding committee (a forged account
    tree); it does not model a committee that forges the withdrawal tree, reorders deposits (impossible by the chain
    heads, unit tested) or signs absurd heights (section 8, last item).
11. `PROTOCOL_FEE`'s seniority in a shortfall, the 90-day rule for the system remainder, every limit and both delays
    are owner inputs (section 13), not decided.

## 13. Owner inputs (listed, not decided)

- `forcedDelay`: 2 days proposed (constructor floor 1 day, R395-25). It is also the keeper bound (8a): a shorter
  value gives users a faster exit and operators less time to repair a quiet relayer.
- Launch seeds: INSURANCE 60,000 USDC and BACKSTOP 25,000 USDC proposed (compliance/risk; engine genesis
  `launchInsuranceMin` / `launchBackstopMin`), deposited through the vault by the owner (v0.3). Once credited they are
  not withdrawable by their payer: they return to `0x775a…0544` only as the escape remainder after 90 days, after
  every user.
- Cap 1,000,000; window 250,000 per day; large threshold 50,000 with 1 day delay; `escapeDelay` 7 days; pause 2 days,
  cooldown 2 days; minimum deposit 10 (`contracts/deploy/vault.config.json`).
- The 7 validator addresses and 3 guardian addresses (`TBD-owner`; generated by their holders, never by an agent);
  the bUSDC address and its runtime code hash (`TBD-deploy`).
- Who runs relayers (8a): proposed the operator plus each validator operator.
- Revenue, middle ground to be confirmed: in an escape shortfall the protocol's accrued, unswept fees
  (`PROTOCOL_FEE`'s escape value) rank WITH users (they take the same haircut and are paid to `0x775a…0544`); the
  alternative is to rank them after users like INSURANCE and BACKSTOP. Today's rule is kept (395 listed it in round
  1); in the `shortfall` story it is 39.92 USDC of 57,000.
- Escape surplus and system remainder to `0x775a…0544` after 90 days, or kept locked: proposed as built.
- Counsel: the perpetuals are not deployed until legal review.

# VAULT.md: PerpsVault, the collateral custody of PosternDEX Perpetuals (v0.1)

**STATUS: PROTOTYPE, UNAUDITED. Not deployed, not for deployment.** The perpetuals are not deployed this round (legal
review first). Owner: agent 390 (continues 352). Source: `contracts/src/PerpsVault.sol`; tests: `contracts/test/`.
Formats here are the vault side of `docs/SPEC.md` (agent 388); where SPEC.md does not yet define them, this file
proposes them (cross-area log, 2026-11-17 04:55 UTC) and SPEC.md either adopts them or a change is agreed there.

## 1. What it is

One contract on the Bloch L2 (EVM, chain id 700770) holding the collateral token bUSDC (6 decimals; the bridge's
`BridgedStablecoinV3`). The exchange engine runs off-chain (sequencer + validator committee, BRIEF-PERPS item 2). The
vault never trusts the sequencer. Funds leave only:

1. against a **checkpoint** signed by `threshold` of a validator committee fixed at deployment, through a **withdrawal
   record** proven by a Merkle sum-tree proof, or
2. in **escape mode**, against the last accepted checkpoint's escape tree, or as a refund of a deposit the last
   checkpoint did not credit (from the vault's own records, no committee input).

There is no owner, admin, upgrade, setter, signer replacement, rescue or administrator mint. Every limit is fixed in
the constructor. Protocol fees are not taken by the vault: they accrue in the engine to the fee account, which is the
owner's address `0x775a993F09Bd80aeCF8F8c2ADe0C84158E3a0544` (immutable `feeRecipient`), and the vault pays that
account's records only to that address.

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
| Ids bound to their position so one item cannot take another's slot (F111-F1 class) | deposit id top bytes = nonce | withdrawal leaf binds `index`; proof position must equal it |
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
`1 hour <= forcedDelay <= 7 days`; `1 day <= escapeDelay <= 30 days` and `escapeDelay >= forcedDelay`; guardians:
at most 7, sorted, not validators; no guardians means `pauseDuration == pauseCooldown == 0`; otherwise
`0 < pauseDuration <= 14 days` and `pauseCooldown >= pauseDuration`.

Proposed launch values (technical proposal; caps and windows are owner decisions, listed in section 10):
7 validators, threshold 5, unpauseThreshold 6, 3 guardians, pauseDuration 2 days, pauseCooldown 2 days,
cap 1,000,000 USDC, minDeposit 10 USDC, windowLimit 250,000 USDC per 1 day, largeThreshold 50,000 USDC,
largeDelay 1 day, forcedDelay 1 day, escapeDelay 7 days.

## 4. ABI

| Function | Who | Effect |
|---|---|---|
| `deposit(uint64 amount, address account) returns (uint64 index)` | anyone, after `approve` | Pulls `amount` (exact balance delta), credits `account` in the engine after L2 finality. Refused in escape, while paused, below `minDeposit`, above `cap` of custody, for account 0 / vault / token. |
| `requestForced(uint8 kind, uint32 marketId, uint64 amount) returns (uint64 index)` | an account that deposited, for itself | Forced request with `deadline = now + forcedDelay`. Kinds: 1 cancel all orders, 2 close positions (`marketId` 0 = every market), 3 withdraw `amount` (0 = everything withdrawable; pays the account itself; `marketId` must be 0). One open request per account. Refused in escape. |
| `submitCheckpoint(Checkpoint cp, bytes[] signatures)` | anyone (a relayer) | Accepts the checkpoint (rules in 5.3). Refused in escape. Not stopped by a pause. |
| `withdraw(uint64 seq, Withdrawal w, SumProof proof, uint64 want) returns (bytes32 key)` | anyone | Registers record `w` proven against accepted checkpoint `seq` (1-based) and pays up to `want` to `w.recipient` now if ready, not paused and the bucket allows; otherwise it stays registered. |
| `release(bytes32 key, uint64 want)` | anyone | Pays min(`want`, remainder, bucket) of a registered payout to its recipient. Reverts `NotRegistered`, `Paused`, `NotReady`, `RateLimited` (bucket empty), `InvalidWithdrawal` (nothing left or `want` 0). |
| `guardianPause()` | a guardian | Pauses payouts and deposits for `pauseDuration` (chain rules of section 2). |
| `unpause(uint64 count, uint256 validUntil, bytes[] signatures)` | anyone with `unpauseThreshold` signatures | Ends pause `count` early; guardians then wait `pauseCooldown`. |
| `triggerEscape()` | anyone | Enters escape (one way) if `escapeReason() != 0`. |
| `claimEscape(address account, uint64 amount, SumProof proof, uint64 want) returns (bytes32 key)` | anyone, pays `account` | In escape: registers `account`'s escape balance from the latest checkpoint, pays up to `want` now as `withdraw` does. |
| `refundDeposit(uint64 index, address account, uint64 amount)` | anyone, pays `account` | In escape: refunds deposit `index >= latest.depositsCredited`, verified against the vault's own chain. No bucket, delay or pause (no committee input). |
| `sweepEscapeSurplus() returns (uint64)` | anyone, pays `feeRecipient` | In escape, 90 days after it began, once: pays what no claim can reach (engine insurance fund, donations). Every amount still claimable stays. |

Views: `custody()`, `reserved()`, `availableToRelease()`, `paused()`, `inEscape()`, `escapeReason()` (0 none,
1 silence, 2 overdue forced request), `depositHead(n)`, `forcedHead(n)`, `depositTotalAt(n)`, `forcedDeadline(i)`,
`latest()`, `checkpointCount()`, `withdrawalsRootAt(seq)`, `withdrawalCountAt(seq)`, `withdrawalsTotalAt(seq)`,
`payouts(key)`, `withdrawalKey(index)`, `escapeKey(account)`, `checkpointDigest(cp)`, `unpauseDigest(count,
validUntil)`, `withdrawalLeaf(w)`, `escapeLeaf(account, amount)`, `nodeHash(...)`, `sumRoot(leaf, sum, proof)`,
`depositLink(...)`, `forcedLink(...)`, `domainSeparator()`.

Events (exact signatures, for the node's watcher 389):

```
Deposit(uint64 indexed index, address indexed account, address indexed sender, uint64 amount, bytes32 head)
ForcedRequest(uint64 indexed index, address indexed account, uint8 kind, uint32 marketId, uint64 amount, uint64 deadline, bytes32 head)
CheckpointAccepted(uint64 indexed seq, uint64 indexed height, bytes32 stateRoot, bytes32 withdrawalsRoot, bytes32 escapeRoot)
PayoutRegistered(bytes32 indexed key, address indexed recipient, uint64 total, uint64 readyAt)
Paid(bytes32 indexed key, address indexed recipient, uint64 amount)
Refunded(uint64 indexed index, address indexed account, uint64 amount)
GuardianPaused(address indexed guardian, uint64 indexed pauseCount, uint64 pausedUntil)
Unpaused(uint64 indexed pauseCount, uint64 guardianBlockedUntil)
EscapeTriggered(uint64 at, uint8 reason, uint64 checkpointSeq)
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
Checkpoint(uint64 height,uint64 time,bytes32 stateRoot,bytes32 logHash,uint64 depositsCredited,bytes32 depositsHead,uint64 forcedProcessed,bytes32 forcedHead,uint64 withdrawalCount,uint64 withdrawalsTotal,bytes32 withdrawalsRoot,uint64 escapeTotal,bytes32 escapeRoot)
```

digest = `keccak256(0x19 0x01 || domainSeparator || keccak256(abi.encode(CHECKPOINT_TYPEHASH, fields...)))`.

| Field | Meaning (engine side defined in SPEC.md) |
|---|---|
| `height`, `time` | engine block height and time (ms) of the state |
| `stateRoot`, `logHash` | the engine state root and input-log hash at `height` (SPEC.md; nonzero state root) |
| `depositsCredited`, `depositsHead` | number of vault deposits the engine has credited (in vault index order) and the vault's deposit chain head after that many |
| `forcedProcessed`, `forcedHead` | number of forced requests the engine has processed (in index order) and the forced chain head after that many |
| `withdrawalCount`, `withdrawalsTotal`, `withdrawalsRoot` | the append-only withdrawal sum tree over every record ever (index 0..count-1): leaf count, sum of amounts, root (0 iff count 0) |
| `escapeTotal`, `escapeRoot` | the escape sum tree: per account the balance it receives if escape starts at this checkpoint; total and root (root 0 means no account, total 0) |

### 5.3 Acceptance rules (`submitCheckpoint`)

Refused (`InvalidCheckpoint`, `Insolvent` or `InvalidCertificate`) unless all hold:

1. not in escape; `height > 0`; after the first, `height` and `time` strictly above the latest's; `stateRoot != 0`;
2. `latest.depositsCredited <= depositsCredited <= depositCount` and `depositsHead == depositHead(depositsCredited)`;
3. `latest.forcedProcessed <= forcedProcessed <= forcedCount` and `forcedHead == forcedHead(forcedProcessed)`;
4. no request is left past its deadline: `forcedProcessed == forcedCount` or
   `block.timestamp <= forcedDeadline[forcedProcessed]`;
5. withdrawals append-only as far as the vault can see: count and total never decrease; the same count means the same
   root and total; root 0 iff count 0; count 0 means total 0; escape root 0 means escape total 0;
6. solvency: `escapeTotal + (withdrawalsTotal - withdrawnTotal) + (depositedTotal - depositTotalAt[depositsCredited])
   <= custody()`;
7. `threshold` valid signatures (section 2) over the digest.

### 5.4 Chains

```
depositHead(0) = keccak256(abi.encode(bytes32("PosternPerps.deposit.v1"), chainid, vault))
depositHead(i+1) = keccak256(abi.encode(depositHead(i), uint256(i), address account, uint256(amount)))
forcedHead(0)  = keccak256(abi.encode(bytes32("PosternPerps.forced.v1"), chainid, vault))
forcedHead(i+1) = keccak256(abi.encode(forcedHead(i), uint256(i), address account, uint256(kind), uint256(marketId), uint256(amount), uint256(deadline)))
```

`bytes32("...")` is the ASCII text left-aligned and zero-padded. Each event carries the new head, so the watcher can
check its own computation per event; the engine credits `account` (never `sender`).

### 5.5 Merkle sum trees

```
withdrawal leaf = keccak256(abi.encode(bytes32("PosternPerps.withdrawal.v1"), uint256(index), address account, address recipient, uint256(amount)))   (160 bytes)
escape leaf     = keccak256(abi.encode(bytes32("PosternPerps.escape.v1"), address account, uint256(amount)))                                         (96 bytes)
node            = keccak256(abi.encodePacked(bytes1(0x01), bytes32 leftHash, uint64 leftSum, bytes32 rightHash, uint64 rightSum))                  (81 bytes)
node sum        = leftSum + rightSum (must fit uint64)
empty leaf      = (hash 0x00..00, sum 0)
```

A tree is the complete binary tree over the leaves padded with empty leaves to a power of two (one leaf: the root is
the leaf). Proof = `(uint64 index, bytes32[] hashes, uint64[] sums)`, sibling of the leaf level first; bit `i` of
`index` set means the running node is the right child at level `i`; `index < 2^depth`; depth <= 48. The vault
recomputes the root hash and total and requires both to equal the checkpoint's. Preimage lengths differ (160, 96, 81
bytes), so a node is never accepted as a leaf or a leaf of one tree as a leaf of the other.

Withdrawal tree: positional by record index; append-only across checkpoints (the engine never rewrites a record); a
record's proof index must equal its `index`. Escape tree: any order (the engine's account order); one leaf per
account; a claim is keyed by account, so a duplicate leaf pays once.

### 5.6 Unpause

`Unpause(uint64 pauseCount,uint256 validUntil)` under the same domain; bound to the pause being ended.

## 6. Payout rules

- **Registration** (`withdraw`, `claimEscape`) proves the item, checks the totals (sum of registered withdrawal
  records <= the newest `withdrawalsTotal`; sum of escape claims <= `escapeTotal`), and records `Payout(recipient,
  total, paid 0, readyAt)`, `readyAt = now + largeDelay` if `total >= largeThreshold`, else now. A key registers once
  (`withdrawalKey(index)`, `escapeKey(account)`): a record paid against one checkpoint cannot be paid against a later
  one.
- **Payment** (`release`, or at registration) pays the smallest of the ask, the remainder and the bucket. A payout
  above the window limit is therefore paid in parts (no item is ever unpayable because of its size, unlike the
  bridge's burn above `bucketCapacity`); a record at or above `largeThreshold` waits its delay for every part
  (splitting does not bypass the delay).
- **Fee account**: a withdrawal record whose `account == feeRecipient` must have `recipient == feeRecipient`.
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

**Escape** (one way) is due when `block.timestamp >= lastCheckpointAt + escapeDelay` (no checkpoint accepted; before
any checkpoint, measured from deployment) or a forced request is past its deadline unprocessed by the latest
checkpoint. In escape: checkpoints, deposits and forced requests stop; withdrawal records of every accepted checkpoint
stay payable (they were debited from the escape balances before their checkpoint); each account claims its escape
balance from the latest checkpoint; deposits with index >= `latest.depositsCredited` are refunded to the credited
account from the vault's own chain; after 90 days the surplus (custody no claim can reach: the engine's insurance
fund, donations) can be swept once to `feeRecipient`.

**Escape balance rule** (vault-side requirement; the engine rule belongs to SPEC.md, 388): the escape tree holds, per
account, the account's equity if every position were closed at the checkpoint's mark price, floored at zero, with
any shortfall covered first by the insurance fund and then by a stated haircut, and the fee account's balance;
`escapeTotal` plus the unregistered withdrawal records plus uncredited deposits must fit in custody, which the vault
enforces (rule 6). The insurance fund is outside the tree.

**Solvency proof (the property the tests check).** Let `owed = (escapeTotal - escapeClaimedTotal) + (withdrawalsTotal
- withdrawnTotal) + (depositedTotal - depositTotalAt[credited] - refundedTotal)` for the latest checkpoint. (a) At
acceptance `owed <= custody()` (rule 6). (b) A deposit raises `custody` and the uncredited term by the same amount;
a registration raises `committedOut` (lowers `custody`) and lowers its term by the same amount; a refund likewise; the
surplus sweep lowers `custody` to exactly the unregistered remainder. So `owed <= custody()` holds at every state.
(c) `balance >= custody() + reserved` holds at every state (deposits add to both, registrations move custody to
reserved, payments lower reserved and the balance equally; transfers are balance-checked). Hence every claim the latest
checkpoint admits can be registered, and every registered payout can be paid: no claim race, even if a later claimant
comes last. With the sum tree the leaves of a valid root sum to exactly the committed total, so all of them fit.

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
- **A checkpoint with height or time at `2^64 - 1`** blocks every later checkpoint (they must be higher): the venue goes
  to escape after `escapeDelay`. Only a quorum can sign it; validators must refuse such fields (SPEC.md bound).

## 9. Not done / not proven

1. UNAUDITED; no external review yet (agent 395 is the internal reviewer).
2. The engine side (SPEC.md trees, escape balance rule, forced processing rule, binding of the vault address and chain
   id in the engine genesis so one engine cannot serve two vaults: bridge finding class D3) is not written yet (388).
3. No on-chain cancel of a fraudulent queued large payout (guardians can only pause, by the brief); the delay buys
   watchers g * `pauseDuration` at most. A proof-of-fraud freeze (two conflicting checkpoints at one height) is a
   possible strict strengthening, not built.
4. No light-client / validity proof of the engine; the committee is trusted (section 8).
5. Tested with a 6-decimal mock token that has bUSDC's transfer semantics, not with `BridgedStablecoinV3` bytecode on a
   private Bloch L2; no anvil rehearsal with the real node yet.
6. Insurance-fund leftover in escape goes to `feeRecipient` after 90 days: an owner decision (section 10).
7. Gas is measured in Foundry, not on the Bloch L2.
8. The backstop liquidity vault (BRIEF-PERPS item 1) has depositors of its own; their share in escape is the engine's
   accounting (leaves of the escape tree), not modelled here.

## 10. Owner inputs

- Cap, window limit and window, large threshold and delay, forced and escape delays (section 3 proposals).
- The 7 validator addresses and 3 guardian addresses (`TBD-owner`; generated by their holders, never by an agent).
- Escape surplus (insurance fund) to `0x775a…0544` after 90 days, or kept locked: proposed sweep (middle ground: users
  are paid first by construction).
- Counsel: the perpetuals are not deployed until legal review.

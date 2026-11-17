// SPDX-License-Identifier: UNLICENSED
pragma solidity 0.8.24;

/// @title PerpsVault: collateral custody of PosternDEX Perpetuals on the Bloch L2 (chain 700770)
/// @notice STATUS: PROTOTYPE, UNAUDITED. Not for deployment. Specification: docs/VAULT.md.
///
/// The vault holds the collateral token (bUSDC, 6 decimals) for the off-chain exchange engine. It never trusts the
/// sequencer: funds leave only against a checkpoint signed by `threshold` of a validator committee fixed at deployment,
/// within limits fixed at deployment. There is no owner, no admin, no upgrade, no setter, no rescue.
///
/// - Deposits: anyone deposits for an engine account. Every deposit extends a hash chain (`depositHead`) and a
///   cumulative total; a checkpoint must name exactly the vault's chain head after the deposits it credited.
/// - Checkpoints (EIP-712, domain "PosternPerpsVault"/"1"/chainid/this): k-of-n signatures, strictly ascending signer
///   addresses, low-s, v 27/28 (the bridge's `_checkCertificate`). Each one commits the engine state, the deposits and
///   forced requests it processed (by count and chain head), an append-only Merkle sum tree of withdrawal records and
///   a Merkle sum tree of escape balances. The vault accepts it only if everything it commits is payable from custody
///   (`_checkSolvency`), so every claim it admits can be paid.
/// - Withdrawals: anyone submits a record with its sum-tree proof against any accepted checkpoint; the recipient is
///   paid, in parts if the window limit requires, through a token bucket (`windowLimit` per `windowSeconds`); a
///   record of at least `largeThreshold` waits `largeDelay` first. Records of the fee account (`feeRecipient`) pay
///   `feeRecipient` only.
/// - Guardians (fixed, disjoint from validators) can only pause payouts and deposits, once per guardian per chain of
///   pauses, `pauseDuration` each (the bridge vault's pause chain); `unpauseThreshold` validators end a pause early.
///   A pause never stops checkpoints, forced requests, the escape trigger or refunds of uncredited deposits.
/// - Forced requests: an account that deposited posts cancel-all / close / withdraw on the L2; a checkpoint must not be
///   accepted while an earlier request is past its deadline and unprocessed.
/// - Escape (one way): if no checkpoint is accepted for `escapeDelay`, or a forced request is overdue, anyone triggers
///   escape. Then deposits, forced requests and checkpoints stop; each account claims its escape balance from the last
///   accepted checkpoint by a sum-tree proof, deposits after that checkpoint's credited count are refunded from the
///   vault's own chain, and certified withdrawals stay payable.
contract PerpsVault {
    // ------------------------------------------------------------------ constants
    uint8 public constant decimals = 6;
    uint256 internal constant MAX_VALIDATORS = 16;
    uint256 internal constant MAX_GUARDIANS = 7;
    uint64 internal constant MAX_PAUSE_DURATION = 14 days;
    uint64 internal constant MAX_LARGE_DELAY = 14 days;
    uint64 internal constant MIN_FORCED_DELAY = 1 hours;
    uint64 internal constant MAX_FORCED_DELAY = 7 days;
    uint64 internal constant MIN_ESCAPE_DELAY = 1 days;
    uint64 internal constant MAX_ESCAPE_DELAY = 30 days;
    /// Escape surplus (custody nobody can claim) becomes sweepable to `feeRecipient` this long after escape.
    uint64 public constant SURPLUS_DELAY = 90 days;
    /// Deepest sum-tree proof accepted (2^48 leaves).
    uint256 internal constant MAX_PROOF_DEPTH = 48;

    uint8 internal constant FORCED_CANCEL_ALL = 1;
    uint8 internal constant FORCED_CLOSE = 2;
    uint8 internal constant FORCED_WITHDRAW = 3;

    bytes32 internal constant DOMAIN_TYPEHASH =
        keccak256("EIP712Domain(string name,string version,uint256 chainId,address verifyingContract)");
    bytes32 internal constant NAME_HASH = keccak256("PosternPerpsVault");
    bytes32 internal constant VERSION_HASH = keccak256("1");
    bytes32 public constant CHECKPOINT_TYPEHASH = keccak256(
        "Checkpoint(uint64 height,uint64 time,bytes32 stateRoot,bytes32 logHash,uint64 depositsCredited,"
        "bytes32 depositsHead,uint64 forcedProcessed,bytes32 forcedHead,uint64 withdrawalCount,"
        "uint64 withdrawalsTotal,bytes32 withdrawalsRoot,uint64 escapeTotal,bytes32 escapeRoot)"
    );
    bytes32 public constant UNPAUSE_TYPEHASH = keccak256("Unpause(uint64 pauseCount,uint256 validUntil)");

    bytes32 internal constant DEPOSIT_CHAIN_TAG = bytes32("PosternPerps.deposit.v1");
    bytes32 internal constant FORCED_CHAIN_TAG = bytes32("PosternPerps.forced.v1");
    bytes32 internal constant WITHDRAWAL_LEAF_TAG = bytes32("PosternPerps.withdrawal.v1");
    bytes32 internal constant ESCAPE_LEAF_TAG = bytes32("PosternPerps.escape.v1");
    bytes32 private constant WITHDRAWAL_KEY = bytes32("W");
    bytes32 private constant ESCAPE_KEY = bytes32("E");

    uint256 private constant HALF_ORDER = 0x7fffffffffffffffffffffffffffffff5d576e7357a4501ddfe92f46681b20a0;

    // ------------------------------------------------------------------ immutables (fixed at deployment)
    address public immutable token;
    /// The protocol fee address: the engine's fee account is this address, and its records pay only here.
    address public immutable feeRecipient;
    uint8 public immutable threshold;
    uint8 public immutable unpauseThreshold;
    /// Supply-style cap on custody (deposits less committed payouts).
    uint64 public immutable cap;
    uint64 public immutable minDeposit;
    uint64 public immutable windowLimit;
    uint64 public immutable windowSeconds;
    uint64 public immutable largeThreshold;
    uint64 public immutable largeDelay;
    uint64 public immutable forcedDelay;
    uint64 public immutable escapeDelay;
    uint64 public immutable pauseDuration;
    uint64 public immutable pauseCooldown;
    uint64 public immutable deployedAt;

    address[] public validators;
    mapping(address => bool) public isValidator;
    address[] public guardians;
    mapping(address => bool) public isGuardian;

    // ------------------------------------------------------------------ state
    bool private entered;

    /// Deposits: count, chain head after n deposits (n = 0: `depositChainGenesis()`), cumulative amount after n.
    uint64 public depositCount;
    mapping(uint64 => bytes32) private depositHeadAt;
    mapping(uint64 => uint64) public depositTotalAt;
    mapping(address => bool) public hasDeposited;

    /// Forced requests: count, chain head after n, deadline of request i, and the last request of each account (+1).
    uint64 public forcedCount;
    mapping(uint64 => bytes32) private forcedHeadAt;
    mapping(uint64 => uint64) public forcedDeadline;
    mapping(address => uint64) public lastForcedPlusOne;

    /// Accepted checkpoints. `checkpointCount` = number accepted; `latest` = the newest.
    struct Stored {
        uint64 height;
        uint64 time;
        uint64 depositsCredited;
        uint64 forcedProcessed;
        uint64 withdrawalCount;
        uint64 withdrawalsTotal;
        uint64 escapeTotal;
        bytes32 withdrawalsRoot;
        bytes32 escapeRoot;
        bytes32 stateRoot;
    }

    uint64 public checkpointCount;
    Stored public latest;
    uint64 public lastCheckpointAt;
    /// Withdrawal root and leaf count of accepted checkpoint s (1-based).
    mapping(uint64 => bytes32) public withdrawalsRootAt;
    mapping(uint64 => uint64) public withdrawalCountAt;
    mapping(uint64 => uint64) public withdrawalsTotalAt;

    /// Payout accounting. `committedOut`: every amount a payout was registered for, plus refunds. `reserved`:
    /// registered and not yet transferred. Invariant: token balance >= custody() + reserved.
    uint64 public depositedTotal;
    uint64 public committedOut;
    uint64 public reserved;
    /// Amounts registered against withdrawal records, escape leaves, and refunded deposits.
    uint64 public withdrawnTotal;
    uint64 public escapeClaimedTotal;
    uint64 public refundedTotal;

    struct Payout {
        address recipient;
        uint64 total;
        uint64 paid;
        uint64 readyAt;
    }

    mapping(bytes32 => Payout) public payouts;
    mapping(uint64 => bool) public refunded;

    /// Release bucket.
    uint64 public bucket;
    uint64 public bucketUpdatedAt;

    /// Guardian pause chain (ProvenanceSourceVault semantics).
    uint64 public pausedUntil;
    uint64 public pauseCount;
    uint64 public pauseChain;
    uint64 public guardianBlockedUntil;
    mapping(address => uint64) public guardianLastChain;

    /// Escape mode: zero while normal; the time it was triggered.
    uint64 public escapeAt;
    bool public surplusSwept;

    // ------------------------------------------------------------------ types
    struct Config {
        address token;
        address feeRecipient;
        address[] validators;
        uint8 threshold;
        uint8 unpauseThreshold;
        uint64 cap;
        uint64 minDeposit;
        uint64 windowLimit;
        uint64 windowSeconds;
        uint64 largeThreshold;
        uint64 largeDelay;
        uint64 forcedDelay;
        uint64 escapeDelay;
        address[] guardians;
        uint64 pauseDuration;
        uint64 pauseCooldown;
    }

    struct Checkpoint {
        uint64 height;
        uint64 time;
        bytes32 stateRoot;
        bytes32 logHash;
        uint64 depositsCredited;
        bytes32 depositsHead;
        uint64 forcedProcessed;
        bytes32 forcedHead;
        uint64 withdrawalCount;
        uint64 withdrawalsTotal;
        bytes32 withdrawalsRoot;
        uint64 escapeTotal;
        bytes32 escapeRoot;
    }

    struct Withdrawal {
        uint64 index;
        address account;
        address recipient;
        uint64 amount;
    }

    /// Merkle sum-tree proof: `index` gives the side at each level (bit i = 1: the node is the right child at level i).
    struct SumProof {
        uint64 index;
        bytes32[] hashes;
        uint64[] sums;
    }

    // ------------------------------------------------------------------ events
    event Deposit(uint64 indexed index, address indexed account, address indexed sender, uint64 amount, bytes32 head);
    event ForcedRequest(
        uint64 indexed index,
        address indexed account,
        uint8 kind,
        uint32 marketId,
        uint64 amount,
        uint64 deadline,
        bytes32 head
    );
    event CheckpointAccepted(
        uint64 indexed seq, uint64 indexed height, bytes32 stateRoot, bytes32 withdrawalsRoot, bytes32 escapeRoot
    );
    event PayoutRegistered(bytes32 indexed key, address indexed recipient, uint64 total, uint64 readyAt);
    event Paid(bytes32 indexed key, address indexed recipient, uint64 amount);
    event Refunded(uint64 indexed index, address indexed account, uint64 amount);
    event GuardianPaused(address indexed guardian, uint64 indexed pauseCount, uint64 pausedUntil);
    event Unpaused(uint64 indexed pauseCount, uint64 guardianBlockedUntil);
    event EscapeTriggered(uint64 at, uint8 reason, uint64 checkpointSeq);
    event SurplusSwept(address indexed recipient, uint64 amount);

    // ------------------------------------------------------------------ errors
    error InvalidConfiguration();
    error Reentrancy();
    error InvalidDeposit();
    error InvalidTransfer();
    error InvalidCheckpoint();
    error InvalidCertificate();
    error Insolvent();
    error InvalidWithdrawal();
    error InvalidProof();
    error AlreadyRegistered();
    error NotRegistered();
    error NotReady();
    error RateLimited();
    error Paused();
    error NotGuardian();
    error InvalidPause();
    error InvalidForced();
    error EscapeActive();
    error NotEscape();
    error EscapeNotDue();
    error InvalidRefund();
    error InvalidSurplus();

    // ------------------------------------------------------------------ constructor
    constructor(Config memory c) {
        uint256 n = c.validators.length;
        if (
            c.token.code.length == 0 || c.feeRecipient == address(0) || c.feeRecipient == c.token
                || c.feeRecipient == address(this) || n == 0 || n > MAX_VALIDATORS || c.threshold < 2
                || c.threshold > n || 2 * uint256(c.threshold) <= n || c.unpauseThreshold < c.threshold
                || c.unpauseThreshold > n || c.cap == 0 || c.minDeposit == 0 || c.minDeposit > c.cap
                || c.windowLimit == 0 || c.windowLimit > c.cap || c.windowSeconds == 0 || c.largeThreshold == 0
                || c.largeThreshold > c.windowLimit || c.largeDelay == 0 || c.largeDelay > MAX_LARGE_DELAY
                || c.forcedDelay < MIN_FORCED_DELAY || c.forcedDelay > MAX_FORCED_DELAY
                || c.escapeDelay < MIN_ESCAPE_DELAY || c.escapeDelay > MAX_ESCAPE_DELAY
                || c.escapeDelay < c.forcedDelay
        ) revert InvalidConfiguration();
        (bool ok, bytes memory data) = c.token.staticcall(abi.encodeWithSignature("decimals()"));
        if (!ok || data.length != 32 || abi.decode(data, (uint256)) != 6) revert InvalidConfiguration();
        address previous;
        for (uint256 i; i < n; ++i) {
            address v = c.validators[i];
            if (v <= previous) revert InvalidConfiguration();
            previous = v;
            isValidator[v] = true;
            validators.push(v);
        }
        uint256 g = c.guardians.length;
        if (
            g > MAX_GUARDIANS || (g == 0 && (c.pauseDuration != 0 || c.pauseCooldown != 0))
                || (g != 0
                    && (c.pauseDuration == 0 || c.pauseDuration > MAX_PAUSE_DURATION || c.pauseCooldown < c.pauseDuration))
        ) revert InvalidConfiguration();
        previous = address(0);
        for (uint256 i; i < g; ++i) {
            address guardian = c.guardians[i];
            if (guardian <= previous || isValidator[guardian] || guardian == c.feeRecipient) {
                revert InvalidConfiguration();
            }
            previous = guardian;
            isGuardian[guardian] = true;
            guardians.push(guardian);
        }
        token = c.token;
        feeRecipient = c.feeRecipient;
        threshold = c.threshold;
        unpauseThreshold = c.unpauseThreshold;
        cap = c.cap;
        minDeposit = c.minDeposit;
        windowLimit = c.windowLimit;
        windowSeconds = c.windowSeconds;
        largeThreshold = c.largeThreshold;
        largeDelay = c.largeDelay;
        forcedDelay = c.forcedDelay;
        escapeDelay = c.escapeDelay;
        pauseDuration = c.pauseDuration;
        pauseCooldown = c.pauseCooldown;
        deployedAt = uint64(block.timestamp);
        lastCheckpointAt = uint64(block.timestamp);
        bucket = c.windowLimit;
        bucketUpdatedAt = uint64(block.timestamp);
    }

    modifier nonReentrant() {
        if (entered) revert Reentrancy();
        entered = true;
        _;
        entered = false;
    }

    // ------------------------------------------------------------------ views
    function validatorCount() external view returns (uint256) {
        return validators.length;
    }

    function guardianCount() external view returns (uint256) {
        return guardians.length;
    }

    function inEscape() public view returns (bool) {
        return escapeAt != 0;
    }

    function paused() public view returns (bool) {
        return block.timestamp < pausedUntil;
    }

    /// @notice What the vault holds for claims not yet registered: deposits less every committed payout and refund.
    function custody() public view returns (uint64) {
        return depositedTotal - committedOut;
    }

    function availableToRelease() public view returns (uint256) {
        uint256 refilled =
            uint256(bucket) + (block.timestamp - bucketUpdatedAt) * uint256(windowLimit) / windowSeconds;
        return refilled > windowLimit ? windowLimit : refilled;
    }

    function domainSeparator() public view returns (bytes32) {
        return keccak256(abi.encode(DOMAIN_TYPEHASH, NAME_HASH, VERSION_HASH, block.chainid, address(this)));
    }

    function depositChainGenesis() internal view returns (bytes32) {
        return keccak256(abi.encode(DEPOSIT_CHAIN_TAG, block.chainid, address(this)));
    }

    function forcedChainGenesis() internal view returns (bytes32) {
        return keccak256(abi.encode(FORCED_CHAIN_TAG, block.chainid, address(this)));
    }

    function depositHead(uint64 count) public view returns (bytes32) {
        return count == 0 ? depositChainGenesis() : depositHeadAt[count];
    }

    function forcedHead(uint64 count) public view returns (bytes32) {
        return count == 0 ? forcedChainGenesis() : forcedHeadAt[count];
    }

    /// @notice Chain link of deposit `index` (0-based) after `previous`.
    function depositLink(bytes32 previous, uint64 index, address account, uint64 amount)
        internal
        pure
        returns (bytes32)
    {
        return keccak256(abi.encode(previous, uint256(index), account, uint256(amount)));
    }

    function forcedLink(
        bytes32 previous,
        uint64 index,
        address account,
        uint8 kind,
        uint32 marketId,
        uint64 amount,
        uint64 deadline
    ) internal pure returns (bytes32) {
        return keccak256(
            abi.encode(
                previous, uint256(index), account, uint256(kind), uint256(marketId), uint256(amount), uint256(deadline)
            )
        );
    }

    function checkpointDigest(Checkpoint calldata cp) public view returns (bytes32) {
        bytes32 structHash = keccak256(
            abi.encode(
                CHECKPOINT_TYPEHASH,
                cp.height,
                cp.time,
                cp.stateRoot,
                cp.logHash,
                cp.depositsCredited,
                cp.depositsHead,
                cp.forcedProcessed,
                cp.forcedHead,
                cp.withdrawalCount,
                cp.withdrawalsTotal,
                cp.withdrawalsRoot,
                cp.escapeTotal,
                cp.escapeRoot
            )
        );
        return keccak256(abi.encodePacked("\x19\x01", domainSeparator(), structHash));
    }

    function unpauseDigest(uint64 count, uint256 validUntil) public view returns (bytes32) {
        return keccak256(
            abi.encodePacked(
                "\x19\x01", domainSeparator(), keccak256(abi.encode(UNPAUSE_TYPEHASH, count, validUntil))
            )
        );
    }

    function withdrawalLeaf(Withdrawal calldata w) internal pure returns (bytes32) {
        return keccak256(abi.encode(WITHDRAWAL_LEAF_TAG, uint256(w.index), w.account, w.recipient, uint256(w.amount)));
    }

    function escapeLeaf(address account, uint64 amount) internal pure returns (bytes32) {
        return keccak256(abi.encode(ESCAPE_LEAF_TAG, account, uint256(amount)));
    }

    /// @notice Parent of two sum-tree nodes: 81 bytes, so never equal to a 128- or 96-byte leaf preimage.
    function nodeHash(bytes32 leftHash, uint64 leftSum, bytes32 rightHash, uint64 rightSum)
        internal
        pure
        returns (bytes32)
    {
        return keccak256(abi.encodePacked(bytes1(0x01), leftHash, leftSum, rightHash, rightSum));
    }

    /// @notice Root hash and total of the tree a leaf belongs to by `proof`. Reverts on overflow or a malformed proof.
    function sumRoot(bytes32 leafHash, uint64 leafSum, SumProof calldata proof)
        internal
        pure
        returns (bytes32 h, uint64 s)
    {
        uint256 depth = proof.hashes.length;
        if (depth != proof.sums.length || depth > MAX_PROOF_DEPTH || proof.index >> depth != 0) {
            revert InvalidProof();
        }
        h = leafHash;
        s = leafSum;
        for (uint256 i; i < depth; ++i) {
            bytes32 sh = proof.hashes[i];
            uint64 ss = proof.sums[i];
            if ((proof.index >> i) & 1 == 0) h = nodeHash(h, s, sh, ss);
            else h = nodeHash(sh, ss, h, s);
            uint256 t = uint256(s) + ss;
            if (t > type(uint64).max) revert InvalidProof();
            s = uint64(t);
        }
    }

    function withdrawalKey(uint64 index) internal pure returns (bytes32) {
        return keccak256(abi.encode(WITHDRAWAL_KEY, uint256(index)));
    }

    function escapeKey(address account) internal pure returns (bytes32) {
        return keccak256(abi.encode(ESCAPE_KEY, account));
    }

    /// @notice 0: not due; 1: no checkpoint for `escapeDelay`; 2: a forced request is past its deadline unprocessed.
    function escapeReason() public view returns (uint8) {
        if (block.timestamp >= uint256(lastCheckpointAt) + escapeDelay) return 1;
        uint64 next = latest.forcedProcessed;
        if (next < forcedCount && block.timestamp > forcedDeadline[next]) return 2;
        return 0;
    }

    // ------------------------------------------------------------------ deposits
    /// @notice Deposits `amount` for engine account `account`. Credited by the engine after L2 finality.
    function deposit(uint64 amount, address account) external nonReentrant returns (uint64 index) {
        if (escapeAt != 0) revert EscapeActive();
        if (paused()) revert Paused();
        if (
            account == address(0) || account == address(this) || account == token || amount < minDeposit
                || uint256(custody()) + amount > cap || depositCount == type(uint64).max
        ) revert InvalidDeposit();
        uint256 beforeVault = _balance(address(this));
        uint256 beforeSender = _balance(msg.sender);
        _call(abi.encodeWithSignature("transferFrom(address,address,uint256)", msg.sender, address(this), amount));
        if (_balance(address(this)) != beforeVault + amount || _balance(msg.sender) + amount != beforeSender) {
            revert InvalidTransfer();
        }
        index = depositCount;
        bytes32 head = depositLink(depositHead(index), index, account, amount);
        depositCount = index + 1;
        depositHeadAt[index + 1] = head;
        depositTotalAt[index + 1] = depositTotalAt[index] + amount;
        depositedTotal += amount;
        hasDeposited[account] = true;
        emit Deposit(index, account, msg.sender, amount, head);
    }

    // ------------------------------------------------------------------ forced requests
    /// @notice The caller's own request that the engine must process before `forcedDelay` passes. One open request per
    ///         account; only an account that deposited (anti-spam). `amount` is used by FORCED_WITHDRAW only (0 = all
    ///         withdrawable); a forced withdrawal always pays the account itself. `marketId` 0 = every market.
    function requestForced(uint8 kind, uint32 marketId, uint64 amount) external nonReentrant returns (uint64 index) {
        if (escapeAt != 0) revert EscapeActive();
        if (
            kind < FORCED_CANCEL_ALL || kind > FORCED_WITHDRAW || !hasDeposited[msg.sender]
                || (kind != FORCED_WITHDRAW && amount != 0) || (kind == FORCED_WITHDRAW && marketId != 0)
                || forcedCount == type(uint64).max
        ) revert InvalidForced();
        uint64 last = lastForcedPlusOne[msg.sender];
        if (last != 0 && last - 1 >= latest.forcedProcessed) revert InvalidForced();
        index = forcedCount;
        uint64 deadline = uint64(block.timestamp) + forcedDelay;
        bytes32 head = forcedLink(forcedHead(index), index, msg.sender, kind, marketId, amount, deadline);
        forcedCount = index + 1;
        forcedHeadAt[index + 1] = head;
        forcedDeadline[index] = deadline;
        lastForcedPlusOne[msg.sender] = index + 1;
        emit ForcedRequest(index, msg.sender, kind, marketId, amount, deadline, head);
    }

    // ------------------------------------------------------------------ checkpoints
    function submitCheckpoint(Checkpoint calldata cp, bytes[] calldata signatures) external nonReentrant {
        if (escapeAt != 0) revert EscapeActive();
        Stored memory prev = latest;
        uint64 seq = checkpointCount;
        if (
            cp.height == 0 || (seq != 0 && (cp.height <= prev.height || cp.time <= prev.time))
                || cp.stateRoot == 0 || cp.depositsCredited < prev.depositsCredited
                || cp.depositsCredited > depositCount || cp.depositsHead != depositHead(cp.depositsCredited)
                || cp.forcedProcessed < prev.forcedProcessed || cp.forcedProcessed > forcedCount
                || cp.forcedHead != forcedHead(cp.forcedProcessed) || cp.withdrawalCount < prev.withdrawalCount
                || cp.withdrawalsTotal < prev.withdrawalsTotal
                || (cp.withdrawalCount == prev.withdrawalCount
                    && (cp.withdrawalsTotal != prev.withdrawalsTotal || cp.withdrawalsRoot != prev.withdrawalsRoot))
                || (cp.withdrawalCount == 0) != (cp.withdrawalsRoot == 0) || (cp.withdrawalCount == 0 && cp.withdrawalsTotal != 0)
                || (cp.escapeRoot == 0 && cp.escapeTotal != 0)
        ) revert InvalidCheckpoint();
        // A checkpoint may not leave behind a request that is already past its deadline.
        if (cp.forcedProcessed < forcedCount && block.timestamp > forcedDeadline[cp.forcedProcessed]) {
            revert InvalidCheckpoint();
        }
        _checkSolvency(cp.escapeTotal, cp.withdrawalsTotal, cp.depositsCredited);
        _checkCertificate(checkpointDigest(cp), signatures, threshold);
        seq += 1;
        checkpointCount = seq;
        latest = Stored({
            height: cp.height,
            time: cp.time,
            depositsCredited: cp.depositsCredited,
            forcedProcessed: cp.forcedProcessed,
            withdrawalCount: cp.withdrawalCount,
            withdrawalsTotal: cp.withdrawalsTotal,
            escapeTotal: cp.escapeTotal,
            withdrawalsRoot: cp.withdrawalsRoot,
            escapeRoot: cp.escapeRoot,
            stateRoot: cp.stateRoot
        });
        withdrawalsRootAt[seq] = cp.withdrawalsRoot;
        withdrawalCountAt[seq] = cp.withdrawalCount;
        withdrawalsTotalAt[seq] = cp.withdrawalsTotal;
        lastCheckpointAt = uint64(block.timestamp);
        emit CheckpointAccepted(seq, cp.height, cp.stateRoot, cp.withdrawalsRoot, cp.escapeRoot);
    }

    /// @dev Everything a checkpoint makes claimable (escape balances, withdrawal records not yet registered, deposits
    ///      it did not credit) must fit in what the vault holds unclaimed. Payouts and refunds lower both sides by the
    ///      same amount and deposits raise both, so the inequality holds until the next checkpoint: every claim the
    ///      latest checkpoint admits is payable.
    function _checkSolvency(uint64 escapeTotal, uint64 withdrawalsTotal, uint64 credited) private view {
        uint256 owed = uint256(escapeTotal) + (withdrawalsTotal - withdrawnTotal) + (depositedTotal - depositTotalAt[credited]);
        if (owed > custody()) revert Insolvent();
    }

    // ------------------------------------------------------------------ withdrawals
    /// @notice Registers withdrawal record `w`, proven against accepted checkpoint `seq`, and pays up to `want` now if it
    ///         is ready, not paused and the bucket allows (else it stays registered for `release`).
    function withdraw(uint64 seq, Withdrawal calldata w, SumProof calldata proof, uint64 want)
        external
        nonReentrant
        returns (bytes32 key)
    {
        if (seq == 0 || seq > checkpointCount || w.index >= withdrawalCountAt[seq] || proof.index != w.index) {
            revert InvalidWithdrawal();
        }
        if (
            w.amount == 0 || w.recipient == address(0) || w.recipient == address(this) || w.recipient == token
                || w.account == address(0) || (w.account == feeRecipient && w.recipient != feeRecipient)
        ) revert InvalidWithdrawal();
        (bytes32 root, uint64 total) = sumRoot(withdrawalLeaf(w), w.amount, proof);
        if (root != withdrawalsRootAt[seq] || total != withdrawalsTotalAt[seq]) revert InvalidProof();
        key = withdrawalKey(w.index);
        if (payouts[key].total != 0) revert AlreadyRegistered();
        // Bound by the newest committed total: the sum of all records ever paid never exceeds what the committee
        // certified (and the sum tree makes every record's amount part of that total).
        if (uint256(withdrawnTotal) + w.amount > latest.withdrawalsTotal) revert Insolvent();
        withdrawnTotal += w.amount;
        _register(key, w.recipient, w.amount);
        _payNow(key, want);
    }

    /// @notice Pays up to `want` of a registered payout once it is ready. Anyone may call; it pays the recipient.
    function release(bytes32 key, uint64 want) external nonReentrant {
        _release(key, want);
    }

    function _register(bytes32 key, address recipient, uint64 total) private {
        if (payouts[key].total != 0) revert AlreadyRegistered();
        uint64 readyAt = uint64(block.timestamp);
        if (total >= largeThreshold) readyAt += largeDelay;
        payouts[key] = Payout({recipient: recipient, total: total, paid: 0, readyAt: readyAt});
        committedOut += total;
        reserved += total;
        emit PayoutRegistered(key, recipient, total, readyAt);
    }

    /// @dev Registering paths pay what they can now and keep the registration otherwise (a large payout, a pause or
    ///      an empty bucket must never undo the claim); `release` is strict and says why it cannot pay.
    function _payNow(bytes32 key, uint64 want) private {
        if (want == 0 || paused() || block.timestamp < payouts[key].readyAt || availableToRelease() == 0) return;
        _release(key, want);
    }

    function _release(bytes32 key, uint64 want) private {
        Payout storage p = payouts[key];
        if (p.total == 0) revert NotRegistered();
        if (paused()) revert Paused();
        if (block.timestamp < p.readyAt) revert NotReady();
        uint64 remaining = p.total - p.paid;
        if (remaining == 0 || want == 0) revert InvalidWithdrawal();
        uint256 available = availableToRelease();
        if (available == 0) revert RateLimited();
        // The smallest of what was asked, what is left and what the bucket allows: a payout above the window limit
        // is paid in parts, and a relayer can always ask for everything.
        uint64 amount = want < remaining ? want : remaining;
        if (amount > available) amount = uint64(available);
        bucket = uint64(available - amount);
        bucketUpdatedAt = uint64(block.timestamp);
        p.paid += amount;
        reserved -= amount;
        _transferOut(p.recipient, amount);
        emit Paid(key, p.recipient, amount);
    }

    // ------------------------------------------------------------------ guardians
    function guardianCanPause(address guardian) public view returns (bool) {
        return isGuardian[guardian] && block.timestamp >= pausedUntil && block.timestamp >= guardianBlockedUntil
            && (_chainEnded() || guardianLastChain[guardian] != pauseChain);
    }

    function _chainEnded() private view returns (bool) {
        return pauseCount == 0 || block.timestamp >= uint256(pausedUntil) + pauseCooldown;
    }

    /// @notice Stops payouts and deposits for `pauseDuration`. Nothing else.
    function guardianPause() external nonReentrant {
        if (!isGuardian[msg.sender]) revert NotGuardian();
        if (!guardianCanPause(msg.sender)) revert InvalidPause();
        if (_chainEnded()) ++pauseChain;
        guardianLastChain[msg.sender] = pauseChain;
        uint64 count = ++pauseCount;
        pausedUntil = uint64(block.timestamp) + pauseDuration;
        emit GuardianPaused(msg.sender, count, pausedUntil);
    }

    /// @notice `unpauseThreshold` validators end the current pause early; guardians then wait `pauseCooldown`.
    function unpause(uint64 count, uint256 validUntil, bytes[] calldata signatures) external nonReentrant {
        if (block.timestamp >= pausedUntil || count != pauseCount || block.timestamp > validUntil) {
            revert InvalidPause();
        }
        _checkCertificate(unpauseDigest(count, validUntil), signatures, unpauseThreshold);
        pausedUntil = uint64(block.timestamp);
        guardianBlockedUntil = uint64(block.timestamp) + pauseCooldown;
        emit Unpaused(count, guardianBlockedUntil);
    }

    // ------------------------------------------------------------------ escape
    function triggerEscape() external nonReentrant {
        if (escapeAt != 0) revert EscapeActive();
        uint8 reason = escapeReason();
        if (reason == 0) revert EscapeNotDue();
        escapeAt = uint64(block.timestamp);
        emit EscapeTriggered(escapeAt, reason, checkpointCount);
    }

    /// @notice In escape: registers `account`'s escape balance from the latest checkpoint and pays up to `want` now.
    ///         The payout goes to `account` only; anyone may submit it.
    function claimEscape(address account, uint64 amount, SumProof calldata proof, uint64 want)
        external
        nonReentrant
        returns (bytes32 key)
    {
        if (escapeAt == 0) revert NotEscape();
        if (amount == 0 || account == address(0) || account == address(this) || account == token) {
            revert InvalidWithdrawal();
        }
        (bytes32 root, uint64 total) = sumRoot(escapeLeaf(account, amount), amount, proof);
        if (root != latest.escapeRoot || total != latest.escapeTotal) revert InvalidProof();
        key = escapeKey(account);
        if (payouts[key].total != 0) revert AlreadyRegistered();
        if (uint256(escapeClaimedTotal) + amount > latest.escapeTotal) revert Insolvent();
        escapeClaimedTotal += amount;
        _register(key, account, amount);
        _payNow(key, want);
    }

    /// @notice In escape: refunds deposit `index` that the latest checkpoint did not credit, to the credited account,
    ///         from the vault's own chain. No committee input, so no bucket, delay or pause applies.
    function refundDeposit(uint64 index, address account, uint64 amount) external nonReentrant {
        if (escapeAt == 0) revert NotEscape();
        if (
            index < latest.depositsCredited || index >= depositCount || refunded[index]
                || depositLink(depositHead(index), index, account, amount) != depositHeadAt[index + 1]
        ) revert InvalidRefund();
        refunded[index] = true;
        refundedTotal += amount;
        committedOut += amount;
        _transferOut(account, amount);
        emit Refunded(index, account, amount);
    }

    /// @notice In escape, `SURPLUS_DELAY` after it began: pays what no claim can ever reach (the engine's insurance
    ///         fund and any rounding or donation) to `feeRecipient`, once. Every amount still claimable stays.
    function sweepEscapeSurplus() external nonReentrant returns (uint64 amount) {
        if (escapeAt == 0) revert NotEscape();
        if (block.timestamp < uint256(escapeAt) + SURPLUS_DELAY || surplusSwept) revert InvalidSurplus();
        // Still claimable and not yet registered: escape balances, certified withdrawal records, uncredited deposits.
        uint64 unregistered = (latest.escapeTotal - escapeClaimedTotal) + (latest.withdrawalsTotal - withdrawnTotal)
            + (depositedTotal - depositTotalAt[latest.depositsCredited] - refundedTotal);
        uint256 balance = _balance(address(this));
        uint256 owed = uint256(unregistered) + reserved;
        if (balance <= owed) revert InvalidSurplus();
        uint256 surplus = balance - owed;
        if (surplus > type(uint64).max) revert InvalidSurplus();
        amount = uint64(surplus);
        surplusSwept = true;
        // custody() >= unregistered by the solvency invariant; what custody holds beyond it is the swept surplus, so
        // after the sweep custody() == unregistered and balance >= custody() + reserved still holds.
        committedOut += custody() - unregistered;
        _transferOut(feeRecipient, amount);
        emit SurplusSwept(feeRecipient, amount);
    }

    // ------------------------------------------------------------------ internals
    function _transferOut(address to, uint64 amount) private {
        uint256 beforeVault = _balance(address(this));
        uint256 beforeTo = _balance(to);
        _call(abi.encodeWithSignature("transfer(address,uint256)", to, uint256(amount)));
        if (_balance(address(this)) + amount != beforeVault || _balance(to) != beforeTo + amount) {
            revert InvalidTransfer();
        }
    }

    function _checkCertificate(bytes32 digest, bytes[] calldata signatures, uint8 required) private view {
        if (signatures.length < required || signatures.length > validators.length) revert InvalidCertificate();
        address previous;
        for (uint256 i; i < signatures.length; ++i) {
            bytes calldata signature = signatures[i];
            if (signature.length != 65) revert InvalidCertificate();
            bytes32 r;
            bytes32 s;
            uint8 v;
            assembly {
                r := calldataload(signature.offset)
                s := calldataload(add(signature.offset, 32))
                v := byte(0, calldataload(add(signature.offset, 64)))
            }
            if (uint256(s) > HALF_ORDER || (v != 27 && v != 28)) revert InvalidCertificate();
            address signer = ecrecover(digest, v, r, s);
            if (signer <= previous || !isValidator[signer]) revert InvalidCertificate();
            previous = signer;
        }
    }

    function _balance(address owner) private view returns (uint256) {
        (bool ok, bytes memory data) = token.staticcall(abi.encodeWithSignature("balanceOf(address)", owner));
        if (!ok || data.length != 32) revert InvalidTransfer();
        return abi.decode(data, (uint256));
    }

    function _call(bytes memory payload) private {
        (bool ok, bytes memory data) = token.call(payload);
        if (!ok || (data.length != 0 && (data.length != 32 || !abi.decode(data, (bool))))) revert InvalidTransfer();
    }
}

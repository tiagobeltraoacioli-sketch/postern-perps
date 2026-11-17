// SPDX-License-Identifier: UNLICENSED
pragma solidity 0.8.24;

/// @title PerpsVault: collateral custody of PosternDEX Perpetuals on the Bloch L2 (chain 700770)
/// @notice STATUS: PROTOTYPE, UNAUDITED. Not for deployment. Specification: docs/VAULT.md (v0.2), with docs/SPEC.md
///         section 11 (state root and proofs) as the engine side.
///
/// The vault holds the collateral token (bUSDC, 6 decimals) for the off-chain exchange engine. It never trusts the
/// sequencer: funds leave only against a checkpoint signed by `threshold` of a validator committee fixed at deployment,
/// within limits fixed at deployment. There is no owner, no admin, no upgrade, no setter, no rescue.
///
/// - Deposits: anyone deposits for an engine account; every deposit extends a hash chain and a cumulative total; a
///   checkpoint must name the vault's own chain head after the deposits it credited (R395-1).
/// - Checkpoints (EIP-712, domain "PosternPerpsVault"/"1"/chainid/this): k-of-n signatures, strictly ascending signer
///   addresses, low-s, v 27/28 (the bridge's `_checkCertificate`). The vault recomputes the engine's `stateRoot` from
///   the signed components (SPEC 11), so the withdrawal and account roots it pays against are the ones inside the
///   state root the validators replayed. Accepted only if every withdrawal record and every uncredited deposit fits in
///   custody.
/// - Withdrawals: a record of the engine's withdrawal tree, proven against any accepted checkpoint, is paid to its
///   recipient through a token bucket, in parts if needed; a record of at least `largeThreshold` waits `largeDelay`.
///   Records of the engine's PROTOCOL_FEE account (0x..01) pay only `feeRecipient`.
/// - Guardians can only pause payouts and deposits (the bridge vault's bounded pause chain); `unpauseThreshold`
///   validators end a pause early. A pause never stops checkpoints, forced requests, escape or refunds.
/// - Forced requests: an account that deposited posts cancel-all / close-all / withdraw on the L2; no checkpoint is
///   accepted while one is past its deadline unprocessed, and anyone may then trigger escape.
/// - Escape (one way): after a forced request goes overdue, or after `escapeDelay` without a checkpoint once one was
///   accepted, anyone triggers escape. Each account then claims its `escapeValue` from the latest checkpoint's account
///   tree, scaled down pro rata if the escape pool is smaller than `totalEscape`; deposits that checkpoint did not
///   credit are refunded from the vault's own chain; certified withdrawal records stay payable.
contract PerpsVault {
    // ------------------------------------------------------------------ constants
    uint8 public constant decimals = 6;
    uint256 internal constant MAX_VALIDATORS = 16;
    uint256 internal constant MAX_GUARDIANS = 7;
    uint64 internal constant MAX_PAUSE_DURATION = 14 days;
    uint64 internal constant MAX_LARGE_DELAY = 14 days;
    /// A forced request is processed by the engine only after L2 finality: its deadline must cover the worst L2
    /// finality lag the venue accepts (R395-25). One day is the floor; the launch proposal is two days.
    uint64 internal constant MIN_FORCED_DELAY = 1 days;
    uint64 internal constant MAX_FORCED_DELAY = 7 days;
    uint64 internal constant MAX_ESCAPE_DELAY = 30 days;
    /// Escape surplus (custody nobody can claim) becomes sweepable to `feeRecipient` this long after escape.
    uint64 public constant SURPLUS_DELAY = 90 days;
    /// SPEC 11: account and withdrawal trees have fixed depth 32.
    uint256 internal constant TREE_DEPTH = 32;

    /// SPEC 14 forced kinds.
    uint8 internal constant FORCED_CANCEL_ALL = 0;
    uint8 internal constant FORCED_CLOSE_ALL = 1;
    uint8 internal constant FORCED_WITHDRAW = 2;

    /// SPEC 8 system accounts (no key). Only PROTOCOL_FEE is ever paid, and only to `feeRecipient`.
    address internal constant PROTOCOL_FEE = address(1);
    /// Addresses at or below this are keyless (system accounts, precompiles): never a payout recipient (R395-26).
    uint160 internal constant KEYLESS_MAX = 0xffff;

    bytes32 internal constant DOMAIN_TYPEHASH =
        keccak256("EIP712Domain(string name,string version,uint256 chainId,address verifyingContract)");
    bytes32 internal constant NAME_HASH = keccak256("PosternPerpsVault");
    bytes32 internal constant VERSION_HASH = keccak256("1");
    bytes32 public constant CHECKPOINT_TYPEHASH = keccak256(
        "Checkpoint(uint64 height,uint64 time,bytes32 logHash,bytes32 accountsRoot,bytes32 marketsRoot,"
        "bytes32 withdrawalsRoot,uint64 nextDepositIndex,uint64 nextForcedIndex,uint64 nextOrderId,"
        "uint128 totalEscape,uint128 dust,bytes32 auxHash,bytes32 depositsHead,bytes32 forcedHead,uint64 withdrawalCount,"
        "uint64 withdrawalsTotal)"
    );
    bytes32 public constant UNPAUSE_TYPEHASH = keccak256("Unpause(uint64 pauseCount,uint256 validUntil)");

    bytes32 internal constant DEPOSIT_CHAIN_TAG = bytes32("PosternPerps.deposit.v1");
    bytes32 internal constant FORCED_CHAIN_TAG = bytes32("PosternPerps.forced.v1");
    bytes32 private constant WITHDRAWAL_KEY = bytes32("W");
    bytes32 private constant ESCAPE_KEY = bytes32("E");

    uint256 private constant HALF_ORDER = 0x7fffffffffffffffffffffffffffffff5d576e7357a4501ddfe92f46681b20a0;

    // ------------------------------------------------------------------ immutables (fixed at deployment)
    address public immutable token;
    /// The owner's protocol fee address; the engine's PROTOCOL_FEE records and escape share pay only here.
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

    address[] public validators;
    mapping(address => bool) public isValidator;
    address[] public guardians;
    mapping(address => bool) public isGuardian;

    // ------------------------------------------------------------------ state
    bool private entered;

    /// Deposits: count, chain head after n deposits (n = 0: genesis), cumulative amount after n.
    uint64 public depositCount;
    mapping(uint64 => bytes32) private depositHeadAt;
    mapping(uint64 => uint64) public depositTotalAt;
    mapping(address => bool) public hasDeposited;

    /// Forced requests: count, chain head after n, deadline (L2 seconds) of request i, last request of an account + 1.
    uint64 public forcedCount;
    mapping(uint64 => bytes32) private forcedHeadAt;
    mapping(uint64 => uint64) public forcedDeadline;
    mapping(address => uint64) public lastForcedPlusOne;

    /// The newest accepted checkpoint, as the vault needs it.
    struct Stored {
        uint64 height;
        uint64 time;
        uint64 nextDepositIndex;
        uint64 nextForcedIndex;
        uint64 withdrawalCount;
        uint64 withdrawalsTotal;
        uint128 totalEscape;
        bytes32 accountsRoot;
        bytes32 stateRoot;
    }

    uint64 public checkpointCount;
    Stored public latest;
    /// L2 time of the newest acceptance (0 before the first: escape by silence needs one accepted checkpoint, R395-24).
    uint64 public lastCheckpointAt;
    /// Withdrawal root and record count of accepted checkpoint s (1-based).
    mapping(uint64 => bytes32) public withdrawalsRootAt;
    mapping(uint64 => uint64) public withdrawalCountAt;

    /// Payout accounting. `committedOut`: every registered payout plus refunds and the surplus sweep. `reserved`:
    /// registered and not yet transferred. Invariant: token balance >= custody() + reserved.
    uint64 public depositedTotal;
    uint64 public committedOut;
    uint64 public reserved;
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

    /// Escape: time it began (0: normal) and the pro-rata numerator `min(pool, totalEscape)` fixed then.
    uint64 public escapeAt;
    uint64 public escapePayable;
    /// Unscaled pool fixed at the trigger; INSURANCE + BACKSTOP escape values once proven (R395-33).
    uint64 public escapePool;
    uint128 public subordinatedTotal;
    bool public systemSharesProven;
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

    /// The components of SPEC 11's `stateRoot` the vault needs, plus the chain heads and withdrawal totals.
    struct Checkpoint {
        uint64 height;
        uint64 time;
        bytes32 logHash;
        bytes32 accountsRoot;
        bytes32 marketsRoot;
        bytes32 withdrawalsRoot;
        uint64 nextDepositIndex;
        uint64 nextForcedIndex;
        uint64 nextOrderId;
        uint128 totalEscape;
        uint128 dust;
        bytes32 auxHash;
        bytes32 depositsHead;
        bytes32 forcedHead;
        uint64 withdrawalCount;
        uint64 withdrawalsTotal;
    }

    /// SPEC 11 withdrawal leaf data.
    struct Withdrawal {
        uint64 withdrawalId;
        address account;
        address recipient;
        uint64 amount;
        uint64 height;
    }

    /// SPEC 11 (0.1.1) account leaf data.
    struct Account {
        address account;
        uint32 index;
        int128 balance;
        uint64 escapeValue;
        bytes32 positionsHash;
    }

    // ------------------------------------------------------------------ events
    event Deposit(uint64 indexed index, address indexed account, address indexed sender, uint64 amount, bytes32 head);
    event ForcedRequest(
        uint64 indexed index, address indexed account, uint8 kind, uint64 deadline, uint64 arg, bytes32 head
    );
    event CheckpointAccepted(uint64 indexed seq, uint64 indexed height, bytes32 stateRoot, bytes32 withdrawalsRoot);
    event PayoutRegistered(bytes32 indexed key, address indexed recipient, uint64 total, uint64 readyAt);
    event Paid(bytes32 indexed key, address indexed recipient, uint64 amount);
    event Refunded(uint64 indexed index, address indexed account, uint64 amount);
    event GuardianPaused(address indexed guardian, uint64 indexed pauseCount, uint64 pausedUntil);
    event Unpaused(uint64 indexed pauseCount, uint64 guardianBlockedUntil);
    event EscapeTriggered(uint64 at, uint8 reason, uint64 checkpointSeq, uint64 payable_, uint128 totalEscape);
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
            c.token.code.length == 0 || uint160(c.feeRecipient) <= KEYLESS_MAX || c.feeRecipient == c.token
                || c.feeRecipient == address(this) || n == 0 || n > MAX_VALIDATORS || c.threshold < 2
                || c.threshold > n || 2 * uint256(c.threshold) <= n || c.unpauseThreshold < c.threshold
                || c.unpauseThreshold > n || c.cap == 0 || c.minDeposit == 0 || c.minDeposit > c.cap
                || c.windowLimit == 0 || c.windowLimit > c.cap || c.windowSeconds == 0 || c.largeThreshold == 0
                || c.largeThreshold > c.windowLimit || c.largeDelay == 0 || c.largeDelay > MAX_LARGE_DELAY
                || c.forcedDelay < MIN_FORCED_DELAY || c.forcedDelay > MAX_FORCED_DELAY
                || c.escapeDelay < c.forcedDelay || c.escapeDelay > MAX_ESCAPE_DELAY
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

    function depositHead(uint64 count) public view returns (bytes32) {
        return count == 0 ? keccak256(abi.encode(DEPOSIT_CHAIN_TAG, block.chainid, address(this))) : depositHeadAt[count];
    }

    function forcedHead(uint64 count) public view returns (bytes32) {
        return count == 0 ? keccak256(abi.encode(FORCED_CHAIN_TAG, block.chainid, address(this))) : forcedHeadAt[count];
    }

    function checkpointDigest(Checkpoint calldata cp) public view returns (bytes32) {
        // abi.encode of static values is their 32-byte words in order: two halves concatenated are the same bytes as
        // one call (split only to stay within the legacy code generator's stack).
        bytes32 structHash = keccak256(
            bytes.concat(
                abi.encode(
                    CHECKPOINT_TYPEHASH,
                    cp.height,
                    cp.time,
                    cp.logHash,
                    cp.accountsRoot,
                    cp.marketsRoot,
                    cp.withdrawalsRoot,
                    cp.nextDepositIndex
                ),
                abi.encode(
                    cp.nextForcedIndex,
                    cp.nextOrderId,
                    cp.totalEscape,
                    cp.dust,
                    cp.auxHash,
                    cp.depositsHead,
                    cp.forcedHead,
                    cp.withdrawalCount,
                    cp.withdrawalsTotal
                )
            )
        );
        return keccak256(abi.encodePacked("\x19\x01", domainSeparator(), structHash));
    }

    /// @notice SPEC 11 (0.1.1): `stateRoot = H("PosternPerps/state/v1" || be64 height || be64 time || accountsRoot ||
    ///         marketsRoot || withdrawalsRoot || systemHash)`, `systemHash = H("PPv1sys" || be64 nextDepositIndex ||
    ///         be64 nextForcedIndex || be64 nextOrderId || be128 totalEscape || be128 dust || auxHash || be64 withdrawalCount || be64 withdrawalsTotal)` (SPEC 0.1.5).
    function stateRootOf(Checkpoint calldata cp) public pure returns (bytes32) {
        bytes32 systemHash = keccak256(
            abi.encodePacked(
                "PPv1sys",
                cp.nextDepositIndex,
                cp.nextForcedIndex,
                cp.nextOrderId,
                cp.totalEscape,
                cp.dust,
                cp.auxHash,
                cp.withdrawalCount,
                cp.withdrawalsTotal
            )
        );
        return keccak256(
            abi.encodePacked(
                "PosternPerps/state/v1",
                cp.height,
                cp.time,
                cp.accountsRoot,
                cp.marketsRoot,
                cp.withdrawalsRoot,
                systemHash
            )
        );
    }

    function unpauseDigest(uint64 count, uint256 validUntil) public view returns (bytes32) {
        return keccak256(
            abi.encodePacked(
                "\x19\x01", domainSeparator(), keccak256(abi.encode(UNPAUSE_TYPEHASH, count, validUntil))
            )
        );
    }

    function withdrawalKey(uint64 withdrawalId) public pure returns (bytes32) {
        return keccak256(abi.encode(WITHDRAWAL_KEY, uint256(withdrawalId)));
    }

    function escapeKey(address account) public pure returns (bytes32) {
        return keccak256(abi.encode(ESCAPE_KEY, account));
    }

    /// @notice 0: not due; 1: no checkpoint for `escapeDelay` since the last accepted one; 2: a forced request is past
    ///         its deadline unprocessed.
    function escapeReason() public view returns (uint8) {
        uint64 next = latest.nextForcedIndex;
        if (next < forcedCount && block.timestamp > forcedDeadline[next]) return 2;
        if (lastCheckpointAt != 0 && block.timestamp >= uint256(lastCheckpointAt) + escapeDelay) return 1;
        return 0;
    }

    /// @dev SPEC 11 Merkle path: leaf `H(0x00 || data)`, node `H(0x01 || left || right)`, depth 32, bit i of `index` set
    ///      = right child at level i.
    function _root(bytes32 leaf, uint64 index, bytes32[] calldata siblings) internal pure returns (bytes32 h) {
        if (siblings.length != TREE_DEPTH || index >> TREE_DEPTH != 0) revert InvalidProof();
        h = leaf;
        for (uint256 i; i < TREE_DEPTH; ++i) {
            h = (index >> i) & 1 == 0
                ? keccak256(abi.encodePacked(bytes1(0x01), h, siblings[i]))
                : keccak256(abi.encodePacked(bytes1(0x01), siblings[i], h));
        }
    }

    function _withdrawalLeaf(Withdrawal calldata w) internal pure returns (bytes32) {
        return keccak256(
            abi.encodePacked(bytes1(0x00), "PPv1wdrl", w.withdrawalId, w.account, w.recipient, w.amount, w.height)
        );
    }

    function _accountLeaf(Account calldata a) internal pure returns (bytes32) {
        return keccak256(
            abi.encodePacked(
                bytes1(0x00), "PPv1acct", a.account, a.index, a.balance, a.escapeValue, a.positionsHash
            )
        );
    }

    function _keyless(address a) internal pure returns (bool) {
        return uint160(a) <= KEYLESS_MAX;
    }

    // ------------------------------------------------------------------ deposits
    /// @notice Deposits `amount` for engine account `account` (credited by the engine after L2 finality).
    function deposit(uint64 amount, address account) external nonReentrant returns (uint64 index) {
        if (escapeAt != 0) revert EscapeActive();
        if (paused()) revert Paused();
        if (
            account == address(this) || account == token || amount < minDeposit || _keyless(account)
                || uint256(custody()) + amount > cap
        ) revert InvalidDeposit();
        uint256 beforeVault = _balance(address(this));
        uint256 beforeSender = _balance(msg.sender);
        _call(abi.encodeWithSignature("transferFrom(address,address,uint256)", msg.sender, address(this), amount));
        if (_balance(address(this)) != beforeVault + amount || _balance(msg.sender) + amount != beforeSender) {
            revert InvalidTransfer();
        }
        index = depositCount;
        bytes32 head = keccak256(abi.encode(depositHead(index), uint256(index), account, uint256(amount)));
        depositCount = index + 1;
        depositHeadAt[index + 1] = head;
        depositTotalAt[index + 1] = depositTotalAt[index] + amount;
        depositedTotal += amount;
        hasDeposited[account] = true;
        emit Deposit(index, account, msg.sender, amount, head);
    }

    // ------------------------------------------------------------------ forced requests
    /// @notice The caller's own request (SPEC 14 kinds: 0 CancelAll, 1 CloseAll, 2 Withdraw `arg` gross to itself),
    ///         which the engine must process before `forcedDelay` passes. One open request per account; only an account
    ///         that deposited (anti-spam). `arg` must be 0 except for Withdraw.
    function requestForced(uint8 kind, uint64 arg) external nonReentrant returns (uint64 index) {
        if (escapeAt != 0) revert EscapeActive();
        if (kind > FORCED_WITHDRAW || !hasDeposited[msg.sender] || (kind != FORCED_WITHDRAW && arg != 0)) {
            revert InvalidForced();
        }
        uint64 last = lastForcedPlusOne[msg.sender];
        if (last != 0 && last - 1 >= latest.nextForcedIndex) revert InvalidForced();
        index = forcedCount;
        uint64 deadline = uint64(block.timestamp) + forcedDelay;
        bytes32 head = keccak256(
            abi.encode(forcedHead(index), uint256(index), msg.sender, uint256(kind), uint256(deadline), uint256(arg))
        );
        forcedCount = index + 1;
        forcedHeadAt[index + 1] = head;
        forcedDeadline[index] = deadline;
        lastForcedPlusOne[msg.sender] = index + 1;
        emit ForcedRequest(index, msg.sender, kind, deadline, arg, head);
    }

    // ------------------------------------------------------------------ checkpoints
    function submitCheckpoint(Checkpoint calldata cp, bytes[] calldata signatures) external nonReentrant {
        if (escapeAt != 0) revert EscapeActive();
        Stored memory prev = latest;
        uint64 seq = checkpointCount;
        if (
            cp.height == 0 || (seq != 0 && (cp.height <= prev.height || cp.time <= prev.time))
                || cp.nextDepositIndex < prev.nextDepositIndex || cp.nextDepositIndex > depositCount
                || cp.depositsHead != depositHead(cp.nextDepositIndex) || cp.nextForcedIndex < prev.nextForcedIndex
                || cp.nextForcedIndex > forcedCount || cp.forcedHead != forcedHead(cp.nextForcedIndex)
                || cp.withdrawalCount < prev.withdrawalCount || cp.withdrawalsTotal < prev.withdrawalsTotal
                || (seq != 0 && cp.withdrawalCount == prev.withdrawalCount
                    && (cp.withdrawalsTotal != prev.withdrawalsTotal || cp.withdrawalsRoot != withdrawalsRootAt[seq]))
                || (cp.withdrawalCount == 0 && cp.withdrawalsTotal != 0)
        ) revert InvalidCheckpoint();
        // A checkpoint may not leave behind a request that is already past its deadline.
        if (cp.nextForcedIndex < forcedCount && block.timestamp > forcedDeadline[cp.nextForcedIndex]) {
            revert InvalidCheckpoint();
        }
        // Every withdrawal record not yet registered and every deposit not yet credited must be payable from custody.
        // Escape values are not part of this test: they are paid pro rata from what remains (R395-23).
        if (
            uint256(cp.withdrawalsTotal - withdrawnTotal) + (depositedTotal - depositTotalAt[cp.nextDepositIndex])
                > custody()
        ) revert Insolvent();
        _checkCertificate(checkpointDigest(cp), signatures, threshold);
        bytes32 stateRoot = stateRootOf(cp);
        seq += 1;
        checkpointCount = seq;
        latest = Stored({
            height: cp.height,
            time: cp.time,
            nextDepositIndex: cp.nextDepositIndex,
            nextForcedIndex: cp.nextForcedIndex,
            withdrawalCount: cp.withdrawalCount,
            withdrawalsTotal: cp.withdrawalsTotal,
            totalEscape: cp.totalEscape,
            accountsRoot: cp.accountsRoot,
            stateRoot: stateRoot
        });
        withdrawalsRootAt[seq] = cp.withdrawalsRoot;
        withdrawalCountAt[seq] = cp.withdrawalCount;
        lastCheckpointAt = uint64(block.timestamp);
        emit CheckpointAccepted(seq, cp.height, stateRoot, cp.withdrawalsRoot);
    }

    // ------------------------------------------------------------------ withdrawals
    /// @notice Registers withdrawal record `w`, proven against accepted checkpoint `seq`, and pays up to `want` now if it
    ///         is ready, not paused and the bucket allows (else it stays registered for `release`).
    function withdraw(uint64 seq, Withdrawal calldata w, bytes32[] calldata siblings, uint64 want)
        external
        nonReentrant
        returns (bytes32 key)
    {
        if (seq == 0 || seq > checkpointCount || w.withdrawalId >= withdrawalCountAt[seq]) revert InvalidWithdrawal();
        if (
            w.amount == 0 || _keyless(w.recipient) || w.recipient == address(this) || w.recipient == token
                || (w.account == PROTOCOL_FEE && w.recipient != feeRecipient)
                || (_keyless(w.account) && w.account != PROTOCOL_FEE)
        ) revert InvalidWithdrawal();
        if (_root(_withdrawalLeaf(w), w.withdrawalId, siblings) != withdrawalsRootAt[seq]) revert InvalidProof();
        key = withdrawalKey(w.withdrawalId);
        if (payouts[key].total != 0) revert AlreadyRegistered();
        // The sum of all records ever registered never exceeds what the committee certified.
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
    /// @notice Enters escape for good. Fixes the escape pool: custody less every certified withdrawal record not yet
    ///         registered and every deposit the latest checkpoint did not credit (both paid in full). Escape values
    ///         are then paid `escapeValue * min(pool, totalEscape) / totalEscape`, rounded down.
    function triggerEscape() external nonReentrant {
        if (escapeAt != 0) revert EscapeActive();
        uint8 reason = escapeReason();
        if (reason == 0) revert EscapeNotDue();
        uint64 pool = custody() - (latest.withdrawalsTotal - withdrawnTotal)
            - (depositedTotal - depositTotalAt[latest.nextDepositIndex]);
        escapePool = pool;
        escapeAt = uint64(block.timestamp);
        emit EscapeTriggered(escapeAt, reason, checkpointCount, pool, latest.totalEscape);
    }

    /// @notice In escape, before any account claims: proves the INSURANCE (0x..02, index 1) and BACKSTOP (0x..03,
    ///         index 2) leaves of the latest account tree (SPEC 8). Their escape values are subordinated (coordinator
    ///         decision on R395-33): user (senior) escape values are paid `escapeValue * min(pool, senior) / senior`
    ///         with `senior = totalEscape - insurance - backstop`; only `min(insurance + backstop, pool - senior paid)`
    ///         remains for them, payable to `feeRecipient` only `SURPLUS_DELAY` after escape began. Anyone may call.
    function proveSystemShares(
        Account calldata insurance,
        bytes32[] calldata insuranceSiblings,
        Account calldata backstop,
        bytes32[] calldata backstopSiblings
    ) external nonReentrant {
        if (escapeAt == 0) revert NotEscape();
        if (systemSharesProven) revert AlreadyRegistered();
        if (
            insurance.account != address(2) || insurance.index != 1 || backstop.account != address(3)
                || backstop.index != 2
        ) revert InvalidWithdrawal();
        if (
            _root(_accountLeaf(insurance), 1, insuranceSiblings) != latest.accountsRoot
                || _root(_accountLeaf(backstop), 2, backstopSiblings) != latest.accountsRoot
        ) revert InvalidProof();
        uint128 sub = uint128(insurance.escapeValue) + backstop.escapeValue;
        if (sub > latest.totalEscape) revert InvalidProof();
        uint128 senior = latest.totalEscape - sub;
        uint64 pool = escapePool;
        escapePayable = senior < pool ? uint64(senior) : pool;
        subordinatedTotal = sub;
        systemSharesProven = true;
        uint64 rest = pool - escapePayable;
        uint64 subPaid = sub < rest ? uint64(sub) : rest;
        if (subPaid != 0) {
            bytes32 key = escapeKey(address(2));
            _register(key, feeRecipient, subPaid);
            payouts[key].readyAt = escapeAt + SURPLUS_DELAY;
        }
    }

    /// @notice In escape: registers the account's pro-rata escape value from the latest checkpoint's account tree and
    ///         pays up to `want` now. Anyone may submit; the payout goes to the account. Keyless accounts (SPEC 8) pay
    ///         `feeRecipient`: PROTOCOL_FEE at once, INSURANCE and BACKSTOP only `SURPLUS_DELAY` after escape began
    ///         (the same rule as the surplus sweep; an owner decision, VAULT.md section 10).
    function claimEscape(Account calldata a, bytes32[] calldata siblings, uint64 want)
        external
        nonReentrant
        returns (bytes32 key)
    {
        if (escapeAt == 0) revert NotEscape();
        if (a.escapeValue == 0 || a.account == address(this) || a.account == token) revert InvalidWithdrawal();
        if (!systemSharesProven) revert NotReady();
        if (_keyless(a.account) && a.account != PROTOCOL_FEE) revert InvalidWithdrawal();
        address recipient = a.account == PROTOCOL_FEE ? feeRecipient : a.account;
        uint256 senior = latest.totalEscape - subordinatedTotal;
        if (senior == 0) revert InvalidWithdrawal();
        if (_root(_accountLeaf(a), a.index, siblings) != latest.accountsRoot) revert InvalidProof();
        key = escapeKey(a.account);
        if (payouts[key].total != 0) revert AlreadyRegistered();
        uint64 amount = uint64(uint256(a.escapeValue) * escapePayable / senior);
        if (amount == 0) revert InvalidWithdrawal();
        if (uint256(escapeClaimedTotal) + amount > escapePayable) revert Insolvent();
        escapeClaimedTotal += amount;
        _register(key, recipient, amount);
        _payNow(key, want);
    }

    /// @notice In escape: refunds deposit `index` that the latest checkpoint did not credit, to the credited account,
    ///         from the vault's own chain. No committee input, so no bucket, delay or pause applies.
    function refundDeposit(uint64 index, address account, uint64 amount) external nonReentrant {
        if (escapeAt == 0) revert NotEscape();
        if (
            index < latest.nextDepositIndex || index >= depositCount || refunded[index]
                || keccak256(abi.encode(depositHead(index), uint256(index), account, uint256(amount)))
                    != depositHeadAt[index + 1]
        ) revert InvalidRefund();
        refunded[index] = true;
        refundedTotal += amount;
        committedOut += amount;
        _transferOut(account, amount);
        emit Refunded(index, account, amount);
    }

    /// @notice In escape, `SURPLUS_DELAY` after it began: pays what no claim can ever reach (rounding, the shares of
    ///         escape shares nobody proved, donations) to `feeRecipient`, once. Every amount still claimable stays.
    function sweepEscapeSurplus() external nonReentrant returns (uint64 amount) {
        if (escapeAt == 0) revert NotEscape();
        // Before the system shares are proven the user pool is not fixed: nothing is surplus yet.
        if (block.timestamp < uint256(escapeAt) + SURPLUS_DELAY || surplusSwept || !systemSharesProven) {
            revert InvalidSurplus();
        }
        // Still claimable and not yet registered: escape values, certified withdrawal records, uncredited deposits.
        uint64 unregistered = (escapePayable - escapeClaimedTotal) + (latest.withdrawalsTotal - withdrawnTotal)
            + (depositedTotal - depositTotalAt[latest.nextDepositIndex] - refundedTotal);
        uint256 balance = _balance(address(this));
        uint256 owed = uint256(unregistered) + reserved;
        if (balance <= owed) revert InvalidSurplus();
        uint256 surplus = balance - owed;
        if (surplus > type(uint64).max) revert InvalidSurplus();
        amount = uint64(surplus);
        surplusSwept = true;
        // custody() >= unregistered by the solvency invariant; what custody holds beyond it is swept, so afterwards
        // custody() == unregistered and balance >= custody() + reserved still holds.
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

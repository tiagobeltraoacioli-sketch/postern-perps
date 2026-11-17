// SPDX-License-Identifier: UNLICENSED
pragma solidity 0.8.24;

import {PerpsVault} from "../src/PerpsVault.sol";

/// Minimal cheatcode interface (no forge-std dependency, as in the bridge repository's suites).
interface Vm {
    function addr(uint256 privateKey) external pure returns (address);
    function sign(uint256 privateKey, bytes32 digest) external pure returns (uint8 v, bytes32 r, bytes32 s);
    function warp(uint256 timestamp) external;
    function prank(address sender) external;
    function startPrank(address sender) external;
    function stopPrank() external;
    function expectRevert(bytes4 selector) external;
    function expectRevert() external;
    function chainId(uint256 newChainId) external;
    function assume(bool condition) external;
    function label(address account, string calldata name) external;
    function etch(address target, bytes calldata code) external;
}

/// Six-decimal token with the bUSDC transfer semantics that matter here: returns true, reverts on failure,
/// no fee, no pause on transfers, no blacklist (SignerMintedToken._transfer).
contract MockUSDC {
    uint8 public decimals = 6;
    mapping(address => uint256) public balanceOf;
    mapping(address => mapping(address => uint256)) public allowance;
    uint256 public totalSupply;
    /// Test switches for hostile-token cases.
    uint256 public skim;
    bool public returnFalse;

    function setDecimals(uint8 d) external {
        decimals = d;
    }

    function setSkim(uint256 s) external {
        skim = s;
    }

    function setReturnFalse(bool b) external {
        returnFalse = b;
    }

    function mint(address to, uint256 v) external {
        balanceOf[to] += v;
        totalSupply += v;
    }

    function approve(address s, uint256 v) external returns (bool) {
        allowance[msg.sender][s] = v;
        return true;
    }

    function transfer(address to, uint256 v) external returns (bool) {
        _move(msg.sender, to, v);
        return !returnFalse;
    }

    function transferFrom(address f, address to, uint256 v) external returns (bool) {
        uint256 a = allowance[f][msg.sender];
        require(a >= v, "allowance");
        if (a != type(uint256).max) allowance[f][msg.sender] = a - v;
        _move(f, to, v);
        return !returnFalse;
    }

    function _move(address f, address to, uint256 v) private {
        require(balanceOf[f] >= v, "balance");
        balanceOf[f] -= v;
        balanceOf[to] += v - skim;
        totalSupply -= skim;
    }
}

/// SPEC 11 Merkle tree builder (test side; the engine builds the same tree in Rust): leaves at positions
/// 0..n-1, depth 32, empty subtrees Z(i), node H(0x01 || left || right).
library Tree {
    uint256 internal constant DEPTH = 32;

    function zero(uint256 level) internal pure returns (bytes32 z) {
        for (uint256 i; i < level; ++i) z = keccak256(abi.encodePacked(bytes1(0x01), z, z));
    }

    function root(bytes32[] memory leaves) internal pure returns (bytes32) {
        if (leaves.length == 0) return zero(DEPTH);
        bytes32[] memory level = _copy(leaves);
        uint256 n = level.length;
        bytes32 z;
        for (uint256 d; d < DEPTH; ++d) {
            uint256 m = (n + 1) / 2;
            for (uint256 i; i < m; ++i) {
                bytes32 l = level[2 * i];
                bytes32 r = 2 * i + 1 < n ? level[2 * i + 1] : z;
                level[i] = keccak256(abi.encodePacked(bytes1(0x01), l, r));
            }
            n = m;
            z = keccak256(abi.encodePacked(bytes1(0x01), z, z));
        }
        return level[0];
    }

    function proof(bytes32[] memory leaves, uint256 index) internal pure returns (bytes32[] memory sib) {
        bytes32[] memory level = _copy(leaves);
        uint256 n = level.length;
        sib = new bytes32[](DEPTH);
        bytes32 z;
        uint256 at = index;
        for (uint256 d; d < DEPTH; ++d) {
            uint256 s = at ^ 1;
            sib[d] = s < n ? level[s] : z;
            uint256 m = (n + 1) / 2;
            for (uint256 i; i < m; ++i) {
                bytes32 l = level[2 * i];
                bytes32 r = 2 * i + 1 < n ? level[2 * i + 1] : z;
                level[i] = keccak256(abi.encodePacked(bytes1(0x01), l, r));
            }
            n = m;
            at /= 2;
            z = keccak256(abi.encodePacked(bytes1(0x01), z, z));
        }
    }

    function _copy(bytes32[] memory a) private pure returns (bytes32[] memory b) {
        b = new bytes32[](a.length);
        for (uint256 i; i < a.length; ++i) b[i] = a[i];
    }
}

abstract contract Base {
    Vm internal constant vm = Vm(address(uint160(uint256(keccak256("hevm cheat code")))));
    address internal constant FEE = 0x775a993F09Bd80aeCF8F8c2ADe0C84158E3a0544;
    uint256 internal constant N = 7;
    uint256 internal constant K = 5;
    uint64 internal constant USDC = 1e6;

    MockUSDC internal usdc;
    PerpsVault internal vault;
    uint256[] internal vkeys; // validator keys sorted by address
    address[] internal vaddrs;
    address[] internal gaddrs;

    function _sortedKeys(uint256 seed, uint256 n) internal pure returns (uint256[] memory keys, address[] memory addrs) {
        keys = new uint256[](n);
        addrs = new address[](n);
        for (uint256 i; i < n; ++i) {
            keys[i] = uint256(keccak256(abi.encode("throwaway-validator", seed, i))) % (2 ** 250) + 1;
            addrs[i] = vm.addr(keys[i]);
        }
        for (uint256 i; i < n; ++i) {
            for (uint256 j = i + 1; j < n; ++j) {
                if (addrs[j] < addrs[i]) {
                    (addrs[i], addrs[j]) = (addrs[j], addrs[i]);
                    (keys[i], keys[j]) = (keys[j], keys[i]);
                }
            }
        }
    }

    function _config() internal view returns (PerpsVault.Config memory c) {
        c.token = address(usdc);
        c.feeRecipient = FEE;
        c.validators = vaddrs;
        c.threshold = uint8(K);
        c.unpauseThreshold = 6;
        c.cap = 10_000_000 * USDC;
        c.minDeposit = 10 * USDC;
        c.windowLimit = 1_000_000 * USDC;
        c.windowSeconds = 1 days;
        c.largeThreshold = 100_000 * USDC;
        c.largeDelay = 1 days;
        c.forcedDelay = 2 days;
        c.escapeDelay = 7 days;
        c.guardians = gaddrs;
        c.pauseDuration = 2 days;
        c.pauseCooldown = 2 days;
    }

    function _setUpVault() internal {
        vm.warp(1_800_000_000);
        usdc = new MockUSDC();
        (uint256[] memory k, address[] memory a) = _sortedKeys(1, N);
        delete vkeys;
        delete vaddrs;
        for (uint256 i; i < N; ++i) {
            vkeys.push(k[i]);
            vaddrs.push(a[i]);
        }
        (, address[] memory g) = _sortedKeys(2, 3);
        delete gaddrs;
        for (uint256 i; i < 3; ++i) gaddrs.push(g[i]);
        vault = new PerpsVault(_config());
    }

    function _sign(bytes32 digest, uint256 count) internal view returns (bytes[] memory sigs) {
        sigs = new bytes[](count);
        for (uint256 i; i < count; ++i) {
            (uint8 v, bytes32 r, bytes32 s) = vm.sign(vkeys[i], digest);
            sigs[i] = abi.encodePacked(r, s, v);
        }
    }

    function _deposit(address from, address account, uint64 amount) internal returns (uint64 index) {
        usdc.mint(from, amount);
        vm.startPrank(from);
        usdc.approve(address(vault), amount);
        index = vault.deposit(amount, account);
        vm.stopPrank();
    }

    address internal constant PROTOCOL_FEE = address(1);

    function _w(uint64 id, address account, address recipient, uint64 amount)
        internal
        pure
        returns (PerpsVault.Withdrawal memory)
    {
        return PerpsVault.Withdrawal(id, account, recipient, amount, 1000 + id);
    }

    /// A checkpoint that credits every deposit and forced request so far, with the given trees.
    function _cp(uint64 height, PerpsVault.Withdrawal[] memory ws, address[] memory accts, uint64[] memory bals)
        internal
        view
        returns (PerpsVault.Checkpoint memory cp)
    {
        cp.height = height;
        cp.time = uint64(block.timestamp) * 1000 + height;
        cp.logHash = keccak256(abi.encode("log", height));
        cp.marketsRoot = keccak256(abi.encode("markets", height));
        cp.nextOrderId = height * 10;
        cp.dust = 7;
        cp.auxHash = keccak256(abi.encode("aux", height));
        cp.nextDepositIndex = vault.depositCount();
        cp.depositsHead = vault.depositHead(cp.nextDepositIndex);
        cp.nextForcedIndex = vault.forcedCount();
        cp.forcedHead = vault.forcedHead(cp.nextForcedIndex);
        cp.withdrawalsRoot = Tree.root(_wLeaves(ws));
        cp.withdrawalCount = uint64(ws.length);
        for (uint256 i; i < ws.length; ++i) cp.withdrawalsTotal += ws[i].amount;
        cp.accountsRoot = Tree.root(_eLeaves(accts, bals));
        for (uint256 i; i < bals.length; ++i) cp.totalEscape += bals[i];
    }

    function _wLeaf(PerpsVault.Withdrawal memory w) internal pure returns (bytes32) {
        return keccak256(
            abi.encodePacked(bytes1(0x00), "PPv1wdrl", w.withdrawalId, w.account, w.recipient, w.amount, w.height)
        );
    }

    function _wLeaves(PerpsVault.Withdrawal[] memory ws) internal pure returns (bytes32[] memory h) {
        h = new bytes32[](ws.length);
        for (uint256 i; i < ws.length; ++i) h[i] = _wLeaf(ws[i]);
    }

    /// SPEC 8: the account tree starts with PROTOCOL_FEE, INSURANCE, BACKSTOP at indices 0, 1, 2. A list that does not
    /// start with 0x..01 gets the three prepended with escape value 0; user index i then sits at position i + 3.
    function _full(address[] memory accts, uint64[] memory bals)
        internal
        pure
        returns (address[] memory A, uint64[] memory B, uint256 off)
    {
        if (accts.length != 0 && accts[0] == address(1)) return (accts, bals, 0);
        A = new address[](accts.length + 3);
        B = new uint64[](accts.length + 3);
        A[0] = address(1);
        A[1] = address(2);
        A[2] = address(3);
        for (uint256 i; i < accts.length; ++i) {
            A[i + 3] = accts[i];
            B[i + 3] = bals[i];
        }
        off = 3;
    }

    function _acct(address[] memory accts, uint64[] memory bals, uint256 i)
        internal
        pure
        returns (PerpsVault.Account memory)
    {
        (address[] memory A, uint64[] memory B, uint256 off) = _full(accts, bals);
        return _acctAt(A, B, i + off);
    }

    function _proveSys(address[] memory accts, uint64[] memory bals) internal {
        (address[] memory A, uint64[] memory B,) = _full(accts, bals);
        vault.proveSystemShares(
            _acctAt(A, B, 1), Tree.proof(_leavesAt(A, B), 1), _acctAt(A, B, 2), Tree.proof(_leavesAt(A, B), 2)
        );
    }

    function _acctAt(address[] memory accts, uint64[] memory bals, uint256 i)
        internal
        pure
        returns (PerpsVault.Account memory a)
    {
        a.account = accts[i];
        a.index = uint32(i);
        a.balance = int128(int64(bals[i])) - 5; // a balance unequal to the escape value (unrealized PnL)
        a.escapeValue = bals[i];
        a.positionsHash = keccak256(abi.encode("positions", accts[i]));
    }

    function _aLeaf(PerpsVault.Account memory a) internal pure returns (bytes32) {
        return keccak256(
            abi.encodePacked(
                bytes1(0x00), "PPv1acct", a.account, a.index, a.balance, a.escapeValue, a.positionsHash
            )
        );
    }

    function _leavesAt(address[] memory accts, uint64[] memory bals) internal pure returns (bytes32[] memory h) {
        h = new bytes32[](accts.length);
        for (uint256 i; i < accts.length; ++i) h[i] = _aLeaf(_acctAt(accts, bals, i));
    }

    function _eLeaves(address[] memory accts, uint64[] memory bals) internal pure returns (bytes32[] memory) {
        (address[] memory A, uint64[] memory B,) = _full(accts, bals);
        return _leavesAt(A, B);
    }

    function _submit(PerpsVault.Checkpoint memory cp) internal {
        vault.submitCheckpoint(cp, _sign(vault.checkpointDigest(cp), K));
    }

    function _wProof(PerpsVault.Withdrawal[] memory ws, uint64 i) internal pure returns (bytes32[] memory) {
        return Tree.proof(_wLeaves(ws), i);
    }

    function _eProof(address[] memory accts, uint64[] memory bals, uint64 i) internal pure returns (bytes32[] memory) {
        (,, uint256 off) = _full(accts, bals);
        return Tree.proof(_eLeaves(accts, bals), i + off);
    }

    // ------------------------------------------------------------------ assertions
    function assertEq(uint256 a, uint256 b, string memory m) internal pure {
        if (a != b) revert(string.concat(m, ": ", _u(a), " != ", _u(b)));
    }

    function assertEq(bytes32 a, bytes32 b, string memory m) internal pure {
        if (a != b) revert(m);
    }

    function assertTrue(bool c, string memory m) internal pure {
        if (!c) revert(m);
    }

    function _u(uint256 v) internal pure returns (string memory) {
        if (v == 0) return "0";
        bytes memory b;
        while (v != 0) {
            b = abi.encodePacked(bytes1(uint8(48 + v % 10)), b);
            v /= 10;
        }
        return string(b);
    }

    function _empty() internal pure returns (PerpsVault.Withdrawal[] memory w, address[] memory a, uint64[] memory b) {}
}

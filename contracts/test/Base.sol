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

/// Merkle sum tree builder (test side; the engine builds the same tree in Rust).
library SumTree {
    function node(bytes32 lh, uint64 ls, bytes32 rh, uint64 rs) internal pure returns (bytes32) {
        return keccak256(abi.encodePacked(bytes1(0x01), lh, ls, rh, rs));
    }

    /// Pads to a power of two with empty leaves (0, 0); returns root and total.
    function root(bytes32[] memory hashes, uint64[] memory sums) internal pure returns (bytes32, uint64) {
        (bytes32[] memory h, uint64[] memory s) = _pad(hashes, sums);
        uint256 n = h.length;
        while (n > 1) {
            for (uint256 i; i < n / 2; ++i) {
                h[i] = node(h[2 * i], s[2 * i], h[2 * i + 1], s[2 * i + 1]);
                s[i] = s[2 * i] + s[2 * i + 1];
            }
            n /= 2;
        }
        return (h[0], s[0]);
    }

    function proof(bytes32[] memory hashes, uint64[] memory sums, uint64 index)
        internal
        pure
        returns (PerpsVault.SumProof memory p)
    {
        (bytes32[] memory h, uint64[] memory s) = _pad(hashes, sums);
        uint256 n = h.length;
        uint256 depth;
        while ((uint256(1) << depth) < n) ++depth;
        p.index = index;
        p.hashes = new bytes32[](depth);
        p.sums = new uint64[](depth);
        uint256 at = index;
        for (uint256 d; d < depth; ++d) {
            uint256 sib = at ^ 1;
            p.hashes[d] = h[sib];
            p.sums[d] = s[sib];
            for (uint256 i; i < n / 2; ++i) {
                h[i] = node(h[2 * i], s[2 * i], h[2 * i + 1], s[2 * i + 1]);
                s[i] = s[2 * i] + s[2 * i + 1];
            }
            n /= 2;
            at /= 2;
        }
    }

    function _pad(bytes32[] memory hashes, uint64[] memory sums)
        private
        pure
        returns (bytes32[] memory h, uint64[] memory s)
    {
        uint256 n = 1;
        while (n < hashes.length) n *= 2;
        h = new bytes32[](n);
        s = new uint64[](n);
        for (uint256 i; i < hashes.length; ++i) {
            h[i] = hashes[i];
            s[i] = sums[i];
        }
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
        c.forcedDelay = 1 days;
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

    /// A checkpoint that credits every deposit and forced request so far, with the given trees.
    function _cp(uint64 height, PerpsVault.Withdrawal[] memory ws, address[] memory accts, uint64[] memory bals)
        internal
        view
        returns (PerpsVault.Checkpoint memory cp)
    {
        cp.height = height;
        cp.time = uint64(block.timestamp) * 1000 + height;
        cp.stateRoot = keccak256(abi.encode("state", height));
        cp.logHash = keccak256(abi.encode("log", height));
        cp.depositsCredited = vault.depositCount();
        cp.depositsHead = vault.depositHead(cp.depositsCredited);
        cp.forcedProcessed = vault.forcedCount();
        cp.forcedHead = vault.forcedHead(cp.forcedProcessed);
        if (ws.length != 0) {
            (bytes32[] memory h, uint64[] memory s) = _wLeaves(ws);
            (cp.withdrawalsRoot, cp.withdrawalsTotal) = SumTree.root(h, s);
            cp.withdrawalCount = uint64(ws.length);
        }
        if (accts.length != 0) {
            (bytes32[] memory h, uint64[] memory s) = _eLeaves(accts, bals);
            (cp.escapeRoot, cp.escapeTotal) = SumTree.root(h, s);
        }
    }

    function _wLeaves(PerpsVault.Withdrawal[] memory ws)
        internal
        pure
        returns (bytes32[] memory h, uint64[] memory s)
    {
        h = new bytes32[](ws.length);
        s = new uint64[](ws.length);
        for (uint256 i; i < ws.length; ++i) {
            h[i] = keccak256(
                abi.encode(
                    bytes32("PosternPerps.withdrawal.v1"),
                    uint256(ws[i].index),
                    ws[i].account,
                    ws[i].recipient,
                    uint256(ws[i].amount)
                )
            );
            s[i] = ws[i].amount;
        }
    }

    function _eLeaves(address[] memory accts, uint64[] memory bals)
        internal
        pure
        returns (bytes32[] memory h, uint64[] memory s)
    {
        h = new bytes32[](accts.length);
        s = new uint64[](accts.length);
        for (uint256 i; i < accts.length; ++i) {
            h[i] = keccak256(abi.encode(bytes32("PosternPerps.escape.v1"), accts[i], uint256(bals[i])));
            s[i] = bals[i];
        }
    }

    function _submit(PerpsVault.Checkpoint memory cp) internal {
        vault.submitCheckpoint(cp, _sign(vault.checkpointDigest(cp), K));
    }

    function _wProof(PerpsVault.Withdrawal[] memory ws, uint64 i) internal pure returns (PerpsVault.SumProof memory) {
        (bytes32[] memory h, uint64[] memory s) = _wLeaves(ws);
        return SumTree.proof(h, s, i);
    }

    function _eProof(address[] memory accts, uint64[] memory bals, uint64 i)
        internal
        pure
        returns (PerpsVault.SumProof memory)
    {
        (bytes32[] memory h, uint64[] memory s) = _eLeaves(accts, bals);
        return SumTree.proof(h, s, i);
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

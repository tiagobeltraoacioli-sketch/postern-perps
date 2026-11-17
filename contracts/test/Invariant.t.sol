// SPDX-License-Identifier: UNLICENSED
pragma solidity 0.8.24;

import {PerpsVault} from "../src/PerpsVault.sol";
import {Base, MockUSDC, Tree} from "./Base.sol";

/// An honest engine model (zero-sum trades, fees to the fee account, withdrawal records) plus a colluding committee
/// that signs arbitrary escape totals, driven against one vault. The invariants must hold in both cases: the vault's
/// own accounting never promises more than it holds, whatever the committee signs.
contract Handler is Base {
    address[] internal accts;
    mapping(address => uint64) internal bal; // engine-side balance (escape balance)
    uint64 internal insurance; // engine-held, outside the escape tree
    PerpsVault.Withdrawal[] internal records;
    uint64 internal credited; // deposits the model has credited
    uint64 internal height;
    bool public colluded;

    bytes32[] public keys;
    mapping(bytes32 => bool) internal seen;
    uint256 public paidOut;

    constructor(PerpsVault v, MockUSDC t, uint256[] memory k, address[] memory va, address[] memory ga) {
        vault = v;
        usdc = t;
        for (uint256 i; i < k.length; ++i) {
            vkeys.push(k[i]);
            vaddrs.push(va[i]);
        }
        for (uint256 i; i < ga.length; ++i) gaddrs.push(ga[i]);
        accts.push(address(uint160(uint256(keccak256("user1")))));
        accts.push(address(uint160(uint256(keccak256("user2")))));
        accts.push(address(uint160(uint256(keccak256("user3")))));
        accts.push(PROTOCOL_FEE);
        accts.push(address(2)); // INSURANCE
    }

    function _a(uint256 i) internal view returns (address) {
        return accts[i % 3];
    }

    // ---------------------------------------------------------------- users
    function deposit(uint256 i, uint64 amount) external {
        amount = uint64(10 * USDC + amount % (200_000 * USDC));
        address a = _a(i);
        usdc.mint(a, amount);
        vm.startPrank(a);
        usdc.approve(address(vault), amount);
        try vault.deposit(amount, a) {} catch {}
        vm.stopPrank();
    }

    function forced(uint256 i, uint8 kind) external {
        vm.prank(_a(i));
        try vault.requestForced(uint8(kind % 2), 0) {} catch {}
    }

    // ---------------------------------------------------------------- engine (honest)
    function trade(uint256 i, uint256 j, uint64 amount) external {
        address a = _a(i);
        address b = _a(j);
        if (a == b) return;
        uint64 x = bal[a] == 0 ? 0 : amount % (bal[a] + 1);
        bal[a] -= x;
        // 2% fee to the fee account, 1% to insurance: still zero-sum with the engine's books
        uint64 fee = x / 50;
        uint64 ins = x / 100;
        bal[b] += x - fee - ins;
        bal[PROTOCOL_FEE] += fee;
        bal[address(2)] += ins;
        insurance += ins;
    }

    function requestWithdraw(uint256 i, uint64 amount, bool toFee) external {
        address a = toFee ? PROTOCOL_FEE : _a(i);
        if (bal[a] == 0) return;
        uint64 x = amount % bal[a] + 1;
        bal[a] -= x;
        records.push(PerpsVault.Withdrawal(uint64(records.length), a, toFee ? FEE : a, x, height));
    }

    function checkpoint() external {
        if (vault.inEscape()) return;
        // credit the vault's new deposits from its own events (read here from the chain)
        uint64 n = vault.depositCount();
        while (credited < n) {
            (address a, uint64 amt) = _depositOf(credited);
            bal[a] += amt;
            credited++;
        }
        vm.warp(block.timestamp + 1);
        height++;
        PerpsVault.Checkpoint memory cp = _honest();
        try vault.submitCheckpoint(cp, _sign(vault.checkpointDigest(cp), K)) {} catch {}
    }

    // ---------------------------------------------------------------- committee (colluding)
    function colludingCheckpoint(uint64 claimed) external {
        if (vault.inEscape()) return;
        vm.warp(block.timestamp + 1);
        height++;
        PerpsVault.Checkpoint memory cp = _honest();
        address[] memory one = new address[](1);
        uint64[] memory amt = new uint64[](1);
        one[0] = address(0xBAD);
        amt[0] = claimed % (vault.custody() + 2) + 1;
        cp.accountsRoot = Tree.root(_eLeaves(one, amt));
        cp.totalEscape = amt[0];
        try vault.submitCheckpoint(cp, _sign(vault.checkpointDigest(cp), K)) {
            colluded = true;
        } catch {}
    }

    // ---------------------------------------------------------------- anyone
    function withdraw(uint256 k) external {
        if (records.length == 0) return;
        uint64 seq = vault.checkpointCount();
        if (seq == 0) return;
        uint64 count = vault.withdrawalCountAt(seq);
        if (count == 0) return;
        uint64 idx = uint64(k % count);
        PerpsVault.Withdrawal[] memory ws = new PerpsVault.Withdrawal[](count);
        for (uint256 i; i < count; ++i) ws[i] = records[i];
        uint256 before = usdc.balanceOf(ws[idx].recipient);
        try vault.withdraw(seq, ws[idx], _wProof(ws, idx), type(uint64).max) returns (bytes32 key) {
            _track(key);
            paidOut += usdc.balanceOf(ws[idx].recipient) - before;
        } catch {}
    }

    function release(uint256 k) external {
        if (keys.length == 0) return;
        bytes32 key = keys[k % keys.length];
        (address r,,,) = vault.payouts(key);
        uint256 before = usdc.balanceOf(r);
        try vault.release(key, type(uint64).max) {
            paidOut += usdc.balanceOf(r) - before;
        } catch {}
    }

    function warp(uint32 dt) external {
        vm.warp(block.timestamp + dt % 3 days);
    }

    function pause(uint256 g) external {
        vm.prank(gaddrs[g % gaddrs.length]);
        try vault.guardianPause() {} catch {}
    }

    function escape() external {
        try vault.triggerEscape() {} catch {}
    }

    function claim(uint256 i) external {
        if (!vault.inEscape() || vault.checkpointCount() == 0 || colluded) return;
        // the tree of the latest accepted honest checkpoint is rebuilt from the model at that checkpoint; the model
        // only changes through `trade`/`requestWithdraw` between checkpoints, so use the snapshot taken at submission
        uint256 at = i % (snapA.length + 1);
        if (at == snapA.length) return;
        address a = snapA[at];
        address r = uint160(a) <= 0xffff ? FEE : a;
        uint256 before = usdc.balanceOf(r);
        try vault.claimEscape(_acct(snapA, snapBal, at), Tree.proof(_eLeaves(snapA, snapBal), at), type(uint64).max)
        returns (bytes32 key) {
            _track(key);
            paidOut += usdc.balanceOf(r) - before;
        } catch {}
    }

    function refund(uint64 index) external {
        if (!vault.inEscape()) return;
        uint64 n = vault.depositCount();
        if (n == 0) return;
        index = index % n;
        (address a, uint64 amt) = _depositOf(index);
        uint256 before = usdc.balanceOf(a);
        try vault.refundDeposit(index, a, amt) {
            paidOut += usdc.balanceOf(a) - before;
        } catch {}
    }

    function sweep() external {
        uint256 before = usdc.balanceOf(FEE);
        try vault.sweepEscapeSurplus() {
            paidOut += usdc.balanceOf(FEE) - before;
        } catch {}
    }

    // ---------------------------------------------------------------- helpers
    address[] internal snapA;
    uint64[] internal snapBal;

    function _honest() internal returns (PerpsVault.Checkpoint memory cp) {
        cp.height = height;
        cp.time = height * 1000;
        cp.logHash = keccak256(abi.encode("log", height));
        cp.marketsRoot = keccak256(abi.encode("markets", height));
        cp.nextDepositIndex = credited;
        cp.depositsHead = vault.depositHead(credited);
        cp.nextForcedIndex = vault.forcedCount();
        cp.forcedHead = vault.forcedHead(cp.nextForcedIndex);
        PerpsVault.Withdrawal[] memory ws = new PerpsVault.Withdrawal[](records.length);
        for (uint256 i; i < records.length; ++i) {
            ws[i] = records[i];
            cp.withdrawalsTotal += records[i].amount;
        }
        cp.withdrawalsRoot = Tree.root(_wLeaves(ws));
        cp.withdrawalCount = uint64(records.length);
        delete snapA;
        delete snapBal;
        for (uint256 i; i < accts.length; ++i) {
            address a = accts[i];
            if (bal[a] == 0) continue;
            snapA.push(a);
            snapBal.push(bal[a]);
            cp.totalEscape += bal[a];
        }
        cp.accountsRoot = Tree.root(_eLeaves(snapA, snapBal));
    }

    /// Deposits are recorded in this handler as they happen through `Deposit` replay: here read back by trying the
    /// chain link for each candidate account and the amount the cumulative totals give.
    function _depositOf(uint64 index) internal view returns (address a, uint64 amt) {
        amt = vault.depositTotalAt(index + 1) - vault.depositTotalAt(index);
        for (uint256 i; i < 3; ++i) {
            if (
                keccak256(abi.encode(vault.depositHead(index), uint256(index), accts[i], uint256(amt)))
                    == vault.depositHead(index + 1)
            ) {
                return (accts[i], amt);
            }
        }
        revert("unknown depositor");
    }

    function _track(bytes32 key) internal {
        if (!seen[key]) {
            seen[key] = true;
            keys.push(key);
        }
    }

    function keyCount() external view returns (uint256) {
        return keys.length;
    }
}

contract PerpsVaultInvariant is Base {
    Handler internal handler;

    function setUp() public {
        _setUpVault();
        handler = new Handler(vault, usdc, vkeys, vaddrs, gaddrs);
    }

    function targetContracts() public view returns (address[] memory t) {
        t = new address[](1);
        t[0] = address(handler);
    }

    /// The vault holds at least what it still owes: unregistered custody plus registered, unpaid payouts.
    function invariant_balance_covers_custody_and_reserved() public view {
        assertTrue(usdc.balanceOf(address(vault)) >= uint256(vault.custody()) + vault.reserved(), "balance");
    }

    /// Nothing leaves the vault beyond what was deposited (donations aside: the handler makes none).
    function invariant_paid_out_at_most_deposited() public view {
        assertTrue(handler.paidOut() <= vault.depositedTotal(), "paid <= deposited");
        assertEq(usdc.balanceOf(address(vault)) + handler.paidOut(), vault.depositedTotal(), "conservation");
    }

    /// Everything the latest checkpoint still makes claimable fits in custody (the solvency check's invariant).
    function invariant_latest_checkpoint_claims_fit_custody() public view {
        if (vault.checkpointCount() == 0) return;
        (,, uint64 credited,,, uint64 wTotal,,,) = vault.latest();
        uint256 owed = uint256(vault.escapePayable() - vault.escapeClaimedTotal()) + (wTotal - vault.withdrawnTotal())
            + (vault.depositedTotal() - vault.depositTotalAt(credited) - vault.refundedTotal());
        assertTrue(owed <= vault.custody(), "owed <= custody");
    }

    /// `reserved` is exactly the sum of what registered payouts still have to pay, and none paid more than its total.
    function invariant_reserved_matches_payouts() public view {
        uint256 sum;
        uint256 n = handler.keyCount();
        for (uint256 i; i < n; ++i) {
            (, uint64 total, uint64 paid,) = vault.payouts(handler.keys(i));
            assertTrue(paid <= total, "paid <= total");
            sum += total - paid;
        }
        assertEq(sum, vault.reserved(), "reserved");
    }

    /// The bucket never exceeds the window limit.
    function invariant_bucket_bounded() public view {
        assertTrue(vault.availableToRelease() <= vault.windowLimit(), "bucket");
    }
}

/// End to end, seed-driven: an honest history of deposits, trades, withdrawals and checkpoints, then escape. Every
/// account receives exactly its escape balance, every certified record and every uncredited deposit is paid, and the
/// vault ends with exactly the engine's insurance fund (swept to the fee address after the delay).
contract PerpsVaultEscapeEndToEnd is Base {
    function setUp() public {
        _setUpVault();
    }

    function testFuzz_escape_pays_everyone_exactly(uint256 seed) public {
        Handler h = new Handler(vault, usdc, vkeys, vaddrs, gaddrs);
        for (uint256 step; step < 40; ++step) {
            uint256 r = uint256(keccak256(abi.encode(seed, step)));
            uint256 op = r % 6;
            if (op == 0) h.deposit(r >> 8, uint64(r >> 64));
            else if (op == 1) h.trade(r >> 8, r >> 16, uint64(r >> 64));
            else if (op == 2) h.requestWithdraw(r >> 8, uint64(r >> 64), (r >> 32) % 5 == 0);
            else if (op == 3) h.checkpoint();
            else if (op == 4) h.withdraw(r >> 8);
            else h.warp(uint32(r >> 8) % 1 hours);
        }
        h.checkpoint();
        uint64 before = vault.depositedTotal();
        h.deposit(seed, uint64(seed >> 64)); // after the last checkpoint: refunded in escape
        uint64 late = vault.depositedTotal() - before;
        assertTrue(late >= 10 * USDC, "late deposit made");
        vm.warp(block.timestamp + 7 days);
        vault.triggerEscape();
        for (uint256 i; i < 6; ++i) h.claim(i);
        uint64 n = vault.depositCount();
        for (uint64 i; i < n; ++i) h.refund(i);
        uint64 wc = vault.withdrawalCountAt(vault.checkpointCount());
        for (uint256 i; i < wc; ++i) h.withdraw(i);
        // large payouts and bucket parts
        for (uint256 round; round < 12; ++round) {
            vm.warp(block.timestamp + 1 days);
            for (uint256 k; k < h.keyCount(); ++k) h.release(k);
        }
        // keyless INSURANCE's share is payable to the fee address only after the surplus delay
        vm.warp(block.timestamp + 90 days);
        for (uint256 k; k < h.keyCount(); ++k) h.release(k);
        assertEq(vault.reserved(), 0, "all registered payouts paid");
        (,,,,, uint64 wTotal, uint64 eTotal,,) = vault.latest();
        assertEq(vault.escapePayable(), eTotal, "honest engine: the pool covers every escape value");
        assertEq(vault.escapeClaimedTotal(), eTotal, "every escape balance claimed");
        assertEq(vault.withdrawnTotal(), wTotal, "every certified record paid");
        assertEq(vault.refundedTotal(), late, "the uncredited deposit refunded");
        assertTrue(eTotal + wTotal > 0 || before == 0, "non-vacuous: something was claimable");
        uint256 left = usdc.balanceOf(address(vault));
        if (left != 0) {
            vault.sweepEscapeSurplus();
            assertEq(usdc.balanceOf(address(vault)), 0, "only rounding was left");
        }
        assertEq(h.paidOut() + left, vault.depositedTotal(), "conservation");
    }
}

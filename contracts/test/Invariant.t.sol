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

    // Round 2 ghosts. `doublePay`: a second registration, refund, proof or sweep of the same item succeeded.
    // `pauseLeak`: a deposit or a payment went through during a pause. `pauseOverreach`: a pause stopped something
    // VAULT.md section 6 says it never stops. The counters show the probes ran (non-vacuous).
    address public funder = address(uint160(uint256(keccak256("system funder"))));
    bool public doublePay;
    bool public pauseLeak;
    bool public pauseOverreach;
    uint256 public repeatProbes;
    uint256 public pauseProbes;
    uint256 public systemDeposits;

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

    /// Round 2 (vault v0.3): the insurance fund and the backstop are funded through the vault by a third party.
    function fundSystem(bool backstop, uint64 amount) external {
        amount = uint64(10 * USDC + amount % (100_000 * USDC));
        usdc.mint(funder, amount);
        vm.startPrank(funder);
        usdc.approve(address(vault), amount);
        try vault.deposit(amount, backstop ? address(3) : address(2)) {
            systemDeposits++;
        } catch {
            usdc.transfer(address(0xdead), amount); // keep the funder's balance equal to what the vault paid it
        }
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
            // no path pays twice: the same record, against this or any other accepted checkpoint, never registers again
            repeatProbes++;
            for (uint64 q = 1; q <= seq; ++q) {
                if (vault.withdrawalCountAt(q) <= idx) continue;
                PerpsVault.Withdrawal[] memory wq = new PerpsVault.Withdrawal[](vault.withdrawalCountAt(q));
                for (uint256 i; i < wq.length; ++i) wq[i] = records[i];
                try vault.withdraw(q, ws[idx], _wProof(wq, idx), type(uint64).max) {
                    doublePay = true;
                } catch {}
            }
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
        if (!vault.systemSharesProven()) {
            try vault.proveSystemShares(
                _acct(snapA, snapBal, 1),
                Tree.proof(_eLeaves(snapA, snapBal), 1),
                _acct(snapA, snapBal, 2),
                Tree.proof(_eLeaves(snapA, snapBal), 2)
            ) {
                (, uint64 t,,) = vault.payouts(vault.escapeKey(address(2)));
                if (t != 0) _track(vault.escapeKey(address(2)));
                repeatProbes++;
                try vault.proveSystemShares(
                    _acct(snapA, snapBal, 1),
                    Tree.proof(_eLeaves(snapA, snapBal), 1),
                    _acct(snapA, snapBal, 2),
                    Tree.proof(_eLeaves(snapA, snapBal), 2)
                ) {
                    doublePay = true;
                } catch {}
            } catch {}
        }
        uint256 at = i % (snapA.length + 1);
        if (at == snapA.length || at == 1 || at == 2) return;
        address a = snapA[at];
        address r = uint160(a) <= 0xffff ? FEE : a;
        uint256 before = usdc.balanceOf(r);
        try vault.claimEscape(_acct(snapA, snapBal, at), Tree.proof(_eLeaves(snapA, snapBal), at), type(uint64).max)
        returns (bytes32 key) {
            _track(key);
            paidOut += usdc.balanceOf(r) - before;
            repeatProbes++;
            try vault.claimEscape(_acct(snapA, snapBal, at), Tree.proof(_eLeaves(snapA, snapBal), at), type(uint64).max) {
                doublePay = true;
            } catch {}
        } catch {}
    }

    function refund(uint64 index) external {
        if (!vault.inEscape()) return;
        uint64 n = vault.depositCount();
        if (n == 0) return;
        index = index % n;
        (address a, uint64 amt) = _depositOf(index);
        // an uncredited system deposit is refunded to its funder, never to the keyless account
        address to = uint160(a) <= 0xffff ? funder : a;
        uint256 before = usdc.balanceOf(to);
        try vault.refundDeposit(index, a, amt) {
            paidOut += usdc.balanceOf(to) - before;
            repeatProbes++;
            try vault.refundDeposit(index, a, amt) {
                doublePay = true;
            } catch {}
        } catch {}
    }

    function sweep() external {
        uint256 before = usdc.balanceOf(FEE);
        try vault.sweepEscapeSurplus() {
            paidOut += usdc.balanceOf(FEE) - before;
            repeatProbes++;
            try vault.sweepEscapeSurplus() {
                doublePay = true;
            } catch {}
        } catch {}
    }

    /// Round 2: what a guardian pause stops and what it never stops (VAULT.md section 6, last rule). Pauses if a
    /// guardian can, then probes every entry point; each probe that changes state is a real action of the run.
    function pauseProbe(uint256 g, uint256 i, uint64 amount) external {
        vm.prank(gaddrs[g % gaddrs.length]);
        try vault.guardianPause() {} catch {}
        if (!vault.paused()) return;
        pauseProbes++;
        bytes4 pausedSel = PerpsVault.Paused.selector;
        address a = _a(i);
        bool esc = vault.inEscape();
        // (1) stopped: deposits (in escape they are stopped by escape itself)
        amount = uint64(10 * USDC + amount % (1000 * USDC));
        usdc.mint(a, amount);
        vm.startPrank(a);
        usdc.approve(address(vault), amount);
        try vault.deposit(amount, a) {
            pauseLeak = true;
        } catch (bytes memory err) {
            if (!esc && bytes4(err) != pausedSel) pauseOverreach = true; // must say why
            usdc.transfer(address(0xdead), amount);
        }
        vm.stopPrank();
        // (2) stopped: every payment of a registered payout
        for (uint256 k; k < keys.length; ++k) {
            (address r, uint64 total, uint64 paid, uint64 readyAt) = vault.payouts(keys[k]);
            if (total == paid) continue;
            uint256 before = usdc.balanceOf(r);
            try vault.release(keys[k], type(uint64).max) {
                pauseLeak = true;
            } catch (bytes memory err) {
                if (block.timestamp >= readyAt && bytes4(err) != pausedSel) pauseOverreach = true;
            }
            if (usdc.balanceOf(r) != before) pauseLeak = true;
        }
        // (3) never stopped: forced requests, checkpoints, the escape trigger
        vm.prank(a);
        try vault.requestForced(0, 0) {} catch (bytes memory err) {
            if (bytes4(err) == pausedSel) pauseOverreach = true;
        }
        if (!esc) {
            uint64 n = vault.depositCount();
            while (credited < n) {
                (address d, uint64 amt) = _depositOf(credited);
                bal[d] += amt;
                credited++;
            }
            vm.warp(block.timestamp + 1);
            height++;
            PerpsVault.Checkpoint memory cp = _honest();
            try vault.submitCheckpoint(cp, _sign(vault.checkpointDigest(cp), K)) {} catch (bytes memory err) {
                if (bytes4(err) == pausedSel) pauseOverreach = true;
            }
        }
        try vault.triggerEscape() {} catch (bytes memory err) {
            if (bytes4(err) == pausedSel) pauseOverreach = true;
        }
        if (!vault.paused()) return; // the +1 s above may have ended the pause
        // (4) never stopped: registration (it registers and pays nothing while paused)
        uint64 seq = vault.checkpointCount();
        if (seq != 0 && !colluded) {
            uint64 count = vault.withdrawalCountAt(seq);
            PerpsVault.Withdrawal[] memory ws = new PerpsVault.Withdrawal[](count);
            for (uint256 k; k < count; ++k) ws[k] = records[k];
            for (uint64 idx; idx < count; ++idx) {
                (, uint64 total,,) = vault.payouts(vault.withdrawalKey(idx));
                if (total != 0) continue;
                uint256 before = usdc.balanceOf(ws[idx].recipient);
                try vault.withdraw(seq, ws[idx], _wProof(ws, idx), type(uint64).max) returns (bytes32 key) {
                    _track(key);
                    if (usdc.balanceOf(ws[idx].recipient) != before) pauseLeak = true;
                } catch (bytes memory err) {
                    if (bytes4(err) == pausedSel) pauseOverreach = true;
                }
                break;
            }
        }
        // (5) never stopped: refunds in escape (no committee input)
        if (vault.inEscape()) {
            uint64 n = vault.depositCount();
            (,, uint64 next,,,,,,) = vault.latest();
            for (uint64 idx = next; idx < n; ++idx) {
                if (vault.refunded(idx)) continue;
                (address d, uint64 amt) = _depositOf(idx);
                address to = uint160(d) <= 0xffff ? funder : d;
                uint256 before = usdc.balanceOf(to);
                try vault.refundDeposit(idx, d, amt) {
                    paidOut += usdc.balanceOf(to) - before;
                } catch {
                    pauseOverreach = true;
                }
                break;
            }
        }
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
        // SPEC 8 order: PROTOCOL_FEE, INSURANCE, BACKSTOP at 0, 1, 2 (always present), then the users
        snapA.push(PROTOCOL_FEE);
        snapBal.push(bal[PROTOCOL_FEE]);
        snapA.push(address(2));
        snapBal.push(bal[address(2)]);
        snapA.push(address(3));
        snapBal.push(bal[address(3)]); // 0 unless the backstop was funded through the vault (round 2)
        for (uint256 i; i < 3; ++i) {
            address a = accts[i];
            if (bal[a] == 0) continue;
            snapA.push(a);
            snapBal.push(bal[a]);
        }
        for (uint256 i; i < snapBal.length; ++i) cp.totalEscape += snapBal[i];
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
        for (uint160 k = 2; k < 4; ++k) {
            if (
                keccak256(abi.encode(vault.depositHead(index), uint256(index), address(k), uint256(amt)))
                    == vault.depositHead(index + 1)
            ) {
                return (address(k), amt);
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

    // ------------------------------------------------------------------ round 2 (agent 433, task 4)

    /// The vault's token balance covers the sum of EVERY unpaid obligation in every reachable state, honest or
    /// colluding committee: registered and unpaid payouts, certified withdrawal records not yet registered, deposits
    /// the latest checkpoint did not credit and that were not refunded, and in escape the whole escape pool (before
    /// the system shares are proven) or the users' unclaimed part of it (after; the system remainder is then a
    /// registered payout and counted in `reserved`).
    function invariant_balance_covers_every_unpaid_obligation() public view {
        (,, uint64 credited,,, uint64 wTotal,,,) = vault.latest();
        uint256 owed = uint256(vault.reserved()) + (wTotal - vault.withdrawnTotal())
            + (vault.depositedTotal() - vault.depositTotalAt(credited) - vault.refundedTotal());
        if (vault.inEscape()) {
            owed += vault.systemSharesProven()
                ? uint256(vault.escapePayable() - vault.escapeClaimedTotal())
                : uint256(vault.escapePool());
        } else {
            // outside escape the uncredited term has no refunds in it
            assertEq(vault.refundedTotal(), 0, "no refund before escape");
            assertEq(vault.escapeClaimedTotal(), 0, "no escape claim before escape");
        }
        assertTrue(usdc.balanceOf(address(vault)) >= owed, "balance >= every unpaid obligation");
    }

    /// No path pays twice: no item registered, refunded, proven or swept a second time (probed after every success),
    /// each total within what the committee certified or the vault's own chain holds, and every honest record's
    /// payout equal to the record.
    function invariant_no_path_pays_twice() public view {
        assertTrue(!handler.doublePay(), "an item was paid or registered twice");
        (,, uint64 credited,,, uint64 wTotal,,,) = vault.latest();
        assertTrue(vault.withdrawnTotal() <= wTotal, "registered records <= certified total");
        assertTrue(vault.escapeClaimedTotal() <= vault.escapePayable(), "escape claims <= payable");
        assertTrue(
            vault.refundedTotal() <= vault.depositedTotal() - vault.depositTotalAt(credited), "refunds <= uncredited"
        );
        assertTrue(
            uint256(vault.committedOut()) <= vault.depositedTotal(), "committed out <= deposited (no donation here)"
        );
    }

    /// A pause stops deposits and payments, and nothing else (VAULT.md section 6): never a checkpoint, a forced
    /// request, the escape trigger, a registration or a refund.
    function invariant_pause_blocks_only_deposits_and_payments() public view {
        assertTrue(!handler.pauseLeak(), "a deposit or a payment went through a pause");
        assertTrue(!handler.pauseOverreach(), "a pause stopped something it must never stop");
    }
}

/// The round-2 probes are not vacuous: a fixed sequence reaches each of them (so the invariants above are checked
/// against real second attempts and real paused states, not against flags nothing ever sets).
contract PerpsVaultInvariantProbes is Base {
    function setUp() public {
        _setUpVault();
    }

    function test_round2_probes_run() public {
        Handler h = new Handler(vault, usdc, vkeys, vaddrs, gaddrs);
        h.deposit(0, 5_000 * USDC);
        h.deposit(1, 7_000 * USDC);
        h.fundSystem(false, 400 * USDC);
        h.fundSystem(true, 300 * USDC);
        assertEq(h.systemDeposits(), 2, "system deposits made");
        h.checkpoint();
        h.trade(0, 1, 1_000 * USDC);
        h.requestWithdraw(1, 500 * USDC, false);
        h.requestWithdraw(0, 100 * USDC, false);
        h.checkpoint();
        h.withdraw(0);
        assertEq(h.repeatProbes(), 1, "second registration probed");
        h.pauseProbe(0, 2, 50 * USDC); // pauses, probes deposit / release / forced / checkpoint / registration
        assertEq(h.pauseProbes(), 1, "pause probed");
        assertTrue(vault.paused(), "paused");
        h.deposit(2, 20 * USDC); // refused by the pause; nothing recorded
        h.fundSystem(true, 20 * USDC);
        assertEq(h.systemDeposits(), 2, "no system deposit during a pause");
        vm.warp(block.timestamp + 2 days + 1);
        h.fundSystem(true, 20 * USDC); // after the last checkpoint: refundable to the funder in escape
        h.deposit(2, 20 * USDC);
        vm.warp(block.timestamp + 7 days);
        h.escape();
        assertTrue(vault.inEscape(), "escape");
        h.pauseProbe(1, 0, 0); // a pause in escape: refunds still paid
        assertEq(h.pauseProbes(), 2, "pause probed in escape");
        for (uint256 i; i < 8; ++i) h.claim(i);
        for (uint64 i; i < vault.depositCount(); ++i) h.refund(i);
        h.withdraw(1);
        vm.warp(block.timestamp + 91 days);
        for (uint256 k; k < h.keyCount(); ++k) h.release(k);
        h.sweep();
        assertTrue(h.repeatProbes() >= 5, "registration, proof, claim, refund probed twice");
        assertTrue(!h.doublePay() && !h.pauseLeak() && !h.pauseOverreach(), "no flag");
        assertEq(usdc.balanceOf(h.funder()), 30 * USDC, "funder got its uncredited system deposit back (10 + 20 USDC)");
        assertEq(usdc.balanceOf(address(vault)) + h.paidOut(), vault.depositedTotal(), "conservation");
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
        for (uint256 i; i < 8; ++i) h.claim(i);
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
        (,,,,, uint64 wTotal, uint128 eTotal,,) = vault.latest();
        assertEq(
            vault.escapePayable(), eTotal - vault.subordinatedTotal(), "honest engine: the pool covers every user value"
        );
        assertEq(vault.escapeClaimedTotal(), vault.escapePayable(), "every user escape value claimed");
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

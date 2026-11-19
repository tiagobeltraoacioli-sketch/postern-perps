// SPDX-License-Identifier: UNLICENSED
// Copyright (c) 2026 Bloch Inc.
pragma solidity 0.8.24;

import {PerpsVault} from "../src/PerpsVault.sol";
import {Base, Tree} from "./Base.sol";

interface EmitVm {
    function expectEmit(bool t1, bool t2, bool t3, bool data, address emitter) external;
}

/// Findings of agent 433 (round 2), each fixed failing-test-first.
contract R433Test is Base {
    event PayoutRegistered(bytes32 indexed key, address indexed recipient, uint64 total, uint64 readyAt);

    address internal alice = address(uint160(uint256(keccak256("r433 alice"))));
    address[] internal a;
    uint64[] internal b;

    function setUp() public {
        _setUpVault();
    }

    /// INSURANCE 80,000 and BACKSTOP 30,000 funded (together above the test vault's largeThreshold of 100,000), alice
    /// 1,000; one checkpoint; escape by silence.
    function _escape() internal {
        _deposit(alice, alice, 1_000 * USDC);
        _deposit(address(this), address(2), 80_000 * USDC);
        _deposit(address(this), address(3), 30_000 * USDC);
        a = new address[](4);
        b = new uint64[](4);
        a[0] = address(1);
        a[1] = address(2);
        a[2] = address(3);
        a[3] = alice;
        b[1] = 80_000 * USDC;
        b[2] = 30_000 * USDC;
        b[3] = 1_000 * USDC;
        (PerpsVault.Withdrawal[] memory w,,) = _empty();
        _submit(_cp(1, w, a, b));
        vm.warp(block.timestamp + 7 days);
        vault.triggerEscape();
    }

    /// R433-1 (Low; PerpsVault.sol `proveSystemShares`, f0e3967 :729-730). The system remainder was registered
    /// through `_register`, which emits `PayoutRegistered(.., readyAt = now [+ largeDelay])`, and only then was the
    /// stored `readyAt` overwritten with `escapeAt + 90 days`: the event told every indexer and screen that 110,000
    /// USDC to the fee address was payable in one day when the vault would refuse it for 90. Found by replaying the
    /// engine vectors on a trace. Failing first on 1c03ef0: the event's last word is escapeAt + 1 day + 0 s.
    function test_R433_1_system_remainder_event_states_the_stored_ready_time() public {
        _escape();
        uint64 ready = vault.escapeAt() + 90 days;
        EmitVm(address(vm)).expectEmit(true, true, true, true, address(vault));
        emit PayoutRegistered(vault.escapeKey(address(2)), FEE, 110_000 * USDC, ready);
        _proveSys(a, b);
        (,,, uint64 readyAt) = vault.payouts(vault.escapeKey(address(2)));
        assertEq(readyAt, ready, "stored ready time");
    }

    /// R433-1, second half: the overwrite also REPLACED the large-amount delay. A proof made shortly before the 90
    /// days ended left a payout far above `largeThreshold` payable at once, without the day guardians are given to
    /// pause any large payout (VAULT.md section 6). The ready time is now the later of the two. Failing first on
    /// 1c03ef0: the release one hour after the proof succeeds.
    function test_R433_1_late_proof_keeps_the_large_amount_delay() public {
        _escape();
        uint64 escapeAt = vault.escapeAt();
        vm.warp(uint256(escapeAt) + 90 days - 1 hours);
        _proveSys(a, b);
        bytes32 key = vault.escapeKey(address(2));
        vm.warp(uint256(escapeAt) + 90 days);
        vm.expectRevert(PerpsVault.NotReady.selector);
        vault.release(key, type(uint64).max);
        vm.warp(uint256(escapeAt) + 90 days + 1 days - 1 hours - 1);
        vm.expectRevert(PerpsVault.NotReady.selector);
        vault.release(key, type(uint64).max);
        vm.warp(block.timestamp + 1);
        vault.release(key, type(uint64).max);
        assertEq(usdc.balanceOf(FEE), 110_000 * USDC, "paid after the large delay");
    }

    /// R433-2 (Info; documentation). The rounding residue of pro-rata escape claims is not "surplus": the vault
    /// counts `escapePayable - escapeClaimedTotal` as still claimable, so less than one base unit per claimant stays
    /// in the vault for good, and VAULT.md v0.2's "the sweep pays ... rounding" was wrong for it. Pinned here (the
    /// engine story `shortfall` shows the same with real data: 2 units left).
    function test_R433_2_pro_rata_rounding_residue_is_never_swept() public {
        address bob = address(uint160(uint256(keccak256("r433 bob"))));
        address carol = address(uint160(uint256(keccak256("r433 carol"))));
        _deposit(alice, alice, 100 * USDC);
        _deposit(bob, bob, 100 * USDC);
        _deposit(carol, carol, 100 * USDC);
        a = new address[](6);
        b = new uint64[](6);
        a[0] = address(1);
        a[1] = address(2);
        a[2] = address(3);
        a[3] = alice;
        a[4] = bob;
        a[5] = carol;
        // the tree owes the three users 700 in equal sevenths-unfriendly parts against a pool of 300
        b[3] = 233_333_333;
        b[4] = 233_333_333;
        b[5] = 233_333_334;
        (PerpsVault.Withdrawal[] memory w,,) = _empty();
        _submit(_cp(1, w, a, b));
        vm.warp(block.timestamp + 7 days);
        vault.triggerEscape();
        _proveSys(a, b);
        for (uint256 i = 3; i < 6; ++i) {
            vault.claimEscape(_acctAt(a, b, i), Tree.proof(_leavesAt(a, b), i), type(uint64).max);
        }
        uint256 left = usdc.balanceOf(address(vault));
        assertTrue(left > 0 && left < 3, "a residue below one unit per claimant");
        assertEq(left, vault.escapePayable() - vault.escapeClaimedTotal(), "it is the unclaimed part of the pool");
        vm.warp(block.timestamp + 90 days);
        vm.expectRevert(PerpsVault.InvalidSurplus.selector);
        vault.sweepEscapeSurplus();
        // a donation on top IS surplus, and sweeping it leaves the residue where it was
        usdc.mint(address(vault), 5 * USDC);
        assertEq(vault.sweepEscapeSurplus(), 5 * USDC, "only the donation is swept");
        assertEq(usdc.balanceOf(address(vault)), left, "residue stays");
    }
}

// SPDX-License-Identifier: UNLICENSED
// Copyright (c) 2026 Bloch Inc.
pragma solidity 0.8.24;

import {PerpsVault} from "../src/PerpsVault.sol";
import {Base, Tree} from "./Base.sol";

/// Round 2, task 5 (gap 8 of VAULT.md v0.2): INSURANCE (0x..02) and BACKSTOP (0x..03) are funded through the vault
/// (SPEC 4.11: "seeds are deposits to the system account addresses"; SPEC O-3). v0.2 refused every keyless deposit
/// because nothing could refund one in escape (R395-26); v0.3 records the funder of a system deposit and refunds an
/// UNCREDITED one to that funder, so the property R395-26 protects (no value is ever sent to, or stranded for, a
/// keyless address) is kept. A CREDITED system deposit is engine money of the insurance fund / backstop: in escape it
/// is subordinated to users and its remainder goes to `feeRecipient` after 90 days (R395-33), never back to a funder.
contract SystemFundingTest is Base {
    address internal constant INSURANCE = address(2);
    address internal constant BACKSTOP = address(3);
    address internal owner = address(uint160(uint256(keccak256("r2 owner funder"))));
    address internal alice = address(uint160(uint256(keccak256("r2 alice"))));

    function setUp() public {
        _setUpVault();
    }

    function _fund(address from, address account, uint64 amount) internal returns (uint64) {
        return _deposit(from, account, amount);
    }

    /// Failing first on v0.2 (`f0e3967`): `InvalidDeposit`.
    function test_system_accounts_are_funded_through_the_vault() public {
        bytes32 h0 = vault.depositHead(0);
        uint64 i0 = _fund(owner, INSURANCE, 60_000 * USDC);
        uint64 i1 = _fund(owner, BACKSTOP, 25_000 * USDC);
        assertEq(i0, 0, "index 0");
        assertEq(i1, 1, "index 1");
        // the same chain link as any deposit (VAULT.md 5.4): the engine credits `account`, here the system account
        bytes32 h1 = keccak256(abi.encode(h0, uint256(0), INSURANCE, uint256(60_000 * USDC)));
        bytes32 h2 = keccak256(abi.encode(h1, uint256(1), BACKSTOP, uint256(25_000 * USDC)));
        assertEq(vault.depositHead(1), h1, "head 1");
        assertEq(vault.depositHead(2), h2, "head 2");
        assertEq(vault.custody(), 85_000 * USDC, "custody");
        assertEq(usdc.balanceOf(address(vault)), 85_000 * USDC, "balance");
        assertTrue(vault.systemFunder(0) == owner && vault.systemFunder(1) == owner, "funder recorded");
        // a system account never gains the right to post forced requests, and neither does its funder
        assertTrue(!vault.hasDeposited(INSURANCE) && !vault.hasDeposited(BACKSTOP), "system accounts are not depositors");
        assertTrue(!vault.hasDeposited(owner), "funding a system account is not a deposit of the funder");
        vm.prank(owner);
        vm.expectRevert(PerpsVault.InvalidForced.selector);
        vault.requestForced(2, 0);
    }

    /// R395-26 kept: every other keyless address, PROTOCOL_FEE included, is still refused; the usual deposit rules
    /// (minimum, cap, pause, escape) apply to system deposits.
    function test_other_keyless_deposits_still_refused_and_rules_apply() public {
        usdc.mint(owner, 1_000_000 * USDC);
        vm.startPrank(owner);
        usdc.approve(address(vault), type(uint256).max);
        vm.expectRevert(PerpsVault.InvalidDeposit.selector);
        vault.deposit(10 * USDC, address(0));
        vm.expectRevert(PerpsVault.InvalidDeposit.selector);
        vault.deposit(10 * USDC, address(1)); // PROTOCOL_FEE: fees accrue in the engine only
        for (uint160 k = 4; k < 12; ++k) {
            vm.expectRevert(PerpsVault.InvalidDeposit.selector);
            vault.deposit(10 * USDC, address(k)); // precompiles
        }
        vm.expectRevert(PerpsVault.InvalidDeposit.selector);
        vault.deposit(10 * USDC, address(0xffff));
        vm.expectRevert(PerpsVault.InvalidDeposit.selector);
        vault.deposit(10 * USDC - 1, INSURANCE); // below minDeposit
        vm.stopPrank();
        // pause stops system deposits like any deposit
        vm.prank(gaddrs[0]);
        vault.guardianPause();
        vm.prank(owner);
        vm.expectRevert(PerpsVault.Paused.selector);
        vault.deposit(10 * USDC, BACKSTOP);
    }

    /// In escape an UNCREDITED system deposit goes back to its funder (never to the keyless address, never to anyone
    /// else); a credited one is not refundable.
    function test_uncredited_system_deposit_is_refunded_to_its_funder() public {
        _deposit(alice, alice, 1_000 * USDC); // index 0
        _fund(owner, INSURANCE, 500 * USDC); // index 1, credited below
        (PerpsVault.Withdrawal[] memory w,,) = _empty();
        address[] memory a = new address[](4);
        uint64[] memory b = new uint64[](4);
        a[0] = address(1);
        a[1] = INSURANCE;
        a[2] = BACKSTOP;
        a[3] = alice;
        b[1] = 500 * USDC;
        b[3] = 1_000 * USDC;
        _submit(_cp(1, w, a, b));
        _fund(owner, BACKSTOP, 300 * USDC); // index 2, never credited
        vm.prank(alice);
        vault.requestForced(2, 0);
        vm.warp(block.timestamp + 2 days + 1);
        vault.triggerEscape();
        assertEq(vault.escapePool(), 1_500 * USDC, "the uncredited deposit is outside the pool");

        vm.expectRevert(PerpsVault.InvalidRefund.selector);
        vault.refundDeposit(1, INSURANCE, 500 * USDC); // credited: not refundable
        vm.expectRevert(PerpsVault.InvalidRefund.selector);
        vault.refundDeposit(2, owner, 300 * USDC); // the chain names the system account, not the funder
        vm.expectRevert(PerpsVault.InvalidRefund.selector);
        vault.refundDeposit(2, BACKSTOP, 301 * USDC);
        vm.prank(alice); // anyone may submit; the money goes to the funder
        vault.refundDeposit(2, BACKSTOP, 300 * USDC);
        assertEq(usdc.balanceOf(owner), 300 * USDC, "funder refunded");
        assertEq(usdc.balanceOf(BACKSTOP), 0, "nothing sent to the keyless address");
        vm.expectRevert(PerpsVault.InvalidRefund.selector);
        vault.refundDeposit(2, BACKSTOP, 300 * USDC); // once

        // the credited insurance stays subordinated: alice in full now, the remainder to the fee address after 90 days
        _proveSys(a, b);
        vault.claimEscape(_acctAt(a, b, 3), Tree.proof(_leavesAt(a, b), 3), type(uint64).max);
        assertEq(usdc.balanceOf(alice), 1_000 * USDC, "user paid in full");
        bytes32 key = vault.escapeKey(INSURANCE);
        vm.expectRevert(PerpsVault.NotReady.selector);
        vault.release(key, type(uint64).max);
        vm.warp(block.timestamp + 90 days);
        vault.release(key, type(uint64).max);
        assertEq(usdc.balanceOf(FEE), 500 * USDC, "credited insurance to the fee address after 90 days");
        assertEq(usdc.balanceOf(address(vault)), 0, "vault empty");
    }

    /// The insurance fund covers a user shortfall in escape even when it was funded through the vault: the funder has
    /// no senior claim on a credited system deposit.
    function test_funded_insurance_covers_users_first() public {
        _deposit(alice, alice, 1_000 * USDC);
        _fund(owner, INSURANCE, 400 * USDC);
        (PerpsVault.Withdrawal[] memory w,,) = _empty();
        address[] memory a = new address[](4);
        uint64[] memory b = new uint64[](4);
        a[0] = address(1);
        a[1] = INSURANCE;
        a[2] = BACKSTOP;
        a[3] = alice;
        // the engine's tree says alice is owed 1,300 (she won 300 from a counterparty the model leaves underwater) and
        // insurance 400: total 1,700 against a pool of 1,400
        b[1] = 400 * USDC;
        b[3] = 1_300 * USDC;
        _submit(_cp(1, w, a, b));
        vm.warp(block.timestamp + 7 days);
        vault.triggerEscape();
        _proveSys(a, b);
        vault.claimEscape(_acctAt(a, b, 3), Tree.proof(_leavesAt(a, b), 3), type(uint64).max);
        assertEq(usdc.balanceOf(alice), 1_300 * USDC, "user first, in full");
        (, uint64 total,,) = vault.payouts(vault.escapeKey(INSURANCE));
        assertEq(total, 100 * USDC, "only the remainder is the system share");
    }
}

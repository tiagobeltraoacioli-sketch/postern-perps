// SPDX-License-Identifier: UNLICENSED
// Copyright (c) 2026 Bloch Inc.
pragma solidity 0.8.24;

import {PerpsVault} from "../src/PerpsVault.sol";
import {MockBUSDC} from "../src/dev/MockBUSDC.sol";
import {Base, Tree} from "./Base.sol";

/// The local-stack token (`src/dev/MockBUSDC.sol`, deployed by `script/local_deploy.py`) against the real vault:
/// the vault's constructor and balance-delta checks accept it, and a deposit -> checkpoint -> withdrawal round trip
/// moves exact amounts. (The hostile-token cases stay in PerpsVault.t.sol with the switchable test token.)
contract DevTokenTest is Base {
    MockBUSDC internal dev;
    address internal alice = address(0xA11CE);

    function setUp() public {
        _setUpVault();
        dev = new MockBUSDC();
        PerpsVault.Config memory c = _config();
        c.token = address(dev);
        vault = new PerpsVault(c);
    }

    function test_dev_token_faucet_bounds() public {
        assertEq(dev.decimals(), 6, "decimals");
        vm.expectRevert(MockBUSDC.InvalidMint.selector);
        dev.mint(address(0), 1);
        vm.expectRevert(MockBUSDC.InvalidMint.selector);
        dev.mint(alice, 0);
        vm.expectRevert(MockBUSDC.InvalidMint.selector);
        dev.mint(alice, 10_000_000e6 + 1);
        dev.mint(alice, 10_000_000e6);
        assertEq(dev.balanceOf(alice), 10_000_000e6, "minted");
        assertEq(dev.totalSupply(), 10_000_000e6, "supply");
    }

    function test_dev_token_transfer_refusals() public {
        dev.mint(alice, 100e6);
        vm.prank(alice);
        vm.expectRevert(MockBUSDC.InsufficientBalance.selector);
        dev.transfer(address(7), 100e6 + 1);
        vm.prank(alice);
        vm.expectRevert(MockBUSDC.InvalidRecipient.selector);
        dev.transfer(address(0), 1);
        vm.expectRevert(MockBUSDC.InsufficientAllowance.selector);
        dev.transferFrom(alice, address(this), 1);
        // The vault's deposit without an approval fails in the token, never half-way.
        vm.prank(alice);
        vm.expectRevert(PerpsVault.InvalidTransfer.selector);
        vault.deposit(50e6, alice);
        assertEq(vault.depositCount(), 0, "no deposit recorded");
    }

    function test_dev_token_deposit_checkpoint_withdraw_round_trip() public {
        dev.mint(alice, 1_000e6);
        vm.startPrank(alice);
        dev.approve(address(vault), 1_000e6);
        vault.deposit(1_000e6, alice);
        vm.stopPrank();
        assertEq(dev.balanceOf(address(vault)), 1_000e6, "vault holds the deposit");
        assertEq(dev.allowance(alice, address(vault)), 0, "allowance consumed");

        PerpsVault.Withdrawal[] memory ws = new PerpsVault.Withdrawal[](1);
        ws[0] = _w(0, alice, alice, 400e6);
        address[] memory accts = new address[](1);
        uint64[] memory bals = new uint64[](1);
        accts[0] = alice;
        bals[0] = 600e6;
        _submit(_cp(1, ws, accts, bals));
        vault.withdraw(1, ws[0], _wProof(ws, 0), type(uint64).max);
        assertEq(dev.balanceOf(alice), 400e6, "paid exactly");
        assertEq(dev.balanceOf(address(vault)), 600e6, "vault keeps the rest");
        assertEq(vault.custody(), 600e6, "custody");
        assertEq(vault.reserved(), 0, "nothing reserved");
    }
}

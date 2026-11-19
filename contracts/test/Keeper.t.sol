// SPDX-License-Identifier: UNLICENSED
// Copyright (c) 2026 Bloch Inc.
pragma solidity 0.8.24;

import {PerpsVault} from "../src/PerpsVault.sol";
import {Base} from "./Base.sol";

/// R395-19 (reviewer 395, Medium): the vault measures "no new root" by checkpoints SUBMITTED to it. These tests pin
/// the facts the keeper duty of VAULT.md section 8a rests on: what a quiet relayer costs, who can repair it and until
/// when, and why the forced-request deadline (not `escapeDelay`) is the bound a keeper must meet.
contract KeeperDutyTest is Base {
    address internal alice = address(uint160(uint256(keccak256("keeper alice"))));
    address internal stranger = address(uint160(uint256(keccak256("keeper stranger"))));
    address[] internal a;
    uint64[] internal b;

    function setUp() public {
        _setUpVault();
        _deposit(alice, alice, 1_000 * USDC);
        a = new address[](1);
        b = new uint64[](1);
        a[0] = alice;
        b[0] = 1_000 * USDC;
    }

    function _signed(uint64 height) internal view returns (PerpsVault.Checkpoint memory cp, bytes[] memory sigs) {
        (PerpsVault.Withdrawal[] memory w,,) = _empty();
        cp = _cp(height, w, a, b);
        sigs = _sign(vault.checkpointDigest(cp), K);
    }

    /// The cost of a quiet relayer: the committee is alive and finalizes (the signed checkpoint exists), nobody
    /// submits for `escapeDelay`, anyone ends the venue for good, and the finalized checkpoint is then worthless.
    function test_R395_19_quiet_relayer_with_a_live_committee_ends_in_escape() public {
        (PerpsVault.Checkpoint memory c1, bytes[] memory s1) = _signed(1);
        vault.submitCheckpoint(c1, s1);
        (PerpsVault.Checkpoint memory c2, bytes[] memory s2) = _signed(2); // finalized off-chain, never relayed
        vm.warp(block.timestamp + 7 days);
        vm.prank(stranger);
        vault.triggerEscape();
        vm.expectRevert(PerpsVault.EscapeActive.selector);
        vault.submitCheckpoint(c2, s2);
    }

    /// The repair needs no privileged party: ANY address holding the finalized checkpoint submits it, even after
    /// the silence bound has passed, as long as nobody has triggered escape yet; that restarts the clock.
    function test_R395_19_anyone_submits_and_a_late_submission_still_counts() public {
        (PerpsVault.Checkpoint memory c1, bytes[] memory s1) = _signed(1);
        vault.submitCheckpoint(c1, s1);
        (PerpsVault.Checkpoint memory c2, bytes[] memory s2) = _signed(2);
        vm.warp(block.timestamp + 7 days + 3 hours);
        assertEq(vault.escapeReason(), 1, "escape is due");
        vm.prank(stranger);
        vault.submitCheckpoint(c2, s2);
        assertEq(vault.escapeReason(), 0, "no longer due");
        vm.expectRevert(PerpsVault.EscapeNotDue.selector);
        vault.triggerEscape();
        vm.warp(block.timestamp + 7 days - 1);
        assertEq(vault.escapeReason(), 0, "a full escapeDelay from the late submission");
    }

    /// The bound a keeper must meet is `forcedDelay`, not `escapeDelay`: any depositor starts a two-day clock, and a
    /// checkpoint that merely exists (submitted or not) does not stop it unless it PROCESSED the request.
    function test_R395_19_forced_request_makes_forcedDelay_the_keeper_bound() public {
        (PerpsVault.Checkpoint memory c1, bytes[] memory s1) = _signed(1);
        vault.submitCheckpoint(c1, s1);
        vm.prank(alice);
        vault.requestForced(0, 0);
        // the engine processes it and the committee finalizes; the relayer stays quiet
        (PerpsVault.Checkpoint memory c2, bytes[] memory s2) = _signed(2);
        assertEq(c2.nextForcedIndex, 1, "the finalized checkpoint processed the request");
        vm.warp(block.timestamp + 2 days);
        assertEq(vault.escapeReason(), 0, "at the deadline: not yet");
        vm.warp(block.timestamp + 1);
        assertEq(vault.escapeReason(), 2, "one second later anyone may end the venue");
        // still repairable until someone does
        vm.prank(stranger);
        vault.submitCheckpoint(c2, s2);
        assertEq(vault.escapeReason(), 0, "repaired by the late relay");
    }

    /// R433-3 (Low, liveness of the silence path only; documented, not changed): the silence clock restarts at every
    /// accepted checkpoint, whatever its age. Whoever holds a backlog of finalized but unrelayed checkpoints of a
    /// committee that has since died can postpone escape-by-silence by one `escapeDelay` per checkpoint. The exit
    /// guarantee does not rest on that path: a forced request, which no old checkpoint can have processed, opens
    /// escape `forcedDelay` after it is posted, backlog or not.
    function test_R433_3_backlog_postpones_silence_but_not_a_forced_request() public {
        (PerpsVault.Checkpoint memory c1, bytes[] memory s1) = _signed(1);
        vault.submitCheckpoint(c1, s1);
        // the committee finalizes 2, 3, 4 and then dies; nobody relayed them
        (PerpsVault.Checkpoint memory c2, bytes[] memory s2) = _signed(2);
        (PerpsVault.Checkpoint memory c3, bytes[] memory s3) = _signed(3);
        (PerpsVault.Checkpoint memory c4, bytes[] memory s4) = _signed(4);
        vm.warp(block.timestamp + 7 days - 1);
        vault.submitCheckpoint(c2, s2);
        vm.warp(block.timestamp + 7 days - 1);
        vault.submitCheckpoint(c3, s3);
        vm.warp(block.timestamp + 7 days - 1);
        assertEq(vault.escapeReason(), 0, "three weeks after the committee died the silence path is still shut");
        // alice does not wait for it: her forced request is beyond every checkpoint that exists
        vm.prank(alice);
        vault.requestForced(1, 0);
        vm.warp(block.timestamp + 1 days);
        vault.submitCheckpoint(c4, s4); // the last of the backlog: accepted (the request is not overdue yet) ...
        vm.warp(block.timestamp + 1 days + 1);
        assertEq(vault.escapeReason(), 2, "... and useless against the deadline");
        vault.triggerEscape();
        assertTrue(vault.inEscape(), "exit open forcedDelay after the request");
    }
}

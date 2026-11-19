// SPDX-License-Identifier: UNLICENSED
pragma solidity 0.8.24;

import {PerpsVault} from "../src/PerpsVault.sol";
import {Base, MockUSDC, Tree} from "./Base.sol";

contract PerpsVaultTest is Base {
    address internal alice = address(uint160(uint256(keccak256("alice"))));
    address internal bob = address(uint160(uint256(keccak256("bob"))));
    address internal carol = address(uint160(uint256(keccak256("carol"))));

    VaultHarness internal harness;

    function setUp() public {
        _setUpVault();
        harness = new VaultHarness(_config());
    }

    // ================================================================== configuration
    function _expectConfigRevert(PerpsVault.Config memory c) internal {
        vm.expectRevert(PerpsVault.InvalidConfiguration.selector);
        new PerpsVault(c);
    }

    function test_config_accepts_launch_defaults() public view {
        assertEq(vault.threshold(), 5, "k");
        assertEq(vault.validatorCount(), 7, "n");
        assertEq(vault.guardianCount(), 3, "g");
        assertTrue(vault.feeRecipient() == FEE, "fee recipient pinned");
        assertEq(vault.availableToRelease(), 1_000_000 * USDC, "bucket full at deployment");
    }

    function test_config_refusals() public {
        PerpsVault.Config memory c;
        c = _config(); c.threshold = 1; _expectConfigRevert(c);
        c = _config(); c.threshold = 3; _expectConfigRevert(c); // 2k <= n: two disjoint quorums could sign
        c = _config(); c.threshold = 8; _expectConfigRevert(c);
        c = _config(); c.unpauseThreshold = 4; _expectConfigRevert(c);
        c = _config(); c.unpauseThreshold = 8; _expectConfigRevert(c);
        c = _config(); c.feeRecipient = address(0); _expectConfigRevert(c);
        c = _config(); c.feeRecipient = address(usdc); _expectConfigRevert(c);
        c = _config(); c.cap = 0; _expectConfigRevert(c);
        c = _config(); c.minDeposit = 0; _expectConfigRevert(c);
        c = _config(); c.windowLimit = c.cap + 1; _expectConfigRevert(c);
        c = _config(); c.windowSeconds = 0; _expectConfigRevert(c);
        c = _config(); c.largeThreshold = 0; _expectConfigRevert(c);
        c = _config(); c.largeThreshold = c.windowLimit + 1; _expectConfigRevert(c);
        c = _config(); c.largeDelay = 0; _expectConfigRevert(c);
        c = _config(); c.largeDelay = 14 days + 1; _expectConfigRevert(c);
        c = _config(); c.forcedDelay = 1 hours - 1; _expectConfigRevert(c);
        c = _config(); c.forcedDelay = 7 days + 1; _expectConfigRevert(c);
        c = _config(); c.escapeDelay = 1 days - 1; _expectConfigRevert(c);
        c = _config(); c.escapeDelay = 30 days + 1; _expectConfigRevert(c);
        c = _config(); c.forcedDelay = 2 days; c.escapeDelay = 1 days + 1; _expectConfigRevert(c);
        c = _config(); c.pauseDuration = 0; _expectConfigRevert(c);
        c = _config(); c.pauseDuration = 14 days + 1; c.pauseCooldown = 15 days; _expectConfigRevert(c);
        c = _config(); c.pauseCooldown = c.pauseDuration - 1; _expectConfigRevert(c);
        c = _config(); c.guardians = new address[](0); _expectConfigRevert(c); // pause values without guardians
        // unsorted / duplicate validators
        c = _config();
        address[] memory v = new address[](7);
        for (uint256 i; i < 7; ++i) v[i] = vaddrs[i];
        (v[0], v[1]) = (v[1], v[0]);
        c.validators = v;
        _expectConfigRevert(c);
        c = _config();
        v = new address[](7);
        for (uint256 i; i < 7; ++i) v[i] = vaddrs[i];
        v[1] = v[0];
        c.validators = v;
        _expectConfigRevert(c);
        // a guardian that is a validator, or the fee recipient
        c = _config();
        address[] memory g = new address[](1);
        g[0] = vaddrs[0];
        c.guardians = g;
        _expectConfigRevert(c);
        c = _config();
        g[0] = FEE;
        c.guardians = g;
        _expectConfigRevert(c);
        // token with other decimals, and a token address without code
        MockUSDC t18 = new MockUSDC();
        t18.setDecimals(18);
        c = _config(); c.token = address(t18); _expectConfigRevert(c);
        c = _config(); c.token = address(0xdead); _expectConfigRevert(c);
        // 17 validators
        c = _config();
        (, address[] memory many) = _sortedKeys(9, 17);
        c.validators = many;
        c.threshold = 9;
        c.unpauseThreshold = 9;
        _expectConfigRevert(c);
    }

    // ================================================================== deposits
    function test_deposit_extends_chain() public {
        uint64 i0 = _deposit(alice, alice, 100 * USDC);
        uint64 i1 = _deposit(alice, bob, 50 * USDC);
        assertEq(i0, 0, "index 0");
        assertEq(i1, 1, "index 1");
        bytes32 h0 = keccak256(abi.encode(bytes32("PosternPerps.deposit.v1"), block.chainid, address(vault)));
        assertEq(vault.depositHead(0), h0, "genesis");
        bytes32 h1 = keccak256(abi.encode(h0, uint256(0), alice, uint256(100 * USDC)));
        bytes32 h2 = keccak256(abi.encode(h1, uint256(1), bob, uint256(50 * USDC)));
        assertEq(vault.depositHead(1), h1, "head 1");
        assertEq(vault.depositHead(2), h2, "head 2 credits bob, not the sender");
        assertEq(vault.depositTotalAt(2), 150 * USDC, "cumulative");
        assertEq(vault.custody(), 150 * USDC, "custody");
        assertEq(usdc.balanceOf(address(vault)), 150 * USDC, "balance");
        assertTrue(vault.hasDeposited(bob) && !vault.hasDeposited(carol), "depositor flags");
    }

    function test_deposit_refusals() public {
        usdc.mint(alice, 100_000_000 * USDC);
        vm.startPrank(alice);
        usdc.approve(address(vault), type(uint256).max);
        vm.expectRevert(PerpsVault.InvalidDeposit.selector);
        vault.deposit(10 * USDC - 1, alice);
        vm.expectRevert(PerpsVault.InvalidDeposit.selector);
        vault.deposit(10 * USDC, address(0));
        vm.expectRevert(PerpsVault.InvalidDeposit.selector);
        vault.deposit(10 * USDC, address(vault));
        vm.expectRevert(PerpsVault.InvalidDeposit.selector);
        vault.deposit(10 * USDC, address(usdc));
        vm.expectRevert(PerpsVault.InvalidDeposit.selector);
        vault.deposit(10_000_000 * USDC + 1, alice); // above the cap
        vault.deposit(10_000_000 * USDC, alice); // exactly the cap
        vm.expectRevert(PerpsVault.InvalidDeposit.selector);
        vault.deposit(10 * USDC, alice); // custody at cap
        vm.stopPrank();
    }

    function test_deposit_refuses_fee_on_transfer_and_false_return() public {
        usdc.mint(alice, 1000 * USDC);
        vm.prank(alice);
        usdc.approve(address(vault), type(uint256).max);
        usdc.setSkim(1);
        vm.prank(alice);
        vm.expectRevert(PerpsVault.InvalidTransfer.selector);
        vault.deposit(100 * USDC, alice);
        usdc.setSkim(0);
        usdc.setReturnFalse(true);
        vm.prank(alice);
        vm.expectRevert(PerpsVault.InvalidTransfer.selector);
        vault.deposit(100 * USDC, alice);
    }

    // ================================================================== checkpoints
    function _basicCheckpoint() internal returns (PerpsVault.Checkpoint memory cp) {
        _deposit(alice, alice, 1000 * USDC);
        (PerpsVault.Withdrawal[] memory w, address[] memory a, uint64[] memory b) = _empty();
        a = new address[](1);
        b = new uint64[](1);
        a[0] = alice;
        b[0] = 1000 * USDC;
        cp = _cp(1, w, a, b);
    }

    function test_checkpoint_accepted() public {
        PerpsVault.Checkpoint memory cp = _basicCheckpoint();
        _submit(cp);
        assertEq(vault.checkpointCount(), 1, "seq");
        (uint64 h,,,,,,,,) = vault.latest();
        assertEq(h, 1, "height");
        assertEq(vault.lastCheckpointAt(), block.timestamp, "time");
    }

    function test_checkpoint_more_than_threshold_signatures_ok() public {
        PerpsVault.Checkpoint memory cp = _basicCheckpoint();
        vault.submitCheckpoint(cp, _sign(vault.checkpointDigest(cp), 7));
    }

    function test_checkpoint_signature_refusals() public {
        PerpsVault.Checkpoint memory cp = _basicCheckpoint();
        bytes32 d = vault.checkpointDigest(cp);
        bytes[] memory s;
        // k - 1
        s = _sign(d, K - 1);
        vm.expectRevert(PerpsVault.InvalidCertificate.selector);
        vault.submitCheckpoint(cp, s);
        // unsorted
        s = _sign(d, K);
        (s[0], s[1]) = (s[1], s[0]);
        vm.expectRevert(PerpsVault.InvalidCertificate.selector);
        vault.submitCheckpoint(cp, s);
        // duplicate signer
        s = _sign(d, K);
        s[1] = s[0];
        vm.expectRevert(PerpsVault.InvalidCertificate.selector);
        vault.submitCheckpoint(cp, s);
        // a stranger's key in place of a validator
        s = _sign(d, K);
        (uint8 v, bytes32 r, bytes32 ss) = vm.sign(0xBAD, d);
        s[K - 1] = abi.encodePacked(r, ss, v);
        vm.expectRevert(PerpsVault.InvalidCertificate.selector);
        vault.submitCheckpoint(cp, s);
        // high-s twin of a valid signature
        s = _sign(d, K);
        s[0] = _malleate(s[0]);
        vm.expectRevert(PerpsVault.InvalidCertificate.selector);
        vault.submitCheckpoint(cp, s);
        // v outside 27/28
        s = _sign(d, K);
        s[0][64] = bytes1(uint8(s[0][64]) - 27);
        vm.expectRevert(PerpsVault.InvalidCertificate.selector);
        vault.submitCheckpoint(cp, s);
        // 64-byte signature
        s = _sign(d, K);
        bytes memory short = new bytes(64);
        for (uint256 i; i < 64; ++i) short[i] = s[0][i];
        s[0] = short;
        vm.expectRevert(PerpsVault.InvalidCertificate.selector);
        vault.submitCheckpoint(cp, s);
        // more signatures than validators
        bytes[] memory eight = new bytes[](8);
        s = _sign(d, 7);
        for (uint256 i; i < 7; ++i) eight[i] = s[i];
        eight[7] = s[6];
        vm.expectRevert(PerpsVault.InvalidCertificate.selector);
        vault.submitCheckpoint(cp, eight);
        // signatures over a different field
        PerpsVault.Checkpoint memory other = cp;
        other.accountsRoot = keccak256("other");
        vm.expectRevert(PerpsVault.InvalidCertificate.selector);
        vault.submitCheckpoint(other, _sign(d, K));
    }

    function _malleate(bytes memory sig) internal pure returns (bytes memory out) {
        bytes32 r;
        bytes32 s;
        assembly {
            r := mload(add(sig, 32))
            s := mload(add(sig, 64))
        }
        uint8 v = uint8(sig[64]);
        uint256 n = 0xfffffffffffffffffffffffffffffffebaaedce6af48a03bbfd25e8cd0364141;
        out = abi.encodePacked(r, bytes32(n - uint256(s)), v == 27 ? uint8(28) : uint8(27));
    }

    function test_checkpoint_domain_binds_chain_and_vault() public {
        PerpsVault.Checkpoint memory cp = _basicCheckpoint();
        bytes[] memory s = _sign(vault.checkpointDigest(cp), K);
        // Another vault with the same committee and the same deposits does not accept it.
        PerpsVault twin = new PerpsVault(_config());
        vm.expectRevert(PerpsVault.InvalidCheckpoint.selector); // its deposit chain differs (bound to its address)
        twin.submitCheckpoint(cp, s);
        // Another chain id: the digest changes, so the same signatures fail.
        vm.chainId(700771);
        PerpsVault.Checkpoint memory cp2 = cp;
        cp2.depositsHead = vault.depositHead(cp.nextDepositIndex);
        cp2.forcedHead = vault.forcedHead(0);
        vm.expectRevert(PerpsVault.InvalidCertificate.selector);
        vault.submitCheckpoint(cp2, s);
        vm.chainId(31337);
    }

    function test_checkpoint_field_refusals() public {
        PerpsVault.Checkpoint memory cp = _basicCheckpoint();
        _submit(cp);
        _deposit(bob, bob, 500 * USDC);
        PerpsVault.Checkpoint memory n;
        (PerpsVault.Withdrawal[] memory w, address[] memory a, uint64[] memory b) = _empty();
        // height not increasing
        n = _cp(1, w, a, b);
        _expectCheckpointRevert(n);
        // time not increasing
        n = _cp(2, w, a, b);
        n.time = cp.time;
        _expectCheckpointRevert(n);
        // more deposits credited than exist
        n = _cp(2, w, a, b);
        n.nextDepositIndex = 3;
        n.depositsHead = vault.depositHead(2);
        _expectCheckpointRevert(n);
        // fewer deposits than the previous checkpoint
        n = _cp(2, w, a, b);
        n.nextDepositIndex = 0;
        n.depositsHead = vault.depositHead(0);
        _expectCheckpointRevert(n);
        // a head that is not the vault's (a deposit the engine invented or mis-read)
        n = _cp(2, w, a, b);
        n.depositsHead = keccak256("invented");
        _expectCheckpointRevert(n);
        // forced head wrong
        n = _cp(2, w, a, b);
        n.forcedHead = keccak256("x");
        _expectCheckpointRevert(n);
        // total without records; a total that moved while the count did not
        n = _cp(2, w, a, b);
        n.withdrawalsTotal = 1;
        _expectCheckpointRevert(n);
        // a forced count above the vault's
        n = _cp(2, w, a, b);
        n.nextForcedIndex = 1;
        _expectCheckpointRevert(n);
        // a valid one still passes after all those refusals
        _submit(_cp(2, w, a, b));
    }

    /// The vault recomputes SPEC 11's state root from the signed components: an independent computation here.
    function test_state_root_equation() public {
        PerpsVault.Checkpoint memory cp = _basicCheckpoint();
        bytes32 sys = keccak256(
            abi.encodePacked(
                bytes7("PPv1sys"),
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
        bytes32 expected = keccak256(
            abi.encodePacked(
                bytes21("PosternPerps/state/v1"),
                cp.height,
                cp.time,
                cp.accountsRoot,
                cp.marketsRoot,
                cp.withdrawalsRoot,
                sys
            )
        );
        _submit(cp);
        (,,,,,,,, bytes32 stored) = vault.latest();
        assertEq(stored, expected, "stateRoot");
    }

    function _expectCheckpointRevert(PerpsVault.Checkpoint memory cp) internal {
        bytes[] memory s = _sign(vault.checkpointDigest(cp), K);
        vm.expectRevert(PerpsVault.InvalidCheckpoint.selector);
        vault.submitCheckpoint(cp, s);
    }

    function test_checkpoint_withdrawals_append_only_rules() public {
        _deposit(alice, alice, 1000 * USDC);
        PerpsVault.Withdrawal[] memory ws = new PerpsVault.Withdrawal[](1);
        ws[0] = _w(0, alice, alice, 100 * USDC);
        (, address[] memory a, uint64[] memory b) = _empty();
        _submit(_cp(1, ws, a, b));
        // same count, different root: refused
        PerpsVault.Withdrawal[] memory ws2 = new PerpsVault.Withdrawal[](1);
        ws2[0] = _w(0, alice, bob, 100 * USDC);
        _expectCheckpointRevert(_cp(2, ws2, a, b));
        // fewer records: refused
        (PerpsVault.Withdrawal[] memory none,,) = _empty();
        _expectCheckpointRevert(_cp(2, none, a, b));
        // same records again: accepted
        _submit(_cp(2, ws, a, b));
    }

    function test_checkpoint_refused_when_insolvent() public {
        _deposit(alice, alice, 1000 * USDC);
        (, address[] memory a, uint64[] memory b) = _empty();
        // Withdrawal records above custody.
        PerpsVault.Withdrawal[] memory ws = new PerpsVault.Withdrawal[](1);
        ws[0] = _w(0, alice, alice, 1000 * USDC + 1);
        PerpsVault.Checkpoint memory cp = _cp(1, ws, a, b);
        bytes[] memory s = _sign(vault.checkpointDigest(cp), K);
        vm.expectRevert(PerpsVault.Insolvent.selector);
        vault.submitCheckpoint(cp, s);
        // Uncredited deposits count as owed in full: records of 1 with nothing credited exceed custody.
        ws[0] = _w(0, alice, alice, 1);
        cp = _cp(1, ws, a, b);
        cp.nextDepositIndex = 0;
        cp.depositsHead = vault.depositHead(0);
        s = _sign(vault.checkpointDigest(cp), K);
        vm.expectRevert(PerpsVault.Insolvent.selector);
        vault.submitCheckpoint(cp, s);
    }

    /// R395-23: an account under water makes sum(max(0, E)) exceed custody. The checkpoint is still accepted (escape
    /// values are paid pro rata from the pool), so withdrawals keep flowing.
    function test_R395_23_escape_total_above_custody_accepted_and_paid_pro_rata() public {
        _deposit(alice, alice, 1000 * USDC);
        _deposit(bob, bob, 1000 * USDC);
        (PerpsVault.Withdrawal[] memory w,,) = _empty();
        address[] memory a = new address[](2);
        uint64[] memory b = new uint64[](2);
        a[0] = alice;
        a[1] = bob;
        b[0] = 1500 * USDC; // alice's profit
        b[1] = 1000 * USDC; // bob's escape value floored at zero loss elsewhere: total 2500 > custody 2000
        _submit(_cp(1, w, a, b));
        vm.warp(block.timestamp + 7 days);
        vault.triggerEscape();
        _proveSys(a, b);
        assertEq(vault.escapePayable(), 2000 * USDC, "pool");
        vault.claimEscape(_acct(a, b, 0), _eProof(a, b, 0), type(uint64).max);
        vault.claimEscape(_acct(a, b, 1), _eProof(a, b, 1), type(uint64).max);
        assertEq(usdc.balanceOf(alice), 1200 * USDC, "1500 * 2000 / 2500");
        assertEq(usdc.balanceOf(bob), 800 * USDC, "1000 * 2000 / 2500");
        assertEq(usdc.balanceOf(address(vault)), 0, "pool paid exactly");
    }

    // ================================================================== withdrawals
    function _withdrawSetup(uint64 amount) internal returns (PerpsVault.Withdrawal[] memory ws) {
        _deposit(alice, alice, 2_000_000 * USDC);
        ws = new PerpsVault.Withdrawal[](3);
        ws[0] = _w(0, alice, carol, amount);
        ws[1] = _w(1, PROTOCOL_FEE, FEE, 7 * USDC);
        ws[2] = _w(2, alice, alice, 3 * USDC);
        address[] memory a = new address[](1);
        uint64[] memory b = new uint64[](1);
        a[0] = alice;
        b[0] = 2_000_000 * USDC - amount - 10 * USDC;
        _submit(_cp(1, ws, a, b));
    }

    function test_withdraw_pays_recipient_once() public {
        PerpsVault.Withdrawal[] memory ws = _withdrawSetup(500 * USDC);
        bytes32 key = vault.withdraw(1, ws[0], _wProof(ws, 0), type(uint64).max);
        assertEq(usdc.balanceOf(carol), 500 * USDC, "paid");
        (address r, uint64 total, uint64 paid,) = vault.payouts(key);
        assertTrue(r == carol && total == 500 * USDC && paid == 500 * USDC, "payout record");
        bytes32[] memory p = _wProof(ws, 0);
        vm.expectRevert(PerpsVault.AlreadyRegistered.selector);
        vault.withdraw(1, ws[0], p, 1);
        // the fee account's record pays the fee address
        vault.withdraw(1, ws[1], _wProof(ws, 1), type(uint64).max);
        assertEq(usdc.balanceOf(FEE), 7 * USDC, "fee paid to the owner address");
        assertEq(vault.reserved(), 0, "nothing reserved");
    }

    function test_withdraw_against_older_checkpoint_and_no_double_pay_across_checkpoints() public {
        PerpsVault.Withdrawal[] memory ws = _withdrawSetup(500 * USDC);
        // a second checkpoint with one more record
        PerpsVault.Withdrawal[] memory ws2 = new PerpsVault.Withdrawal[](4);
        for (uint256 i; i < 3; ++i) ws2[i] = ws[i];
        ws2[3] = _w(3, alice, alice, 1 * USDC);
        address[] memory a = new address[](1);
        uint64[] memory b = new uint64[](1);
        a[0] = alice;
        b[0] = 2_000_000 * USDC - 500 * USDC - 11 * USDC;
        vm.warp(block.timestamp + 1);
        _submit(_cp(2, ws2, a, b));
        vault.withdraw(1, ws[0], _wProof(ws, 0), type(uint64).max); // proof against checkpoint 1
        bytes32[] memory p = _wProof(ws2, 0);
        vm.expectRevert(PerpsVault.AlreadyRegistered.selector);
        vault.withdraw(2, ws2[0], p, type(uint64).max);
        // record 3 is not in checkpoint 1
        p = _wProof(ws2, 3);
        vm.expectRevert(PerpsVault.InvalidWithdrawal.selector);
        vault.withdraw(1, ws2[3], p, type(uint64).max);
        vault.withdraw(2, ws2[3], p, type(uint64).max);
    }

    function test_withdraw_refusals() public {
        PerpsVault.Withdrawal[] memory ws = _withdrawSetup(500 * USDC);
        bytes32[] memory p = _wProof(ws, 0);
        PerpsVault.Withdrawal memory w = _copyW(ws[0]);
        // tampered amount, recipient, account, index
        w.amount = 501 * USDC;
        vm.expectRevert(PerpsVault.InvalidProof.selector);
        vault.withdraw(1, w, p, 1);
        w = _copyW(ws[0]);
        w.recipient = bob;
        vm.expectRevert(PerpsVault.InvalidProof.selector);
        vault.withdraw(1, w, p, 1);
        w = _copyW(ws[0]);
        w.account = bob;
        vm.expectRevert(PerpsVault.InvalidProof.selector);
        vault.withdraw(1, w, p, 1);
        w = _copyW(ws[0]);
        w.height = 1; // the record's height is part of the leaf
        vm.expectRevert(PerpsVault.InvalidProof.selector);
        vault.withdraw(1, w, p, 1);
        w = _copyW(ws[0]);
        w.withdrawalId = 1; // the same leaf data proven at another position
        vm.expectRevert(PerpsVault.InvalidProof.selector);
        vault.withdraw(1, w, p, 1);
        // record 1's real proof with record 0's id: position bound
        bytes32[] memory p1 = _wProof(ws, 1);
        vm.expectRevert(PerpsVault.InvalidProof.selector);
        vault.withdraw(1, ws[0], p1, 1);
        // unknown checkpoint
        p = _wProof(ws, 0);
        vm.expectRevert(PerpsVault.InvalidWithdrawal.selector);
        vault.withdraw(0, ws[0], p, 1);
        vm.expectRevert(PerpsVault.InvalidWithdrawal.selector);
        vault.withdraw(2, ws[0], p, 1);
        // an id at or above the checkpoint's record count
        w = _copyW(ws[0]);
        w.withdrawalId = 3;
        vm.expectRevert(PerpsVault.InvalidWithdrawal.selector);
        vault.withdraw(1, w, p, 1);
        // a sibling changed
        p[5] = keccak256("x");
        vm.expectRevert(PerpsVault.InvalidProof.selector);
        vault.withdraw(1, ws[0], p, 1);
    }

    function test_fee_account_record_to_other_recipient_refused() public {
        _deposit(alice, alice, 1000 * USDC);
        PerpsVault.Withdrawal[] memory ws = new PerpsVault.Withdrawal[](1);
        ws[0] = _w(0, PROTOCOL_FEE, bob, 5 * USDC);
        (, address[] memory a, uint64[] memory b) = _empty();
        _submit(_cp(1, ws, a, b));
        bytes32[] memory p = _wProof(ws, 0);
        vm.expectRevert(PerpsVault.InvalidWithdrawal.selector);
        vault.withdraw(1, ws[0], p, 1);
    }

    function test_withdraw_recipient_refusals() public {
        _deposit(alice, alice, 1000 * USDC);
        PerpsVault.Withdrawal[] memory ws = new PerpsVault.Withdrawal[](4);
        ws[0] = _w(0, alice, address(0), 1 * USDC);
        ws[1] = _w(1, alice, address(vault), 1 * USDC);
        ws[2] = _w(2, alice, address(usdc), 1 * USDC);
        ws[3] = _w(3, address(0), alice, 1 * USDC);
        (, address[] memory a, uint64[] memory b) = _empty();
        _submit(_cp(1, ws, a, b));
        for (uint64 i; i < 4; ++i) {
            bytes32[] memory p = _wProof(ws, i);
            vm.expectRevert(PerpsVault.InvalidWithdrawal.selector);
            vault.withdraw(1, ws[i], p, 1);
        }
    }

    function test_large_withdrawal_waits_delay() public {
        PerpsVault.Withdrawal[] memory ws = _withdrawSetup(100_000 * USDC);
        bytes32[] memory p = _wProof(ws, 0);
        bytes32 key = vault.withdraw(1, ws[0], p, type(uint64).max); // registers; not ready, so nothing paid
        assertEq(usdc.balanceOf(carol), 0, "nothing paid before the delay");
        (,, uint64 paid0, uint64 readyAt) = vault.payouts(key);
        assertTrue(paid0 == 0 && readyAt == block.timestamp + 1 days, "registered with its delay");
        vm.expectRevert(PerpsVault.AlreadyRegistered.selector);
        vault.withdraw(1, ws[0], p, 0);
        vm.expectRevert(PerpsVault.NotReady.selector);
        vault.release(key, type(uint64).max);
        vm.warp(block.timestamp + 1 days - 1);
        vm.expectRevert(PerpsVault.NotReady.selector);
        vault.release(key, type(uint64).max);
        vm.warp(block.timestamp + 1);
        vault.release(key, type(uint64).max);
        assertEq(usdc.balanceOf(carol), 100_000 * USDC, "paid after delay");
        // just below the threshold: immediate
    }

    function test_below_large_threshold_is_immediate() public {
        PerpsVault.Withdrawal[] memory ws = _withdrawSetup(100_000 * USDC - 1);
        vault.withdraw(1, ws[0], _wProof(ws, 0), type(uint64).max);
        assertEq(usdc.balanceOf(carol), 100_000 * USDC - 1, "paid");
    }

    function test_partial_release_in_bucket_steps() public {
        _deposit(alice, alice, 3_000_000 * USDC);
        PerpsVault.Withdrawal[] memory ws = new PerpsVault.Withdrawal[](1);
        ws[0] = _w(0, alice, alice, 2_500_000 * USDC);
        (, address[] memory a, uint64[] memory b) = _empty();
        _submit(_cp(1, ws, a, b));
        bytes32 key = vault.withdraw(1, ws[0], _wProof(ws, 0), 0);
        vm.warp(block.timestamp + 1 days);
        vault.release(key, type(uint64).max); // the bucket allows 1,000,000
        assertEq(usdc.balanceOf(alice), 1_000_000 * USDC, "first part = window limit");
        vm.expectRevert(PerpsVault.RateLimited.selector);
        vault.release(key, 1);
        vm.warp(block.timestamp + 12 hours);
        vault.release(key, type(uint64).max);
        assertEq(usdc.balanceOf(alice), 1_500_000 * USDC, "half a window refilled");
        vm.warp(block.timestamp + 2 days); // refill caps at the window limit
        assertEq(vault.availableToRelease(), 1_000_000 * USDC, "bucket capped");
        vault.release(key, 400_000 * USDC); // a smaller ask is honoured exactly
        vault.release(key, type(uint64).max);
        assertEq(usdc.balanceOf(alice), 2_500_000 * USDC, "paid in four parts");
        vm.expectRevert(PerpsVault.InvalidWithdrawal.selector);
        vault.release(key, 1);
        assertEq(vault.reserved(), 0, "reserved cleared");
        assertEq(vault.availableToRelease(), 1_000_000 * USDC - 1_000_000 * USDC, "bucket used by exactly what was paid");
    }

    function test_unregistered_release_refused() public {
        vm.expectRevert(PerpsVault.NotRegistered.selector);
        vault.release(keccak256("nothing"), 1);
    }

    // ================================================================== guardians
    function test_guardian_pause_blocks_payouts_and_deposits_only() public {
        PerpsVault.Withdrawal[] memory ws = _withdrawSetup(500 * USDC);
        vm.prank(gaddrs[0]);
        vault.guardianPause();
        assertTrue(vault.paused(), "paused");
        bytes32[] memory p = _wProof(ws, 0);
        bytes32 key = vault.withdraw(1, ws[0], p, type(uint64).max); // registers, pays nothing while paused
        assertEq(usdc.balanceOf(carol), 0, "nothing paid while paused");
        vm.expectRevert(PerpsVault.Paused.selector);
        vault.release(key, 1);
        usdc.mint(bob, 100 * USDC);
        vm.startPrank(bob);
        usdc.approve(address(vault), 100 * USDC);
        vm.expectRevert(PerpsVault.Paused.selector);
        vault.deposit(100 * USDC, bob);
        vm.stopPrank();
        // checkpoints and forced requests continue
        vm.prank(alice);
        vault.requestForced(0, 0);
        (, address[] memory a, uint64[] memory b) = _empty();
        a = new address[](1);
        b = new uint64[](1);
        a[0] = alice;
        b[0] = 1;
        vm.warp(block.timestamp + 1);
        _submit(_cp(2, ws, a, b));
        vm.warp(block.timestamp + 2 days);
        vault.release(key, type(uint64).max);
        assertEq(usdc.balanceOf(carol), 500 * USDC, "paid after the pause");
    }

    function test_guardian_pause_chain_is_bounded() public {
        vm.prank(bob);
        vm.expectRevert(PerpsVault.NotGuardian.selector);
        vault.guardianPause();
        uint256 start = block.timestamp;
        vm.prank(gaddrs[0]);
        vault.guardianPause();
        vm.prank(gaddrs[1]);
        vm.expectRevert(PerpsVault.InvalidPause.selector); // already paused
        vault.guardianPause();
        vm.warp(start + 2 days);
        vm.prank(gaddrs[0]);
        vm.expectRevert(PerpsVault.InvalidPause.selector); // once per guardian per chain
        vault.guardianPause();
        vm.prank(gaddrs[1]);
        vault.guardianPause();
        vm.warp(start + 4 days);
        vm.prank(gaddrs[2]);
        vault.guardianPause();
        vm.warp(start + 6 days);
        for (uint256 i; i < 3; ++i) {
            vm.prank(gaddrs[i]);
            vm.expectRevert(PerpsVault.InvalidPause.selector);
            vault.guardianPause();
        }
        assertTrue(!vault.paused(), "chain over after g * pauseDuration");
        vm.warp(start + 8 days); // cooldown passed: a new chain
        vm.prank(gaddrs[0]);
        vault.guardianPause();
    }

    function test_unpause_needs_unpause_threshold() public {
        vm.prank(gaddrs[0]);
        vault.guardianPause();
        uint64 count = vault.pauseCount();
        uint256 until = block.timestamp + 1 hours;
        bytes32 d = vault.unpauseDigest(count, until);
        bytes[] memory s5 = _sign(d, 5);
        vm.expectRevert(PerpsVault.InvalidCertificate.selector);
        vault.unpause(count, until, s5);
        vault.unpause(count, until, _sign(d, 6));
        assertTrue(!vault.paused(), "unpaused");
        vm.prank(gaddrs[1]);
        vm.expectRevert(PerpsVault.InvalidPause.selector); // guardians wait the cooldown
        vault.guardianPause();
        vm.expectRevert(PerpsVault.InvalidPause.selector); // replay
        vault.unpause(count, until, _sign(d, 6));
    }

    // ================================================================== forced requests and escape
    function test_forced_request_rules() public {
        vm.prank(alice);
        vm.expectRevert(PerpsVault.InvalidForced.selector); // never deposited
        vault.requestForced(0, 0);
        _deposit(alice, alice, 100 * USDC);
        vm.startPrank(alice);
        vm.expectRevert(PerpsVault.InvalidForced.selector);
        vault.requestForced(3, 0); // kind 3 does not exist
        vm.expectRevert(PerpsVault.InvalidForced.selector);
        vault.requestForced(255, 0);
        vm.expectRevert(PerpsVault.InvalidForced.selector); // amount only for withdraw
        vault.requestForced(0, 5);
        vm.expectRevert(PerpsVault.InvalidForced.selector); // market only for cancel/close
        vault.requestForced(1, 5);
        uint64 i = vault.requestForced(2, 50 * USDC);
        assertEq(i, 0, "index");
        vm.expectRevert(PerpsVault.InvalidForced.selector); // one open request per account
        vault.requestForced(0, 0);
        vm.stopPrank();
        bytes32 g = keccak256(abi.encode(bytes32("PosternPerps.forced.v1"), block.chainid, address(vault)));
        bytes32 h1 = keccak256(
            abi.encode(g, uint256(0), alice, uint256(2), uint256(block.timestamp + 2 days), uint256(50 * USDC))
        );
        assertEq(vault.forcedHead(1), h1, "forced chain");
        // processed by a checkpoint: a new request is allowed
        (PerpsVault.Withdrawal[] memory w, address[] memory a, uint64[] memory b) = _empty();
        _submit(_cp(1, w, a, b));
        vm.prank(alice);
        vault.requestForced(1, 0);
    }

    function test_overdue_forced_request_blocks_checkpoints_and_triggers_escape() public {
        _deposit(alice, alice, 100 * USDC);
        vm.prank(alice);
        vault.requestForced(1, 0);
        (PerpsVault.Withdrawal[] memory w, address[] memory a, uint64[] memory b) = _empty();
        PerpsVault.Checkpoint memory skip = _cp(1, w, a, b);
        skip.nextForcedIndex = 0;
        skip.forcedHead = vault.forcedHead(0);
        a = new address[](1);
        b = new uint64[](1);
        a[0] = alice;
        b[0] = 100 * USDC;
        skip.accountsRoot = _cp(1, w, a, b).accountsRoot;
        skip.totalEscape = 100 * USDC;
        // before the deadline a checkpoint may leave it pending
        _submit(skip);
        assertEq(vault.escapeReason(), 0, "not due");
        vm.expectRevert(PerpsVault.EscapeNotDue.selector);
        vault.triggerEscape();
        vm.warp(block.timestamp + 2 days + 1);
        PerpsVault.Checkpoint memory late = skip;
        late.height = 2;
        late.time = skip.time + 1;
        bytes[] memory s = _sign(vault.checkpointDigest(late), K);
        vm.expectRevert(PerpsVault.InvalidCheckpoint.selector);
        vault.submitCheckpoint(late, s);
        assertEq(vault.escapeReason(), 2, "overdue forced");
        vault.triggerEscape();
        assertTrue(vault.inEscape(), "escape");
    }

    function test_overdue_forced_request_processed_late_is_accepted() public {
        _deposit(alice, alice, 100 * USDC);
        vm.prank(alice);
        vault.requestForced(0, 0);
        vm.warp(block.timestamp + 3 days);
        (PerpsVault.Withdrawal[] memory w, address[] memory a, uint64[] memory b) = _empty();
        a = new address[](1);
        b = new uint64[](1);
        a[0] = alice;
        b[0] = 100 * USDC;
        _submit(_cp(1, w, a, b)); // processes it
        assertEq(vault.escapeReason(), 0, "no longer due");
    }

    function test_escape_after_silence_and_what_stops() public {
        _deposit(alice, alice, 1000 * USDC);
        // R395-24: before any checkpoint, silence never makes escape due (a vault deployed early is not bricked)
        vm.warp(block.timestamp + 30 days);
        assertEq(vault.escapeReason(), 0, "no silence escape before the first checkpoint");
        vm.expectRevert(PerpsVault.EscapeNotDue.selector);
        vault.triggerEscape();
        // ... but a depositor's own forced request still opens the exit if the committee never starts
        vm.prank(alice);
        vault.requestForced(2, 0);
        vm.warp(block.timestamp + 2 days + 1);
        assertEq(vault.escapeReason(), 2, "overdue forced request");
        vault.triggerEscape();
        vm.expectRevert(PerpsVault.EscapeActive.selector);
        vault.triggerEscape();
        (PerpsVault.Withdrawal[] memory w, address[] memory a, uint64[] memory b) = _empty();
        PerpsVault.Checkpoint memory cp = _cp(1, w, a, b);
        bytes[] memory s = _sign(vault.checkpointDigest(cp), K);
        vm.expectRevert(PerpsVault.EscapeActive.selector);
        vault.submitCheckpoint(cp, s);
        usdc.mint(bob, 100 * USDC);
        vm.startPrank(bob);
        usdc.approve(address(vault), 100 * USDC);
        vm.expectRevert(PerpsVault.EscapeActive.selector);
        vault.deposit(100 * USDC, bob);
        vm.stopPrank();
        vm.prank(alice);
        vm.expectRevert(PerpsVault.EscapeActive.selector);
        vault.requestForced(0, 0);
        // no checkpoint ever: the deposit is refunded from the vault's own chain, to the credited account
        vm.expectRevert(PerpsVault.InvalidRefund.selector);
        vault.refundDeposit(0, bob, 1000 * USDC);
        vm.expectRevert(PerpsVault.InvalidRefund.selector);
        vault.refundDeposit(0, alice, 1001 * USDC);
        vault.refundDeposit(0, alice, 1000 * USDC);
        assertEq(usdc.balanceOf(alice), 1000 * USDC, "refunded");
        vm.expectRevert(PerpsVault.InvalidRefund.selector);
        vault.refundDeposit(0, alice, 1000 * USDC);
    }

    function test_escape_claims_refunds_and_certified_withdrawals() public {
        _deposit(alice, alice, 1000 * USDC);
        _deposit(bob, bob, 600 * USDC);
        PerpsVault.Withdrawal[] memory ws = new PerpsVault.Withdrawal[](1);
        ws[0] = _w(0, bob, bob, 100 * USDC);
        address[] memory a = new address[](3);
        uint64[] memory b = new uint64[](3);
        a[0] = alice;
        a[1] = bob;
        a[2] = PROTOCOL_FEE;
        b[0] = 1200 * USDC; // alice won 200 from bob
        b[1] = 290 * USDC;
        b[2] = 10 * USDC;
        _submit(_cp(1, ws, a, b));
        _deposit(carol, carol, 50 * USDC); // after the checkpoint: uncredited
        vm.warp(block.timestamp + 7 days);
        vault.triggerEscape();
        _proveSys(a, b);
        assertEq(vault.escapePayable(), 1500 * USDC, "pool = custody - record - uncredited, equal to the total");
        // credited deposits are not refundable; a tampered refund is refused
        vm.expectRevert(PerpsVault.InvalidRefund.selector);
        vault.refundDeposit(0, alice, 1000 * USDC);
        vm.expectRevert(PerpsVault.InvalidRefund.selector);
        vault.refundDeposit(2, bob, 50 * USDC);
        vault.refundDeposit(2, carol, 50 * USDC);
        // escape claims
        vault.claimEscape(_acct(a, b, 0), _eProof(a, b, 0), type(uint64).max);
        bytes32[] memory p = _eProof(a, b, 1);
        PerpsVault.Account memory t = _acct(a, b, 1);
        t.escapeValue = 291 * USDC;
        vm.expectRevert(PerpsVault.InvalidProof.selector);
        vault.claimEscape(t, p, 1);
        t = _acct(a, b, 1);
        t.account = carol;
        vm.expectRevert(PerpsVault.InvalidProof.selector);
        vault.claimEscape(t, p, 1);
        t = _acct(a, b, 1);
        t.index = 0; // the leaf moved to another position
        vm.expectRevert(PerpsVault.InvalidProof.selector);
        vault.claimEscape(t, p, 1);
        vault.claimEscape(_acct(a, b, 1), p, type(uint64).max);
        vm.expectRevert(PerpsVault.AlreadyRegistered.selector);
        vault.claimEscape(_acct(a, b, 1), p, 1);
        // PROTOCOL_FEE's share pays the owner's fee address, never the keyless account
        vault.claimEscape(_acct(a, b, 2), _eProof(a, b, 2), type(uint64).max);
        // the certified withdrawal stays payable
        vault.withdraw(1, ws[0], _wProof(ws, 0), type(uint64).max);
        assertEq(usdc.balanceOf(alice), 1200 * USDC, "alice");
        assertEq(usdc.balanceOf(bob), 390 * USDC, "bob");
        assertEq(usdc.balanceOf(carol), 50 * USDC, "carol");
        assertEq(usdc.balanceOf(FEE), 10 * USDC, "fee");
        assertEq(usdc.balanceOf(PROTOCOL_FEE), 0, "nothing burned at 0x..01");
        assertEq(usdc.balanceOf(address(vault)), 0, "every unit paid, none left");
    }

    function test_escape_claim_not_in_normal_mode() public {
        bytes32[] memory p;
        PerpsVault.Account memory acc;
        vm.expectRevert(PerpsVault.NotEscape.selector);
        vault.claimEscape(acc, p, 1);
        vm.expectRevert(PerpsVault.NotEscape.selector);
        vault.refundDeposit(0, alice, 1);
        vm.expectRevert(PerpsVault.NotEscape.selector);
        vault.sweepEscapeSurplus();
    }

    function test_escape_claim_above_window_in_parts() public {
        _deposit(alice, alice, 2_500_000 * USDC);
        (PerpsVault.Withdrawal[] memory w,,) = _empty();
        address[] memory a = new address[](1);
        uint64[] memory b = new uint64[](1);
        a[0] = alice;
        b[0] = 2_500_000 * USDC;
        _submit(_cp(1, w, a, b));
        vm.warp(block.timestamp + 7 days);
        vault.triggerEscape();
        _proveSys(a, b);
        bytes32 key = vault.claimEscape(_acct(a, b, 0), _eProof(a, b, 0), 0);
        vm.warp(block.timestamp + 1 days); // large delay
        vault.release(key, 1_000_000 * USDC);
        vm.warp(block.timestamp + 1 days);
        vault.release(key, 1_000_000 * USDC);
        vm.warp(block.timestamp + 1 days);
        vault.release(key, 1_000_000 * USDC);
        assertEq(usdc.balanceOf(alice), 2_500_000 * USDC, "paid in parts");
    }

    /// The surplus sweep takes donations and rounding only, never before the system shares are proven, and every
    /// user claim stays payable after it; nothing is ever sent to a keyless address.
    function test_surplus_sweep_after_system_shares() public {
        _deposit(alice, alice, 1000 * USDC);
        _deposit(bob, bob, 1000 * USDC);
        (PerpsVault.Withdrawal[] memory w,,) = _empty();
        address[] memory a = new address[](2);
        uint64[] memory b = new uint64[](2);
        a[0] = alice;
        a[1] = bob;
        b[0] = 1500 * USDC;
        b[1] = 400 * USDC;
        _submit(_cp(1, w, a, b));
        usdc.mint(address(vault), 3 * USDC); // a donation
        vm.warp(block.timestamp + 7 days);
        vault.triggerEscape();
        vm.warp(block.timestamp + 90 days);
        vm.expectRevert(PerpsVault.InvalidSurplus.selector); // system shares not proven: the user pool is not fixed
        vault.sweepEscapeSurplus();
        _proveSys(a, b);
        vault.claimEscape(_acct(a, b, 0), _eProof(a, b, 0), type(uint64).max);
        uint64 swept = vault.sweepEscapeSurplus();
        assertEq(swept, 103 * USDC, "100 the tree never assigned, plus the donation");
        assertEq(usdc.balanceOf(FEE), 103 * USDC, "to the fee address");
        vm.expectRevert(PerpsVault.InvalidSurplus.selector);
        vault.sweepEscapeSurplus();
        vault.claimEscape(_acct(a, b, 1), _eProof(a, b, 1), type(uint64).max);
        assertEq(usdc.balanceOf(bob), 400 * USDC, "bob still paid after the sweep");
        assertEq(usdc.balanceOf(address(vault)), 0, "empty");
        assertEq(usdc.balanceOf(address(2)) + usdc.balanceOf(address(3)), 0, "nothing burned at keyless addresses");
    }

    function test_unclaimed_user_shares_never_swept() public {
        _deposit(alice, alice, 1000 * USDC);
        (PerpsVault.Withdrawal[] memory w,,) = _empty();
        address[] memory a = new address[](1);
        uint64[] memory b = new uint64[](1);
        a[0] = alice;
        b[0] = 1000 * USDC;
        _submit(_cp(1, w, a, b));
        vm.warp(block.timestamp + 7 days);
        vault.triggerEscape();
        _proveSys(a, b);
        vm.warp(block.timestamp + 90 days);
        vm.expectRevert(PerpsVault.InvalidSurplus.selector); // everything is still alice's
        vault.sweepEscapeSurplus();
        vault.claimEscape(_acct(a, b, 0), _eProof(a, b, 0), type(uint64).max);
        assertEq(usdc.balanceOf(alice), 1000 * USDC, "alice paid in full after 90 days");
    }

    function test_escape_with_zero_total_or_value_refused() public {
        _deposit(alice, alice, 1000 * USDC);
        (PerpsVault.Withdrawal[] memory w,,) = _empty();
        address[] memory a = new address[](1);
        uint64[] memory b = new uint64[](1);
        a[0] = alice;
        b[0] = 0;
        _submit(_cp(1, w, a, b));
        vm.warp(block.timestamp + 7 days);
        vault.triggerEscape();
        _proveSys(a, b);
        bytes32[] memory p = _eProof(a, b, 0);
        PerpsVault.Account memory acc = _acct(a, b, 0);
        vm.expectRevert(PerpsVault.InvalidWithdrawal.selector);
        vault.claimEscape(acc, p, 1);
    }

    function test_proof_must_have_depth_32() public {
        PerpsVault.Withdrawal[] memory ws = _withdrawSetup(500 * USDC);
        bytes32[] memory full = _wProof(ws, 0);
        bytes32[] memory short = new bytes32[](31);
        for (uint256 i; i < 31; ++i) short[i] = full[i];
        vm.expectRevert(PerpsVault.InvalidProof.selector);
        vault.withdraw(1, ws[0], short, 1);
        bytes32[] memory long = new bytes32[](33);
        for (uint256 i; i < 32; ++i) long[i] = full[i];
        vm.expectRevert(PerpsVault.InvalidProof.selector);
        vault.withdraw(1, ws[0], long, 1);
    }

    function test_reentrancy_through_token_refused() public {
        // A token that calls back into the vault on transfer cannot re-enter (one guard over every entry point).
        ReentrantToken t = new ReentrantToken();
        PerpsVault.Config memory c = _config();
        c.token = address(t);
        PerpsVault v = new PerpsVault(c);
        t.arm(address(v));
        t.mint(alice, 100 * USDC);
        vm.startPrank(alice);
        t.approve(address(v), 100 * USDC);
        vm.expectRevert(PerpsVault.InvalidTransfer.selector);
        v.deposit(100 * USDC, alice);
        vm.stopPrank();
    }

    // ================================================================== fuzz
    function testFuzz_tree_roundtrip(uint8 n, uint64 seed, uint8 pick) public view {
        uint256 count = uint256(n) % 33 + 1;
        bytes32[] memory h = new bytes32[](count);
        for (uint256 i; i < count; ++i) h[i] = keccak256(abi.encode(seed, i));
        uint64 idx = uint64(uint256(pick) % count);
        bytes32 root = Tree.root(h);
        assertEq(harness.rootX(h[idx], idx, Tree.proof(h, idx)), root, "root");
        if (count > 1) {
            uint64 other = (idx + 1) % uint64(count);
            assertTrue(harness.rootX(h[idx], other, Tree.proof(h, idx)) != root, "position bound");
        }
    }

    function testFuzz_withdraw_amount_tamper(uint64 amount, uint64 delta) public {
        amount = uint64(bound(amount, 1, 1_000_000 * USDC));
        delta = uint64(bound(delta, 1, 1e12));
        _deposit(alice, alice, 2_000_000 * USDC);
        PerpsVault.Withdrawal[] memory ws = new PerpsVault.Withdrawal[](2);
        ws[0] = _w(0, alice, alice, amount);
        ws[1] = _w(1, bob, bob, 1);
        (, address[] memory a, uint64[] memory b) = _empty();
        _submit(_cp(1, ws, a, b));
        bytes32[] memory p = _wProof(ws, 0);
        PerpsVault.Withdrawal memory w = _copyW(ws[0]);
        w.amount = amount + delta;
        vm.expectRevert(PerpsVault.InvalidProof.selector);
        vault.withdraw(1, w, p, 1);
    }

    /// Memory structs are references: tamper with a copy, never with the record the proof was built from.
    function _copyW(PerpsVault.Withdrawal memory w) internal pure returns (PerpsVault.Withdrawal memory) {
        return PerpsVault.Withdrawal(w.withdrawalId, w.account, w.recipient, w.amount, w.height);
    }

    function bound(uint256 x, uint256 lo, uint256 hi) internal pure returns (uint256) {
        return lo + x % (hi - lo + 1);
    }
}

contract ReentrantToken {
    uint8 public decimals = 6;
    mapping(address => uint256) public balanceOf;
    mapping(address => mapping(address => uint256)) public allowance;
    address public target;

    function arm(address t) external {
        target = t;
    }

    function mint(address to, uint256 v) external {
        balanceOf[to] += v;
    }

    function approve(address s, uint256 v) external returns (bool) {
        allowance[msg.sender][s] = v;
        return true;
    }

    function transferFrom(address f, address to, uint256 v) external returns (bool) {
        allowance[f][msg.sender] -= v;
        balanceOf[f] -= v;
        balanceOf[to] += v;
        // re-enter: the guard must refuse, which makes this call fail and the deposit revert
        PerpsVault(target).deposit(uint64(v), f);
        return true;
    }

    function transfer(address to, uint256 v) external returns (bool) {
        balanceOf[msg.sender] -= v;
        balanceOf[to] += v;
        return true;
    }
}

contract PerpsVaultGas is Base {
    /// Creation must fit the L2's per-transaction gas cap with a wide margin (bridge constraint: 5,000,000).
    function test_creation_gas_below_l2_cap() public {
        _setUpVault();
        PerpsVault.Config memory c = _config();
        uint256 g = gasleft();
        new PerpsVault(c);
        uint256 used = g - gasleft();
        assertTrue(used < 5_000_000, "creation above 5,000,000 gas");
        emit log_named_uint("creation gas (7 validators, 3 guardians)", used);
    }

    event log_named_uint(string key, uint256 val);
}

/// Test-only: exposes the vault's internal Merkle verifier (the deployed vault keeps it internal for size).
contract VaultHarness is PerpsVault {
    constructor(Config memory c) PerpsVault(c) {}

    function rootX(bytes32 leaf, uint64 index, bytes32[] calldata siblings) external pure returns (bytes32) {
        return _root(leaf, index, siblings);
    }
}

contract PerpsVaultSilenceTest is Base {
    function setUp() public {
        _setUpVault();
    }

    /// After the first accepted checkpoint, `escapeDelay` without another makes escape due.
    function test_escape_by_silence_after_first_checkpoint() public {
        address u = address(0x1000000000000000000000000000000000000001);
        _deposit(u, u, 100 * USDC);
        (PerpsVault.Withdrawal[] memory w,,) = _empty();
        address[] memory a = new address[](1);
        uint64[] memory b = new uint64[](1);
        a[0] = u;
        b[0] = 100 * USDC;
        _submit(_cp(1, w, a, b));
        vm.warp(block.timestamp + 7 days - 1);
        vm.expectRevert(PerpsVault.EscapeNotDue.selector);
        vault.triggerEscape();
        vm.warp(block.timestamp + 1);
        assertEq(vault.escapeReason(), 1, "silence");
        vault.triggerEscape();
        _proveSys(a, b);
        vault.claimEscape(_acct(a, b, 0), _eProof(a, b, 0), type(uint64).max);
        assertEq(usdc.balanceOf(u), 100 * USDC, "paid");
    }

    /// Deposits for keyless accounts (precompiles, PROTOCOL_FEE, the zero address) are refused: nothing could ever
    /// refund them. Restated in round 2 (vault v0.3): INSURANCE (0x..02) and BACKSTOP (0x..03) left this list
    /// because the vault now records their funder and refunds an uncredited system deposit to it
    /// (SystemFunding.t.sol); the property (no value stranded for a keyless address) is unchanged, and the loop
    /// below still covers 0x..00, 0x..01 and now also 0x..04 to 0x..0b.
    function test_keyless_deposit_refused() public {
        address u = address(0x1000000000000000000000000000000000000001);
        usdc.mint(u, 100 * USDC);
        vm.startPrank(u);
        usdc.approve(address(vault), 100 * USDC);
        for (uint160 k; k < 12; ++k) {
            if (k == 2 || k == 3) continue;
            vm.expectRevert(PerpsVault.InvalidDeposit.selector);
            vault.deposit(10 * USDC, address(k));
        }
        vm.expectRevert(PerpsVault.InvalidDeposit.selector);
        vault.deposit(10 * USDC, address(0xffff));
        vault.deposit(10 * USDC, address(0x10000));
        vm.stopPrank();
    }
}

/// R395-33 (coordinator decision): in escape, INSURANCE and BACKSTOP shares cover user shortfalls first; only the
/// remainder goes to the fee address, and only after the 90-day window.
contract PerpsVaultSubordinationTest is Base {
    function setUp() public {
        _setUpVault();
    }

    function _scenario() internal returns (address[] memory a, uint64[] memory b) {
        address alice = address(uint160(uint256(keccak256("alice"))));
        address bob = address(uint160(uint256(keccak256("bob"))));
        _deposit(alice, alice, 1000 * USDC);
        _deposit(bob, bob, 1000 * USDC);
        // SPEC 8 order: PROTOCOL_FEE index 0, INSURANCE 1, BACKSTOP 2, users after
        a = new address[](5);
        b = new uint64[](5);
        a[0] = address(1);
        a[1] = address(2);
        a[2] = address(3);
        a[3] = alice;
        a[4] = bob;
        b[0] = 0;
        b[1] = 250 * USDC; // insurance
        b[2] = 50 * USDC; // backstop
        b[3] = 1500 * USDC;
        b[4] = 400 * USDC; // users 1900 <= pool 2000 < total 2200
        (PerpsVault.Withdrawal[] memory w,,) = _empty();
        _submit(_cp(1, w, a, b));
        vm.warp(block.timestamp + 7 days);
        vault.triggerEscape();
    }

    function test_R395_33_insurance_covers_users_first() public {
        (address[] memory a, uint64[] memory b) = _scenario();
        vault.proveSystemShares(_acct(a, b, 1), _eProof(a, b, 1), _acct(a, b, 2), _eProof(a, b, 2));
        vault.claimEscape(_acct(a, b, 3), _eProof(a, b, 3), type(uint64).max);
        vault.claimEscape(_acct(a, b, 4), _eProof(a, b, 4), type(uint64).max);
        assertEq(usdc.balanceOf(a[3]), 1500 * USDC, "alice paid in full");
        assertEq(usdc.balanceOf(a[4]), 400 * USDC, "bob paid in full");
        bytes32 sub = vault.escapeKey(address(2));
        vm.expectRevert(PerpsVault.NotReady.selector);
        vault.release(sub, type(uint64).max);
        vm.warp(block.timestamp + 90 days);
        vault.release(sub, type(uint64).max);
        assertEq(usdc.balanceOf(FEE), 100 * USDC, "only the remainder, after 90 days");
        assertEq(usdc.balanceOf(address(vault)), 0, "empty");
    }

    function test_R395_33_user_claims_wait_for_system_shares() public {
        (address[] memory a, uint64[] memory b) = _scenario();
        PerpsVault.Account memory u = _acct(a, b, 3);
        bytes32[] memory p = _eProof(a, b, 3);
        vm.expectRevert(PerpsVault.NotReady.selector);
        vault.claimEscape(u, p, 1);
        // keyless subordinated accounts cannot claim directly
        PerpsVault.Account memory ins = _acct(a, b, 1);
        bytes32[] memory pi = _eProof(a, b, 1);
        vault.proveSystemShares(ins, pi, _acct(a, b, 2), _eProof(a, b, 2));
        vm.expectRevert(PerpsVault.InvalidWithdrawal.selector);
        vault.claimEscape(ins, pi, 1);
        vm.expectRevert(PerpsVault.AlreadyRegistered.selector);
        vault.proveSystemShares(ins, pi, _acct(a, b, 2), _eProof(a, b, 2));
    }

    function test_R395_33_shortfall_beyond_insurance_is_pro_rata_and_fee_gets_nothing() public {
        address alice = address(uint160(uint256(keccak256("alice"))));
        _deposit(alice, alice, 1000 * USDC);
        address[] memory a = new address[](4);
        uint64[] memory b = new uint64[](4);
        a[0] = address(1);
        a[1] = address(2);
        a[2] = address(3);
        a[3] = alice;
        b[1] = 100 * USDC;
        b[3] = 1250 * USDC; // users alone exceed the pool of 1000
        (PerpsVault.Withdrawal[] memory w,,) = _empty();
        _submit(_cp(1, w, a, b));
        vm.warp(block.timestamp + 7 days);
        vault.triggerEscape();
        vault.proveSystemShares(_acct(a, b, 1), _eProof(a, b, 1), _acct(a, b, 2), _eProof(a, b, 2));
        vault.claimEscape(_acct(a, b, 3), _eProof(a, b, 3), type(uint64).max);
        assertEq(usdc.balanceOf(alice), 1000 * USDC, "the whole pool to the user");
        (, uint64 total,,) = vault.payouts(vault.escapeKey(address(2)));
        assertEq(total, 0, "no subordinated payout registered");
    }
}

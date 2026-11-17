// SPDX-License-Identifier: UNLICENSED
pragma solidity 0.8.24;

import {PerpsVault} from "../src/PerpsVault.sol";

/// Minimal 6-decimal token for the demonstrations.
contract Tok {
    mapping(address => uint256) public balanceOf;
    mapping(address => mapping(address => uint256)) public allowance;
    function decimals() external pure returns (uint256) { return 6; }
    function mint(address to, uint256 a) external { balanceOf[to] += a; }
    function approve(address s, uint256 a) external returns (bool) { allowance[msg.sender][s] = a; return true; }
    function transfer(address to, uint256 a) external returns (bool) { balanceOf[msg.sender] -= a; balanceOf[to] += a; return true; }
    function transferFrom(address f, address to, uint256 a) external returns (bool) {
        allowance[f][msg.sender] -= a; balanceOf[f] -= a; balanceOf[to] += a; return true;
    }
}

interface Vm {
    function warp(uint256) external;
    function addr(uint256) external returns (address);
    function sign(uint256, bytes32) external returns (uint8, bytes32, bytes32);
    function prank(address) external;
    function expectRevert(bytes4) external;
}

contract R395VaultTest {
    Vm constant vm = Vm(address(uint160(uint256(keccak256("hevm cheat code")))));
    Tok tok;
    PerpsVault vault;
    uint256[] keys;
    address user = address(0xBEEF);

    function setUp() public {
        tok = new Tok();
        // three validator keys, sorted by address
        uint256[3] memory k = [uint256(0xA11CE), 0xB0B, 0xC0FFEE];
        for (uint256 i; i < 3; ++i) keys.push(k[i]);
        for (uint256 i; i < 3; ++i) for (uint256 j = i + 1; j < 3; ++j) {
            if (vm.addr(keys[j]) < vm.addr(keys[i])) (keys[i], keys[j]) = (keys[j], keys[i]);
        }
        address[] memory vals = new address[](3);
        for (uint256 i; i < 3; ++i) vals[i] = vm.addr(keys[i]);
        PerpsVault.Config memory c;
        c.token = address(tok);
        c.feeRecipient = 0x775a993F09Bd80aeCF8F8c2ADe0C84158E3a0544;
        c.validators = vals;
        c.threshold = 2;
        c.unpauseThreshold = 2;
        c.cap = 1e15;
        c.minDeposit = 1e6;
        c.windowLimit = 1e12;
        c.windowSeconds = 1 days;
        c.largeThreshold = 1e11;
        c.largeDelay = 1 days;
        c.forcedDelay = 1 hours;
        c.escapeDelay = 1 days;
        vault = new PerpsVault(c);
        tok.mint(user, 1e12);
        vm.prank(user);
        tok.approve(address(vault), type(uint256).max);
    }

    function _sigs(bytes32 d) internal returns (bytes[] memory s) {
        s = new bytes[](2);
        for (uint256 i; i < 2; ++i) {
            (uint8 v, bytes32 r, bytes32 ss) = vm.sign(keys[i], d);
            s[i] = abi.encodePacked(r, ss, v);
        }
    }

    /// R395-24: before the engine's first checkpoint, anyone can put a freshly deployed vault into (irreversible)
    /// escape once `escapeDelay` has passed since DEPLOYMENT; deposits are then refused for ever.
    function test_R395_24_vault_bricked_before_first_checkpoint() public {
        vm.warp(block.timestamp + 1 days);
        vault.triggerEscape(); // passes: reason 1, lastCheckpointAt was set in the constructor
        require(vault.inEscape(), "escape");
        vm.prank(user);
        vm.expectRevert(PerpsVault.EscapeActive.selector);
        vault.deposit(5e6, user);
    }

    /// R395-23: a checkpoint whose escape total exceeds custody by one unit is refused (`Insolvent`). With SPEC 11's
    /// escapeValue = max(0, equity), ONE account below zero equity at the checkpoint height (gap, or a halted market
    /// where S-4.10 stops liquidations) makes sum(escapeValue) > custody, so every checkpoint is refused until it is
    /// cured, withdrawals stop, and after escapeDelay anyone triggers escape.
    function test_R395_23_negative_equity_account_blocks_checkpoints() public {
        vm.prank(user);
        vault.deposit(100e6, user); // custody 100 USDC: user A
        // Engine view: A long, B short, B at -10 USDC equity not yet liquidated; A's equity = 110 (100 + B's loss).
        // sum(max(0, E)) = 110 + 0 = 110 > custody 100.
        PerpsVault.Checkpoint memory cp;
        cp.height = 1;
        cp.time = 1;
        cp.stateRoot = keccak256("s");
        cp.logHash = keccak256("l");
        cp.depositsCredited = 1;
        cp.depositsHead = vault.depositHead(1);
        cp.forcedHead = vault.forcedHead(0);
        cp.escapeTotal = 110e6;
        cp.escapeRoot = keccak256("e");
        bytes[] memory s = _sigs(vault.checkpointDigest(cp));
        vm.expectRevert(PerpsVault.Insolvent.selector);
        vault.submitCheckpoint(cp, s);
        // The same checkpoint with escape total = custody passes: the refusal is the solvency rule, nothing else.
        cp.escapeTotal = 100e6;
        vault.submitCheckpoint(cp, _sigs(vault.checkpointDigest(cp)));
        require(vault.checkpointCount() == 1, "accepted");
    }
}

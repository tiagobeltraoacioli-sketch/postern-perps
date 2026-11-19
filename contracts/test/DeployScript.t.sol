// SPDX-License-Identifier: UNLICENSED
// Copyright (c) 2026 Bloch Inc.
pragma solidity 0.8.24;

import {PerpsVault} from "../src/PerpsVault.sol";
import {DeployPerpsVault} from "../script/DeployPerpsVault.s.sol";
import {Base} from "./Base.sol";

contract CheckHarness is DeployPerpsVault {
    function check(PerpsVault v, PerpsVault.Config memory c) external {
        _check(v, c);
    }

    function checkToken(address token, bytes32 pinned) external view {
        _checkToken(token, pinned);
    }
}

/// Answers every getter the post gate reads with the configured values, and is not a vault: it has a `drain`.
contract ImpostorVault {
    PerpsVault.Config internal c;

    constructor(PerpsVault.Config memory config) {
        c = config;
    }

    function drain(address token, address to) external {
        (bool ok,) = token.call(abi.encodeWithSignature("transfer(address,uint256)", to, type(uint256).max));
        ok;
    }

    function token() external view returns (address) { return c.token; }
    function feeRecipient() external view returns (address) { return c.feeRecipient; }
    function threshold() external view returns (uint8) { return c.threshold; }
    function unpauseThreshold() external view returns (uint8) { return c.unpauseThreshold; }
    function cap() external view returns (uint64) { return c.cap; }
    function minDeposit() external view returns (uint64) { return c.minDeposit; }
    function windowLimit() external view returns (uint64) { return c.windowLimit; }
    function windowSeconds() external view returns (uint64) { return c.windowSeconds; }
    function largeThreshold() external view returns (uint64) { return c.largeThreshold; }
    function largeDelay() external view returns (uint64) { return c.largeDelay; }
    function forcedDelay() external view returns (uint64) { return c.forcedDelay; }
    function escapeDelay() external view returns (uint64) { return c.escapeDelay; }
    function pauseDuration() external view returns (uint64) { return c.pauseDuration; }
    function pauseCooldown() external view returns (uint64) { return c.pauseCooldown; }
    function validatorCount() external view returns (uint256) { return c.validators.length; }
    function guardianCount() external view returns (uint256) { return c.guardians.length; }
    function validators(uint256 i) external view returns (address) { return c.validators[i]; }
    function guardians(uint256 i) external view returns (address) { return c.guardians[i]; }
    function checkpointCount() external pure returns (uint64) { return 0; }
    function depositCount() external pure returns (uint64) { return 0; }
    function inEscape() external pure returns (bool) { return false; }
}

/// The owner-run script's post gate (`MODE=check` and after a deployment).
contract DeployScriptTest is Base {
    CheckHarness internal harness;

    function setUp() public {
        _setUpVault();
        harness = new CheckHarness();
    }

    function test_post_gate_accepts_the_real_vault() public {
        harness.check(vault, _config());
    }

    /// R436-9 (Low, reviewer 436): the post gate compared getters only, so a contract that ANSWERS like the vault
    /// passed `MODE=check`. It now also requires the runtime code of the address to equal the code this build gives
    /// for this config (a twin created in the script's simulation, never broadcast). Failing first on 17edfc8: the
    /// impostor passes the gate.
    function test_R436_9_post_gate_refuses_a_contract_that_only_answers_like_the_vault() public {
        ImpostorVault impostor = new ImpostorVault(_config());
        (bool ok,) =
            address(harness).call(abi.encodeWithSelector(CheckHarness.check.selector, address(impostor), _config()));
        assertTrue(!ok, "an impostor with matching getters passed the post gate");
    }

    /// The code check covers the immutables: the real vault checked against a config that differs in one limit is
    /// refused by the code comparison already (and by the getter comparison after it).
    function test_post_gate_refuses_another_config() public {
        PerpsVault.Config memory c = _config();
        c.largeDelay += 1;
        (bool ok,) = address(harness).call(abi.encodeWithSelector(CheckHarness.check.selector, address(vault), c));
        assertTrue(!ok, "a vault with another limit passed");
        c = _config();
        c.validators[6] = address(uint160(c.validators[6]) + 1); // storage, not code: caught by the getters
        (ok,) = address(harness).call(abi.encodeWithSelector(CheckHarness.check.selector, address(vault), c));
        assertTrue(!ok, "a vault with another committee passed");
    }

    /// Bridge pattern (custody deployment preflight): the collateral token is identified by its runtime code hash,
    /// pinned in the config, not by its address and `decimals()` alone.
    function test_token_code_pin() public {
        harness.checkToken(address(usdc), address(usdc).codehash);
        (bool ok,) = address(harness).staticcall(
            abi.encodeWithSelector(CheckHarness.checkToken.selector, address(usdc), bytes32(uint256(1)))
        );
        assertTrue(!ok, "a token with another runtime code passed");
        (ok,) = address(harness).staticcall(
            abi.encodeWithSelector(CheckHarness.checkToken.selector, address(0xdead), address(usdc).codehash)
        );
        assertTrue(!ok, "an address without code passed");
    }
}

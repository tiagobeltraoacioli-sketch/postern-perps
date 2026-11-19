// SPDX-License-Identifier: UNLICENSED
// Copyright (c) 2026 Bloch Inc.
pragma solidity 0.8.24;

/// @title MockBUSDC: LOCAL DEVELOPMENT stand-in for bUSDC (6 decimals). NEVER deployed to a real network.
/// @notice PROTOTYPE, UNAUDITED. Anyone can mint (it is a faucet): it has no value by construction. It has the
///         transfer semantics of the bridge's `BridgedStablecoinV3` that the vault relies on: returns true, reverts on
///         failure, exact amounts, no fee, no transfer pause, no blacklist. Used only by `script/local_deploy.py`
///         (anvil) and by the tests; `script/DeployPerpsVault.s.sol` never deploys it.
contract MockBUSDC {
    string public constant name = "Mock bUSDC (local dev, no value)";
    string public constant symbol = "bUSDC";
    uint8 public constant decimals = 6;
    /// One faucet call cannot exceed this (keeps totals far inside uint64, the vault's amount type).
    uint256 public constant MAX_MINT = 10_000_000e6;

    uint256 public totalSupply;
    mapping(address => uint256) public balanceOf;
    mapping(address => mapping(address => uint256)) public allowance;

    event Transfer(address indexed from, address indexed to, uint256 value);
    event Approval(address indexed owner, address indexed spender, uint256 value);

    error InvalidMint();
    error InsufficientBalance();
    error InsufficientAllowance();
    error InvalidRecipient();

    /// @notice Open faucet (local development only).
    function mint(address to, uint256 value) external {
        if (to == address(0) || value == 0 || value > MAX_MINT) revert InvalidMint();
        totalSupply += value;
        balanceOf[to] += value;
        emit Transfer(address(0), to, value);
    }

    function approve(address spender, uint256 value) external returns (bool) {
        allowance[msg.sender][spender] = value;
        emit Approval(msg.sender, spender, value);
        return true;
    }

    function transfer(address to, uint256 value) external returns (bool) {
        _move(msg.sender, to, value);
        return true;
    }

    function transferFrom(address from, address to, uint256 value) external returns (bool) {
        uint256 allowed = allowance[from][msg.sender];
        if (allowed < value) revert InsufficientAllowance();
        if (allowed != type(uint256).max) allowance[from][msg.sender] = allowed - value;
        _move(from, to, value);
        return true;
    }

    function _move(address from, address to, uint256 value) private {
        if (to == address(0)) revert InvalidRecipient();
        uint256 balance = balanceOf[from];
        if (balance < value) revert InsufficientBalance();
        unchecked {
            balanceOf[from] = balance - value;
        }
        balanceOf[to] += value;
        emit Transfer(from, to, value);
    }
}

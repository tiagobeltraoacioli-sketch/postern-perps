// SPDX-License-Identifier: UNLICENSED
pragma solidity 0.8.24;

import {PerpsVault} from "../src/PerpsVault.sol";

interface ScriptVm {
    function envString(string calldata name) external view returns (string memory);
    function envOr(string calldata name, string calldata defaultValue) external view returns (string memory);
    function readFile(string calldata path) external view returns (string memory);
    function parseJsonString(string calldata json, string calldata key) external pure returns (string memory);
    function parseJsonStringArray(string calldata json, string calldata key) external pure returns (string[] memory);
    function parseJsonUint(string calldata json, string calldata key) external pure returns (uint256);
    function parseJsonBytes32(string calldata json, string calldata key) external pure returns (bytes32);
    function toString(bytes32 v) external pure returns (string memory);
    function parseAddress(string calldata s) external pure returns (address);
    function startBroadcast() external;
    function stopBroadcast() external;
    function toString(address a) external pure returns (string memory);
    function toString(uint256 v) external pure returns (string memory);
}

/// Owner-run deployment of PerpsVault (PROTOTYPE, UNAUDITED: not for deployment before an audit and legal review).
///
///   VAULT_CONFIG=deploy/vault.config.json forge script script/DeployPerpsVault.s.sol --rpc-url <L2 RPC> --ledger --broadcast
///
/// The script refuses to run unless: the config has no `TBD` value, the RPC's chain id equals the config's, the fee
/// recipient is the owner's fixed address, the token's runtime code hash equals the pinned `tokenCodehash`, and
/// (`MODE=check`, read-only, and after every deployment) the vault's runtime code equals the code this build gives for
/// this config (R436-9) and its immutables and committee equal the config. It prints every value before the
/// transaction so the owner can compare it with the config file.
contract DeployPerpsVault {
    ScriptVm internal constant vm = ScriptVm(address(uint160(uint256(keccak256("hevm cheat code")))));
    address internal constant OWNER_FEE_ADDRESS = 0x775a993F09Bd80aeCF8F8c2ADe0C84158E3a0544;

    event log(string);

    function run() external returns (address deployed) {
        string memory json = vm.readFile(vm.envOr("VAULT_CONFIG", "deploy/vault.config.json"));
        PerpsVault.Config memory c = _read(json);
        require(block.chainid == vm.parseJsonUint(json, ".chainId"), "RPC chain id differs from the config");
        require(c.feeRecipient == OWNER_FEE_ADDRESS, "fee recipient is not the owner's fixed address");
        // Bridge pattern: the collateral token is pinned by its runtime code hash (a `TBD` value does not parse).
        bytes32 tokenCodehash = vm.parseJsonBytes32(json, ".tokenCodehash");
        _checkToken(c.token, tokenCodehash);
        _print(c);
        emit log(string.concat("token runtime code hash ", vm.toString(tokenCodehash)));
        string memory mode = vm.envOr("MODE", "deploy");
        if (keccak256(bytes(mode)) == keccak256("check")) {
            deployed = vm.parseAddress(vm.envString("VAULT"));
            _check(PerpsVault(deployed), c);
            emit log("post gate: deployed vault equals the config");
            return deployed;
        }
        require(keccak256(bytes(mode)) == keccak256("deploy"), "MODE must be deploy or check");
        vm.startBroadcast();
        deployed = address(new PerpsVault(c));
        vm.stopBroadcast();
        _check(PerpsVault(deployed), c);
        emit log(string.concat("deployed PerpsVault at ", vm.toString(deployed)));
    }

    function _addr(string memory s) internal pure returns (address) {
        bytes memory b = bytes(s);
        require(b.length == 42 && b[0] == "0" && b[1] == "x", string.concat("not an address (TBD?): ", s));
        return vm.parseAddress(s);
    }

    function _addrs(string memory json, string memory key) internal pure returns (address[] memory out) {
        string[] memory raw = vm.parseJsonStringArray(json, key);
        out = new address[](raw.length);
        for (uint256 i; i < raw.length; ++i) out[i] = _addr(raw[i]);
    }

    function _u64(string memory json, string memory key) internal pure returns (uint64) {
        uint256 v = vm.parseJsonUint(json, key);
        require(v <= type(uint64).max, key);
        return uint64(v);
    }

    function _read(string memory json) internal pure returns (PerpsVault.Config memory c) {
        c.token = _addr(vm.parseJsonString(json, ".token"));
        c.feeRecipient = _addr(vm.parseJsonString(json, ".feeRecipient"));
        c.validators = _addrs(json, ".validators");
        c.threshold = uint8(_u64(json, ".threshold"));
        c.unpauseThreshold = uint8(_u64(json, ".unpauseThreshold"));
        c.guardians = _addrs(json, ".guardians");
        c.pauseDuration = _u64(json, ".pauseDuration");
        c.pauseCooldown = _u64(json, ".pauseCooldown");
        c.cap = _u64(json, ".cap");
        c.minDeposit = _u64(json, ".minDeposit");
        c.windowLimit = _u64(json, ".windowLimit");
        c.windowSeconds = _u64(json, ".windowSeconds");
        c.largeThreshold = _u64(json, ".largeThreshold");
        c.largeDelay = _u64(json, ".largeDelay");
        c.forcedDelay = _u64(json, ".forcedDelay");
        c.escapeDelay = _u64(json, ".escapeDelay");
    }

    function _print(PerpsVault.Config memory c) internal {
        emit log(string.concat("chain id ", vm.toString(block.chainid)));
        emit log(string.concat("token ", vm.toString(c.token)));
        emit log(string.concat("feeRecipient ", vm.toString(c.feeRecipient)));
        for (uint256 i; i < c.validators.length; ++i) emit log(string.concat("validator ", vm.toString(c.validators[i])));
        emit log(string.concat("threshold ", vm.toString(c.threshold), " unpauseThreshold ", vm.toString(c.unpauseThreshold)));
        for (uint256 i; i < c.guardians.length; ++i) emit log(string.concat("guardian ", vm.toString(c.guardians[i])));
        emit log(string.concat("cap ", vm.toString(c.cap), " minDeposit ", vm.toString(c.minDeposit)));
        emit log(string.concat("window ", vm.toString(c.windowLimit), " per ", vm.toString(c.windowSeconds), " s"));
        emit log(string.concat("large ", vm.toString(c.largeThreshold), " delay ", vm.toString(c.largeDelay), " s"));
        emit log(string.concat("forcedDelay ", vm.toString(c.forcedDelay), " escapeDelay ", vm.toString(c.escapeDelay)));
        emit log(string.concat("pause ", vm.toString(c.pauseDuration), " cooldown ", vm.toString(c.pauseCooldown)));
    }

    function _checkToken(address token, bytes32 pinned) internal view {
        require(token.code.length != 0, "no code at the token address");
        require(token.codehash == pinned, "token runtime code differs from the pinned tokenCodehash");
    }

    function _check(PerpsVault v, PerpsVault.Config memory c) internal {
        require(address(v).code.length != 0, "no code at the vault address");
        // R436-9: the code itself, not only what it answers. A twin built here from the same config has the same
        // runtime code (immutables included; no metadata hash in this build). The twin exists only in the script's
        // simulation: it is created outside startBroadcast/stopBroadcast and is never sent.
        require(
            address(v).codehash == address(new PerpsVault(c)).codehash,
            "runtime code differs from this build with this config"
        );
        require(v.token() == c.token && v.feeRecipient() == c.feeRecipient, "token / fee recipient");
        require(v.threshold() == c.threshold && v.unpauseThreshold() == c.unpauseThreshold, "thresholds");
        require(v.cap() == c.cap && v.minDeposit() == c.minDeposit, "cap / min deposit");
        require(v.windowLimit() == c.windowLimit && v.windowSeconds() == c.windowSeconds, "window");
        require(v.largeThreshold() == c.largeThreshold && v.largeDelay() == c.largeDelay, "large");
        require(v.forcedDelay() == c.forcedDelay && v.escapeDelay() == c.escapeDelay, "forced / escape");
        require(v.pauseDuration() == c.pauseDuration && v.pauseCooldown() == c.pauseCooldown, "pause");
        require(v.validatorCount() == c.validators.length && v.guardianCount() == c.guardians.length, "set sizes");
        for (uint256 i; i < c.validators.length; ++i) require(v.validators(i) == c.validators[i], "validator");
        for (uint256 i; i < c.guardians.length; ++i) require(v.guardians(i) == c.guardians[i], "guardian");
        require(v.checkpointCount() == 0 && v.depositCount() == 0 && !v.inEscape(), "fresh vault expected");
    }
}

// SPDX-License-Identifier: UNLICENSED
// Copyright (c) 2026 Bloch Inc.
pragma solidity 0.8.24;

import {PerpsVault} from "../src/PerpsVault.sol";
import {Base} from "./Base.sol";
import {VaultHarness} from "./PerpsVault.t.sol";
import {JsonVm} from "./EngineVectors.t.sol";

/// Agent 430's own proof vectors, the output of the engine CLI (`engine proofs-example` / `engine prove`), copied
/// unchanged from `engine/vectors/proofs-example.json` at `codex/perps-engine-r2-20261118` `cd8e5ae` (SPEC 0.2.2) to
/// `test/vectors/engine-cli/`: every account and withdrawal proof the CLI prints verifies with the vault's own Merkle
/// walk against the roots of the CLI's checkpoint, every leaf has the vault's byte format, the vault's state-root
/// equation gives the CLI's state root, and the escape values sum to `totalEscape` and to custody. (The checkpoint in
/// that file is unsigned and made for the testkit vault address, so acceptance by a deployed vault is shown with the
/// generator's stories in EngineVectors.t.sol, not here.)
contract EngineCliProofsTest is Base {
    JsonVm internal constant jvm = JsonVm(address(uint160(uint256(keccak256("hevm cheat code")))));
    string internal j;
    VaultHarness internal h;

    function setUp() public {
        _setUpVault();
        h = new VaultHarness(_config());
        j = jvm.readFile("test/vectors/engine-cli/proofs-example.json");
    }

    function _u(string memory k) internal view returns (uint256) {
        return jvm.parseJsonUint(j, k);
    }

    function _cp() internal view returns (PerpsVault.Checkpoint memory cp) {
        cp.height = uint64(_u(".checkpoint.height"));
        cp.time = uint64(_u(".checkpoint.time"));
        cp.logHash = jvm.parseJsonBytes32(j, ".checkpoint.logHash");
        cp.accountsRoot = jvm.parseJsonBytes32(j, ".checkpoint.accountsRoot");
        cp.marketsRoot = jvm.parseJsonBytes32(j, ".checkpoint.marketsRoot");
        cp.withdrawalsRoot = jvm.parseJsonBytes32(j, ".checkpoint.withdrawalsRoot");
        cp.nextDepositIndex = uint64(_u(".checkpoint.nextDepositIndex"));
        cp.nextForcedIndex = uint64(_u(".checkpoint.nextForcedIndex"));
        cp.nextOrderId = uint64(_u(".checkpoint.nextOrderId"));
        cp.totalEscape = uint128(_u(".checkpoint.totalEscape"));
        cp.dust = uint128(_u(".checkpoint.dust"));
        cp.auxHash = jvm.parseJsonBytes32(j, ".checkpoint.auxHash");
        cp.depositsHead = jvm.parseJsonBytes32(j, ".checkpoint.depositsHead");
        cp.forcedHead = jvm.parseJsonBytes32(j, ".checkpoint.forcedHead");
        cp.withdrawalCount = uint64(_u(".checkpoint.withdrawalCount"));
        cp.withdrawalsTotal = uint64(_u(".checkpoint.withdrawalsTotal"));
    }

    function test_engine_cli_proofs_verify_with_the_vaults_merkle_walk() public view {
        PerpsVault.Checkpoint memory cp = _cp();
        assertEq(h.stateRootOf(cp), jvm.parseJsonBytes32(j, ".stateRoot"), "state root (engine CLI) != vault equation");

        uint256 escapeSum;
        uint256 accounts = 6; // the file's rehearsal; a missing or extra account breaks the sum below
        for (uint256 i; i < accounts; ++i) {
            string memory p = string.concat(".accounts[", jvm.toString(i), "]");
            bytes memory leafData = jvm.parseJsonBytes(j, string.concat(p, ".leafData"));
            uint64 escapeValue = uint64(_u(string.concat(p, ".escapeValue")));
            uint32 index = uint32(_u(string.concat(p, ".index")));
            assertEq(index, i, "account index is its position");
            assertEq(
                keccak256(leafData),
                keccak256(
                    abi.encodePacked(
                        "PPv1acct",
                        jvm.parseJsonAddress(j, string.concat(p, ".account")),
                        index,
                        int128(jvm.parseJsonInt(j, string.concat(p, ".balance"))),
                        escapeValue,
                        jvm.parseJsonBytes32(j, string.concat(p, ".positionsHash"))
                    )
                ),
                "account leaf bytes"
            );
            bytes32 leaf = keccak256(abi.encodePacked(bytes1(0x00), leafData));
            assertEq(leaf, jvm.parseJsonBytes32(j, string.concat(p, ".leafHash")), "account leaf hash");
            bytes32[] memory sib = jvm.parseJsonBytes32Array(j, string.concat(p, ".siblings"));
            assertEq(h.rootX(leaf, index, sib), cp.accountsRoot, "account proof");
            // the same proof at the neighbouring position does not verify
            assertTrue(h.rootX(leaf, index ^ 1, sib) != cp.accountsRoot, "position is bound");
            escapeSum += escapeValue;
        }
        assertEq(escapeSum, cp.totalEscape, "sum of escape values == totalEscape");
        assertEq(escapeSum, _u(".custody"), "sum of escape values == custody (insurance first)");
        // the under-water account of the rehearsal is valued at zero and INSURANCE below its balance
        assertEq(_u(".accounts[5].escapeValue"), 0, "under-water account: escape value 0");
        assertTrue(_u(".accounts[1].escapeValue") < _u(".accounts[1].balance"), "INSURANCE carries the deficit");

        uint256 total;
        for (uint256 i; i < cp.withdrawalCount; ++i) {
            string memory p = string.concat(".withdrawals[", jvm.toString(i), "]");
            bytes memory leafData = jvm.parseJsonBytes(j, string.concat(p, ".leafData"));
            uint64 amount = uint64(_u(string.concat(p, ".amount")));
            assertEq(
                keccak256(leafData),
                keccak256(
                    abi.encodePacked(
                        "PPv1wdrl",
                        uint64(_u(string.concat(p, ".withdrawalId"))),
                        jvm.parseJsonAddress(j, string.concat(p, ".account")),
                        jvm.parseJsonAddress(j, string.concat(p, ".recipient")),
                        amount,
                        uint64(_u(string.concat(p, ".height")))
                    )
                ),
                "withdrawal leaf bytes"
            );
            bytes32 leaf = keccak256(abi.encodePacked(bytes1(0x00), leafData));
            assertEq(leaf, jvm.parseJsonBytes32(j, string.concat(p, ".leafHash")), "withdrawal leaf hash");
            assertEq(_u(string.concat(p, ".withdrawalId")), i, "record id is its position");
            assertEq(
                h.rootX(leaf, uint64(i), jvm.parseJsonBytes32Array(j, string.concat(p, ".siblings"))),
                cp.withdrawalsRoot,
                "withdrawal proof"
            );
            total += amount;
        }
        assertEq(total, cp.withdrawalsTotal, "sum of records == withdrawalsTotal");
        assertTrue(cp.withdrawalCount == 2, "non-vacuous: two records");
    }
}

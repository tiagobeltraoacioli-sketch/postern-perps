// SPDX-License-Identifier: UNLICENSED
pragma solidity 0.8.24;

import {PerpsVault} from "../src/PerpsVault.sol";
import {Base, Tree} from "./Base.sol";

/// Prints the shared vectors (test/vectors/vault-vectors.json is this output) and pins them, so the engine (388) and
/// the node (389) can check their encoders byte for byte against the vault.
contract PerpsVaultVectors is Base {
    event log_named_bytes32(string key, bytes32 val);
    event log_named_address(string key, address val);

    function test_vectors() public {
        _setUpVault();
        vm.chainId(700770);
        PerpsVault v = new PerpsVault(_config());
        emit log_named_address("vault", address(v));
        PerpsVault.Withdrawal memory w = PerpsVault.Withdrawal(
            3, address(0x1111111111111111111111111111111111111111), address(0x2222222222222222222222222222222222222222),
            1_234_567, 99
        );
        bytes32 wl = _wLeaf(w);
        emit log_named_bytes32("withdrawalLeaf(id 3, 0x11.., 0x22.., 1234567, height 99)", wl);
        PerpsVault.Account memory a;
        a.account = address(0x3333333333333333333333333333333333333333);
        a.index = 5;
        a.balance = -42;
        a.escapeValue = 7_000_000;
        a.positionsHash = keccak256("positions");
        bytes32 al = _aLeaf(a);
        emit log_named_bytes32("accountLeaf(0x33.., index 5, balance -42, escape 7000000, keccak('positions'))", al);
        bytes32[] memory leaves = new bytes32[](3);
        leaves[0] = keccak256("a");
        leaves[1] = keccak256("b");
        leaves[2] = keccak256("c");
        emit log_named_bytes32("root32(['a','b','c'] hashed as leaves)", Tree.root(leaves));
        emit log_named_bytes32("root32([])", Tree.root(new bytes32[](0)));
        emit log_named_bytes32("depositHead(0) on 700770", v.depositHead(0));
        emit log_named_bytes32("forcedHead(0) on 700770", v.forcedHead(0));
        PerpsVault.Checkpoint memory cp;
        cp.height = 10;
        cp.time = 1_800_000_000_000;
        cp.logHash = keccak256("log");
        cp.accountsRoot = keccak256("accounts");
        cp.marketsRoot = keccak256("markets");
        cp.withdrawalsRoot = keccak256("withdrawals");
        cp.nextDepositIndex = 2;
        cp.nextForcedIndex = 1;
        cp.nextOrderId = 77;
        cp.totalEscape = 5_000_000;
        cp.dust = 9;
        cp.auxHash = keccak256("aux");
        cp.depositsHead = keccak256("dh");
        cp.forcedHead = keccak256("fh");
        cp.withdrawalCount = 4;
        cp.withdrawalsTotal = 1_000_000;
        emit log_named_bytes32("stateRootOf(cp)", v.stateRootOf(cp));
        emit log_named_bytes32("domainSeparator", v.domainSeparator());
        emit log_named_bytes32("checkpointDigest(cp)", v.checkpointDigest(cp));
        emit log_named_bytes32("CHECKPOINT_TYPEHASH", v.CHECKPOINT_TYPEHASH());
        assertEq(wl, 0xe9166baba646062a6c6d061b7bd43516cf74f9893efb8dc38fe32d02a0b77bd9, "pinned withdrawal leaf");
        assertEq(al, 0xa2ea3a54aca1e1783d55a91764b89cd23991312c8e5b29ae07641a7b9264c61e, "pinned account leaf");
        assertEq(Tree.root(leaves), 0x8fbe559c6533a8a7b143ff5aeb5a68f790da1a04469c65f6b7a3a8895fa97d89, "pinned root");
        assertEq(v.stateRootOf(cp), 0xa37422a49ce13023aba96c4eb9989223238fc85e408d8bbf083d645d79dba9ac, "pinned stateRoot (SPEC 0.1.5)");
        assertEq(v.checkpointDigest(cp), 0xedbdd9a9ebd4e2e3767ec12a7c2e2fa499fd328953513ff27eb7ad385e3163f9, "pinned digest");
        // independent recomputation of the struct hash with one abi.encode call
        bytes32 sh = keccak256(
            abi.encode(
                v.CHECKPOINT_TYPEHASH(), cp.height, cp.time, cp.logHash, cp.accountsRoot, cp.marketsRoot,
                cp.withdrawalsRoot, cp.nextDepositIndex, cp.nextForcedIndex
            )
        );
        sh; // the split-encoding equality is pinned below through the digest of an off-chain-style computation
        bytes memory full = bytes.concat(
            abi.encode(
                v.CHECKPOINT_TYPEHASH(), cp.height, cp.time, cp.logHash, cp.accountsRoot, cp.marketsRoot,
                cp.withdrawalsRoot, cp.nextDepositIndex
            ),
            abi.encode(cp.nextForcedIndex, cp.nextOrderId, cp.totalEscape, cp.dust, cp.auxHash),
            abi.encode(cp.depositsHead, cp.forcedHead, cp.withdrawalCount, cp.withdrawalsTotal)
        );
        assertEq(full.length, 17 * 32, "17 words: typehash + 16 fields");
        assertEq(
            keccak256(abi.encodePacked("\x19\x01", v.domainSeparator(), keccak256(full))),
            v.checkpointDigest(cp),
            "digest"
        );
    }
}

interface StoreVm {
    function store(address target, bytes32 slot, bytes32 value) external;
}

/// 388's shared forced-request vector (engine/vectors/forced-vectors.json, codex/perps-engine-20261117 e2fca75):
/// reproduced by the vault's own `requestForced` chain at the vector's vault address on chain 700770.
contract PerpsVaultForcedVector is Base {
    function test_forced_vector_388() public {
        _setUpVault();
        vm.chainId(700770);
        address at = address(0x00000000000000000000000000000000000Ba417);
        vm.etch(at, address(vault).code); // same immutables (token, forcedDelay 2 days), empty storage
        vault = PerpsVault(at);
        address acct = address(0x4be5Ca6bcF47515945697c1451676b7A3971FbCA);
        uint256 t0 = 1794966400 - 2 days;
        vm.warp(t0);
        _deposit(acct, acct, 10 * USDC);
        vm.prank(acct);
        vault.requestForced(0, 0); // deadline 1794966400
        // let request 0 count as processed (latest.nextForcedIndex = 1; slot 13, bytes 24..31) so the account may
        // open its next request, as after an accepted checkpoint
        StoreVm(address(vm)).store(at, bytes32(uint256(13)), bytes32(uint256(1) << 192));
        vm.warp(t0 + 1);
        vm.prank(acct);
        vault.requestForced(2, 5_000_000); // deadline 1794966401
        assertEq(vault.forcedDeadline(0), 1794966400, "deadline 0");
        assertEq(vault.forcedDeadline(1), 1794966401, "deadline 1");
        assertEq(
            vault.forcedHead(2), 0xef4758f64316a851cda4dbd78f4627774502320f46836b5255e365dfcff61be3, "388 forcedHead2"
        );
    }
}

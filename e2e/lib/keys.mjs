// Throwaway keys of the e2e rehearsal. Every key here is derived from a PUBLIC label: none protects anything.
// UNAUDITED prototype. Copyright (c) 2026 Bloch Inc. All rights reserved.
import { keccak256, hex } from '../../node/mock/lib/keccak.mjs';
import { addressOf, N } from '../../node/mock/lib/secp256k1.mjs';

const enc = new TextEncoder();
const valid = (k) => { const x = BigInt(hex(k)); return x >= 1n && x < N; };

/** A rehearsal user key: keccak256("postern-perps-e2e THROWAWAY key | <label>"). */
export function userKey(label) {
  const k = keccak256(enc.encode(`postern-perps-e2e THROWAWAY key | ${label}`));
  if (!valid(k)) throw new Error(`label ${label} gives no valid scalar`);
  return k;
}

/** The node's dev-mode key derivation (node/src/sig.rs `throwaway_key`), reproduced so the rehearsal can play a
 *  committee member that signs a WRONG state root (the sequencer must not count it). */
export function nodeDevKey(label, i) {
  for (let ctr = 0; ; ctr++) {
    const k = keccak256(enc.encode(`postern-perps THROWAWAY dev key | ${label} | ${i} | ${ctr}`));
    if (valid(k)) return k;
  }
}

/** Dev committee of a seed, sorted by address ascending (node/src/dev.rs `sorted_keys`). kind: validator | oracle. */
export function nodeDevCommittee(seed, kind, n) {
  return Array.from({ length: n }, (_, i) => nodeDevKey(`${seed}/${kind}`, i))
    .map((key) => ({ key, address: addressOf(key) }))
    .sort((a, b) => (a.address < b.address ? -1 : 1));
}

export const user = (label) => { const key = userKey(label); return { label, key, address: addressOf(key) }; };

/**
 * The Merkle formats of docs/VAULT.md 5.5 (SPEC 11), recomputed in the page. UNAUDITED prototype.
 *
 * Why the page recomputes them: a withdrawal or escape claim is a transaction built from a record and 32 sibling
 * hashes that the exchange's API hands over. Before the wallet is asked, the page hashes the record itself and walks
 * the path to a root, and compares that root with the one the VAULT holds on the L2 (read through the L2 endpoint, not
 * through the API). A record the API altered (another recipient, another amount) or a proof against a root the vault
 * never accepted is refused here, in words, instead of costing the user a reverted transaction or showing a claim
 * that pays someone else. Pinned by contracts/test/vectors/vault-vectors.json (src/test/vault-merkle.test.ts).
 *
 *   leaf  = keccak256(0x00 || data)         node = keccak256(0x01 || left || right)       depth 32
 *   withdrawal data = "PPv1wdrl" || be64 withdrawalId || account:20 || recipient:20 || be64 amount || be64 height
 *   account data    = "PPv1acct" || account:20 || be32 index || be128 balance (two's complement) || be64 escapeValue
 *                     || positionsHash:32
 *   bit i of the position set = the running node is the RIGHT child at level i.
 */
import { concatHex, encodeAbiParameters, keccak256, numberToHex, stringToHex, toHex, type Hex } from 'viem';

export const TREE_DEPTH = 32;
export type WithdrawalRecord = { withdrawalId: bigint; account: Hex; recipient: Hex; amount: bigint; height: bigint };
export type AccountLeaf = { account: Hex; index: number; balance: bigint; escapeValue: bigint; positionsHash: Hex };

const MAX_U64 = (1n << 64n) - 1n;
const be = (value: bigint, bytes: number, what: string): Hex => {
  if (value < 0n || value >= 1n << BigInt(bytes * 8)) throw new RangeError(`${what} is out of range`);
  return numberToHex(value, { size: bytes });
};
const address = (value: string, what: string): Hex => {
  if (!/^0x[0-9a-fA-F]{40}$/.test(value)) throw new RangeError(`${what} is not an address`);
  return value.toLowerCase() as Hex;
};
const hash32 = (value: string, what: string): Hex => {
  if (!/^0x[0-9a-fA-F]{64}$/.test(value)) throw new RangeError(`${what} is not 32 bytes`);
  return value.toLowerCase() as Hex;
};

export function withdrawalLeaf(w: WithdrawalRecord): Hex {
  return keccak256(
    concatHex(['0x00', stringToHex('PPv1wdrl'), be(w.withdrawalId, 8, 'withdrawalId'), address(w.account, 'account'), address(w.recipient, 'recipient'), be(w.amount, 8, 'amount'), be(w.height, 8, 'height')]),
  );
}

export function accountLeaf(a: AccountLeaf): Hex {
  if (!Number.isInteger(a.index) || a.index < 0 || a.index > 0xffffffff) throw new RangeError('index is out of range');
  const min = -(1n << 127n);
  if (a.balance < min || a.balance >= 1n << 127n) throw new RangeError('balance is out of range');
  const twos = a.balance < 0n ? (1n << 128n) + a.balance : a.balance;
  return keccak256(
    concatHex(['0x00', stringToHex('PPv1acct'), address(a.account, 'account'), numberToHex(a.index, { size: 4 }), numberToHex(twos, { size: 16 }), be(a.escapeValue, 8, 'escapeValue'), hash32(a.positionsHash, 'positionsHash')]),
  );
}

export const node = (left: Hex, right: Hex): Hex => keccak256(concatHex(['0x01', left, right]));

/** Z(0) = 32 zero bytes, Z(i+1) = node(Z(i), Z(i)). ZERO[32] is the root of the empty tree. */
export const ZERO: readonly Hex[] = (() => {
  const z: Hex[] = [`0x${'00'.repeat(32)}`];
  for (let i = 0; i < TREE_DEPTH; i++) z.push(node(z[i], z[i]));
  return z;
})();

/** The root reached from `leaf` at `position` through exactly 32 siblings (bottom-up). Throws on a malformed proof. */
export function rootFromProof(leaf: Hex, position: bigint, siblings: readonly string[]): Hex {
  if (siblings.length !== TREE_DEPTH) throw new RangeError(`a proof has exactly ${TREE_DEPTH} hashes (got ${siblings.length})`);
  if (position < 0n || position >> BigInt(TREE_DEPTH) !== 0n) throw new RangeError('the position is outside the tree');
  let h = hash32(leaf, 'leaf');
  for (let i = 0; i < TREE_DEPTH; i++) {
    const s = hash32(siblings[i], `sibling ${i}`);
    h = (position >> BigInt(i)) & 1n ? node(s, h) : node(h, s);
  }
  return h;
}

/**
 * A depth-32 tree over leaf HASHES at positions 0..n-1 (the rest empty), with proofs. Used by the tests and the local
 * anvil rehearsal to stand in for the engine; the page itself never builds a tree, it only checks proofs.
 */
export class SparseTree {
  private readonly levels: Hex[][];
  constructor(leaves: readonly Hex[]) {
    this.levels = [leaves.map((l) => hash32(l, 'leaf'))];
    for (let d = 0; d < TREE_DEPTH; d++) {
      const below = this.levels[d];
      const above: Hex[] = [];
      for (let i = 0; i < below.length; i += 2) above.push(node(below[i], below[i + 1] ?? ZERO[d]));
      this.levels.push(above);
    }
  }
  get root(): Hex {
    return this.levels[TREE_DEPTH][0] ?? ZERO[TREE_DEPTH];
  }
  proof(position: number): Hex[] {
    const out: Hex[] = [];
    let i = position;
    for (let d = 0; d < TREE_DEPTH; d++) {
      out.push(this.levels[d][i ^ 1] ?? ZERO[d]);
      i >>= 1;
    }
    return out;
  }
}

/** VAULT.md 5.4 chain heads (the vault's own deposit and forced-request chains). */
const tag = (text: string): Hex => stringToHex(text, { size: 32 });
export function depositHead0(chainId: number, vault: Hex): Hex {
  return keccak256(encodeAbiParameters([{ type: 'bytes32' }, { type: 'uint256' }, { type: 'address' }], [tag('PosternPerps.deposit.v1'), BigInt(chainId), vault]));
}
export function forcedHead0(chainId: number, vault: Hex): Hex {
  return keccak256(encodeAbiParameters([{ type: 'bytes32' }, { type: 'uint256' }, { type: 'address' }], [tag('PosternPerps.forced.v1'), BigInt(chainId), vault]));
}
export function nextDepositHead(head: Hex, index: bigint, account: Hex, amount: bigint): Hex {
  return keccak256(encodeAbiParameters([{ type: 'bytes32' }, { type: 'uint256' }, { type: 'address' }, { type: 'uint256' }], [head, index, account, amount]));
}

export const isU64 = (v: bigint) => v >= 0n && v <= MAX_U64;
export { toHex };

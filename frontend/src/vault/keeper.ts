/**
 * Keeps the user's own exit proof on this device. UNAUDITED prototype. Answers review finding R436-11 (c).
 *
 * Why: in escape mode an account claims with its leaf and 32 sibling hashes against the account root of the LAST
 * checkpoint the vault accepted. Those siblings depend on every account's state, so only someone who replayed the
 * exchange can produce them, and escape is exactly the case where the exchange's API may be gone. So while the
 * exchange is alive the page asks it for the user's proof at each checkpoint the vault accepts, CHECKS it against
 * the root the vault holds on the L2 (vault.ts `checkEscapeClaim`), and only then stores it. A stored proof is
 * therefore one the vault will accept for as long as that checkpoint is the vault's latest; the newest few are kept
 * because a newer checkpoint may reach the vault after the last visit. The proof is public data (the exchange is
 * transparent); it is kept in this browser's storage and offered as a file. It is not a key and moves nothing.
 *
 * What it does NOT solve: a user who never opened the page after the vault's last accepted checkpoint has no proof
 * for it here; then the proof must come from a validator or an archive (pasted on the Exit tab).
 */
import type { Hex } from 'viem';
import type { AccountLeaf } from './merkle';
import { parseAccountProof } from './wire';

export const KEPT = 4;
const PREFIX = 'postern.perps.exitproof.v1';
export type KeptProof = {
  chainId: number;
  vault: Hex;
  account: Hex;
  /** Engine height and account root of the vault checkpoint the proof was verified against. */
  height: string;
  accountsRoot: Hex;
  savedAt: number;
  leafData: { account: Hex; index: number; balance: string; escapeValue: string; positionsHash: Hex };
  proof: Hex[];
};

const key = (vault: string, account: string) => `${PREFIX}:${vault.toLowerCase()}:${account.toLowerCase()}`;

function valid(v: unknown): v is KeptProof {
  if (!v || typeof v !== 'object') return false;
  const p = v as KeptProof;
  if (typeof p.height !== 'string' || !/^\d{1,20}$/.test(p.height) || typeof p.accountsRoot !== 'string' || !/^0x[0-9a-f]{64}$/.test(p.accountsRoot) || typeof p.savedAt !== 'number') return false;
  try {
    parseAccountProof(p);
    return true;
  } catch {
    return false;
  }
}

/** The proofs kept for this account and vault, newest checkpoint first. Unreadable storage or entries: none. */
export function keptProofs(vault: string, account: string): KeptProof[] {
  try {
    const raw = localStorage.getItem(key(vault, account));
    const list: unknown = raw ? JSON.parse(raw) : [];
    if (!Array.isArray(list)) return [];
    return list.filter(valid).filter((p) => p.vault === vault.toLowerCase() && p.account === account.toLowerCase());
  } catch {
    return [];
  }
}

/** Stores a proof ALREADY CHECKED against `accountsRoot` as held by the vault. Returns false when storage refused. */
export function keepProof(entry: { chainId: number; vault: Hex; account: Hex; height: bigint; accountsRoot: Hex; leaf: AccountLeaf; siblings: readonly Hex[] }, now: number = Date.now()): boolean {
  const kept: KeptProof = {
    chainId: entry.chainId,
    vault: entry.vault.toLowerCase() as Hex,
    account: entry.account.toLowerCase() as Hex,
    height: entry.height.toString(),
    accountsRoot: entry.accountsRoot.toLowerCase() as Hex,
    savedAt: now,
    leafData: { account: entry.leaf.account, index: entry.leaf.index, balance: entry.leaf.balance.toString(), escapeValue: entry.leaf.escapeValue.toString(), positionsHash: entry.leaf.positionsHash },
    proof: [...entry.siblings],
  };
  if (!valid(kept) || kept.leafData.account !== kept.account) return false;
  const others = keptProofs(kept.vault, kept.account).filter((p) => p.accountsRoot !== kept.accountsRoot);
  const list = [kept, ...others].sort((a, b) => (BigInt(b.height) > BigInt(a.height) ? 1 : BigInt(b.height) < BigInt(a.height) ? -1 : 0)).slice(0, KEPT);
  try {
    localStorage.setItem(key(kept.vault, kept.account), JSON.stringify(list));
    return true;
  } catch {
    return false;
  }
}

/** The kept proof for exactly this account root (the vault's latest), ready for `checkEscapeClaim`, or null. */
export function keptProofFor(vault: string, account: string, accountsRoot: string): { leaf: AccountLeaf; siblings: Hex[]; kept: KeptProof } | null {
  const hit = keptProofs(vault, account).find((p) => p.accountsRoot === accountsRoot.toLowerCase());
  if (!hit) return null;
  return { ...parseAccountProof(hit), kept: hit };
}

/** The proof as a file a user can keep anywhere and paste back on the Exit tab (it parses as an account proof). */
export function proofFile(p: KeptProof): { name: string; text: string } {
  return {
    name: `postern-perps-exit-proof-${p.account.slice(0, 10)}-h${p.height}.json`,
    text: `${JSON.stringify(
      {
        note: 'PosternDEX Perpetuals (UNAUDITED prototype): escape proof of one account. Public data, not a key. Usable with PerpsVault.claimEscape while the vault\'s latest checkpoint has this accountsRoot.',
        chainId: p.chainId,
        vault: p.vault,
        account: p.account,
        checkpointHeight: p.height,
        accountsRoot: p.accountsRoot,
        savedAt: new Date(p.savedAt).toISOString(),
        leafData: p.leafData,
        proof: p.proof,
      },
      null,
      1,
    )}\n`,
  };
}

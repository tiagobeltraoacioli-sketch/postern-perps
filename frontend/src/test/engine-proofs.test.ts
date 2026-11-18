/**
 * vault/merkle.ts and vault/wire.ts against proofs PRODUCED BY THE ENGINE (engine/vectors/proofs-example.json, agent
 * 430, `engine proofs-example` at cd8e5ae): the leaves the page hashes and the roots it walks to are the engine's.
 */
import { createHash } from 'node:crypto';
import { existsSync, readFileSync } from 'node:fs';
import { resolve } from 'node:path';
import { describe, expect, it } from 'vitest';
import { concatHex, keccak256, type Hex } from 'viem';
import p from './vectors/proofs-example.json';
import { accountLeaf, rootFromProof, withdrawalLeaf } from '../vault/merkle';
import { parseAccountProof, parseWithdrawalCertificate } from '../vault/wire';
import { checkEscapeClaim, escapeAmount, type VaultView } from '../vault/vault';

describe('engine proofs (SPEC 11) through the page\'s own hashing', () => {
  it('the copy is the engine\'s file', () => {
    const copy = readFileSync(resolve(process.cwd(), 'src/test/vectors/proofs-example.json'));
    expect(createHash('sha256').update(copy).digest('hex')).toBe('d9925150b089c5fde04eacaa459bd75cff724a77087d39397923d70054682a9d');
    const inTree = resolve(process.cwd(), '../engine/vectors/proofs-example.json');
    if (existsSync(inTree)) expect(readFileSync(inTree).equals(copy), 'engine/vectors/proofs-example.json changed: copy it again and re-run').toBe(true);
  });
  it(`every account leaf (${p.accounts.length}) is hashed as the engine hashes it and walks to accountsRoot`, () => {
    for (const a of p.accounts) {
      // The API's AccountProof shape (docs/API.md 4): leafData fields + 32 siblings.
      const parsed = parseAccountProof({ leafData: { account: a.account, index: a.index, balance: a.balance, escapeValue: a.escapeValue, positionsHash: a.positionsHash }, proof: a.siblings });
      expect(accountLeaf(parsed.leaf)).toBe(a.leafHash);
      expect(keccak256(concatHex(['0x00', a.leafData as Hex]))).toBe(a.leafHash);
      expect(rootFromProof(a.leafHash as Hex, BigInt(a.index), a.siblings)).toBe(p.checkpoint.accountsRoot);
      const view = { latest: { accountsRoot: p.checkpoint.accountsRoot } } as VaultView;
      expect(() => checkEscapeClaim(view, parsed.leaf, parsed.siblings, a.account)).not.toThrow();
      expect(() => checkEscapeClaim(view, { ...parsed.leaf, escapeValue: parsed.leaf.escapeValue + 1n }, parsed.siblings, a.account)).toThrow(/does not match/);
    }
    expect(p.accounts[p.insuranceIndex].account).toBe(`0x${'0'.repeat(39)}2`);
    expect(p.accounts[p.backstopIndex].account).toBe(`0x${'0'.repeat(39)}3`);
  });
  it(`every withdrawal record (${p.withdrawals.length}) is hashed as the engine hashes it and walks to withdrawalsRoot`, () => {
    for (const w of p.withdrawals) {
      const parsed = parseWithdrawalCertificate({ record: { withdrawalId: w.withdrawalId, account: w.account, recipient: w.recipient, amount: w.amount, height: w.height }, proof: w.siblings });
      expect(withdrawalLeaf(parsed.record)).toBe(w.leafHash);
      expect(rootFromProof(w.leafHash as Hex, parsed.record.withdrawalId, parsed.siblings)).toBe(p.checkpoint.withdrawalsRoot);
    }
    expect(p.withdrawals.reduce((t, w) => t + BigInt(w.amount), 0n).toString()).toBe(p.checkpoint.withdrawalsTotal);
  });
  it('the escape values add up to totalEscape, and with the pool = custody every user is paid in full by the vault\'s formula', () => {
    const total = p.accounts.reduce((t, a) => t + BigInt(a.escapeValue), 0n);
    expect(total.toString()).toBe(p.checkpoint.totalEscape);
    const system = BigInt(p.accounts[p.insuranceIndex].escapeValue) + BigInt(p.accounts[p.backstopIndex].escapeValue);
    const senior = total - system;
    const view = { escapeAt: 1n, systemSharesProven: true, escapePayable: senior, subordinatedTotal: system, latest: { totalEscape: total } } as VaultView;
    for (const a of p.accounts.slice(3)) expect(escapeAmount(view, BigInt(a.escapeValue))).toBe(BigInt(a.escapeValue));
  });
});

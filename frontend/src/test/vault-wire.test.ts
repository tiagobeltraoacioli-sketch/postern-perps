import { describe, expect, it } from 'vitest';
import { parseAccountProof, parsePastedProof, parseWithdrawalCertificate, WireError } from '../vault/wire';

const H = (b: string) => `0x${b.repeat(32)}`;
const A = (b: string) => `0x${b.repeat(20)}`;
const proof = Array.from({ length: 32 }, (_, i) => H(i.toString(16).padStart(2, '0')));
const record = { withdrawalId: '74', account: A('aB'), recipient: A('cd'), amount: '400000000', height: '119903' };
const leafData = { account: A('ab'), index: 3, balance: '-42', escapeValue: '7000000', positionsHash: H('ee') };

describe('withdrawal certificate', () => {
  it('reads the record in base units and the 32 hashes, lower-cased', () => {
    const c = parseWithdrawalCertificate({ record, proof, digest: '0x', signatures: [] });
    expect(c.record).toEqual({ withdrawalId: 74n, account: A('ab'), recipient: A('cd'), amount: 400_000_000n, height: 119_903n });
    expect(c.siblings).toHaveLength(32);
    expect(parseWithdrawalCertificate({ record: { ...record, withdrawalId: undefined, index: 74 }, proof: { hashes: proof } }).record.withdrawalId).toBe(74n);
    expect(parseWithdrawalCertificate({ record, siblings: proof }).siblings[31]).toBe(proof[31]);
  });
  it('an amount that could be read two ways is refused: a decimal fraction, a float, hex, a leading zero, a sign', () => {
    for (const amount of ['400.000000', 400.5, '0x17d78400', '0400', '-1', '', null, '1e9', 2 ** 60])
      expect(() => parseWithdrawalCertificate({ record: { ...record, amount }, proof }), String(amount)).toThrow(/amount/);
    expect(() => parseWithdrawalCertificate({ record: { ...record, amount: (1n << 64n).toString() }, proof })).toThrow(/out of range/);
  });
  it('a proof that is not exactly 32 well-formed hashes is refused (the mock tree\'s 3 hashes are)', () => {
    expect(() => parseWithdrawalCertificate({ record, proof: proof.slice(0, 3) })).toThrow(/3 hashes; the vault takes exactly 32/);
    expect(() => parseWithdrawalCertificate({ record, proof: [...proof.slice(0, 31), '0x12'] })).toThrow(/proof hash 31/);
    expect(() => parseWithdrawalCertificate({ record })).toThrow(/proof is missing/);
    expect(() => parseWithdrawalCertificate({ record: { ...record, recipient: '0x12' }, proof })).toThrow(/recipient/);
    expect(() => parseWithdrawalCertificate(null)).toThrow(WireError);
  });
});

describe('account proof', () => {
  it('reads the SPEC 11 leaf; a negative balance keeps its sign', () => {
    const p = parseAccountProof({ address: A('ab'), leafData, proof, checkpoint: {} });
    expect(p.leaf).toEqual({ account: A('ab'), index: 3, balance: -42n, escapeValue: 7_000_000n, positionsHash: H('ee') });
    expect(parseAccountProof({ leaf: { ...leafData, account: undefined, address: A('ab'), index: undefined }, index: '3', proof }).leaf.index).toBe(3);
  });
  it('a proof without the fields the vault hashes is refused by name (round-1 mock leaf data)', () => {
    const old = { address: A('ab'), collateral: '23476827238', positions: [] };
    expect(() => parseAccountProof({ leafData: old, index: '2', proof })).toThrow(/balance/);
    expect(() => parseAccountProof({ leafData: { ...leafData, escapeValue: '7.0' }, proof })).toThrow(/escapeValue/);
    expect(() => parseAccountProof({ leafData: { ...leafData, positionsHash: '0x00' }, proof })).toThrow(/positionsHash/);
    expect(() => parseAccountProof({ leafData: { ...leafData, index: 2 ** 32 }, proof })).toThrow(/index/);
    expect(() => parseAccountProof({ leaf: H('aa'), proof })).toThrow(/account leaf is missing/);
  });
  it('pasted text: JSON only, bounded', () => {
    expect(parsePastedProof(JSON.stringify({ leafData, proof })).leaf.escapeValue).toBe(7_000_000n);
    expect(() => parsePastedProof('not json')).toThrow(/not JSON/);
    expect(() => parsePastedProof(' '.repeat(70_000))).toThrow(/too long/);
  });
});

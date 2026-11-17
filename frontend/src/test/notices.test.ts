import { describe, expect, it } from 'vitest';
import { keccak256, toBytes } from 'viem';
import { generatePrivateKey, privateKeyToAccount } from 'viem/accounts';
import { STATEMENT_HASH, STATEMENT_TEXT, attestationMessage, attestationTypedData, signAttestation, statementProblem, storedAttestation, keepAttestation, ATTESTATION_TYPES } from '../compliance/eligibility';
import { NOTICE_VERSION, RISK_POINTS, restrictedText } from '../compliance/notices';
import type { Signer } from '../lib/sign';
import { NOW } from './fixtures';

describe('notices (copy of compliance/legal/notices.json NOTICES-0.2)', () => {
  it('version and texts as published by 392', () => {
    expect(NOTICE_VERSION).toBe('NOTICES-0.2-20261117');
    expect(RISK_POINTS).toHaveLength(7);
    expect(restrictedText('BR')).toBe('PosternDEX Perpetuals is not available in your region (BR). You can still cancel orders, reduce or close positions and withdraw.');
  });
});

describe('attestation (397 typed data, 392 statement)', () => {
  const key = generatePrivateKey();
  const acct = privateKeyToAccount(key);
  const account = acct.address.toLowerCase();
  const signer: Signer = { kind: 'wallet', address: account, sign: (d) => acct.signTypedData({ ...(d as object), message: { ...(d.message as object), issuedAt: BigInt(d.message.issuedAt as string), expiresAt: BigInt(d.message.expiresAt as string) } } as never) };
  it('the statement shown hashes to the pinned statementHash (keccak-256 of the exact text)', () => {
    expect(keccak256(toBytes(STATEMENT_TEXT))).toBe(STATEMENT_HASH);
    expect(statementProblem()).toBeNull();
  });
  it('the type is 397s field order', () => {
    expect(ATTESTATION_TYPES.Attestation.map((f) => f.name)).toEqual(['account', 'statementId', 'statementHash', 'residenceCountry', 'issuedAt', 'expiresAt']);
    expect(attestationTypedData(attestationMessage(account, 'pa', NOW)).domain).toEqual({ name: 'PosternPerps Attestation', version: '1', chainId: 7007701 });
  });
  it('refuses a reserved or malformed country', () => {
    expect(() => attestationMessage(account, 'XX', NOW)).toThrow(/two letters/);
    expect(() => attestationMessage(account, 'B', NOW)).toThrow(/two letters/);
  });
  it('signs with the main wallet, verifies the signer, keeps the record; an agent key cannot sign it', async () => {
    const record = await signAttestation(signer, attestationMessage(account, 'PA', NOW));
    keepAttestation(record);
    expect(storedAttestation(account, NOW + 1)?.signature).toBe(record.signature);
    expect(storedAttestation(account, NOW + 366 * 86_400_000)).toBeNull();
    await expect(signAttestation({ ...signer, kind: 'agent' }, attestationMessage(account, 'PA', NOW))).rejects.toThrow(/main wallet/);
    const other = privateKeyToAccount(generatePrivateKey());
    await expect(signAttestation({ ...signer, sign: (d) => other.signTypedData({ ...(d as object), message: { ...(d.message as object), issuedAt: BigInt(d.message.issuedAt as string), expiresAt: BigInt(d.message.expiresAt as string) } } as never) }, attestationMessage(account, 'PA', NOW))).rejects.toThrow(/another account/);
  });
});

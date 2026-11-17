import { describe, expect, it, vi } from 'vitest';
import { generatePrivateKey, privateKeyToAccount } from 'viem/accounts';
import { forViem, isExitAction, prepare, refusals, signAndSubmit, signatureProblem, valueProblems, type Signer } from '../lib/sign';
import { compareWithNode, domainFor, domainProblem, TYPES, type Message } from '../lib/eip712';
import { RESTRICTED_GEO, guard, market, markets, NOW, status, TEST_DOMAIN } from './fixtures';
import published from '../../mock/eip712.json';

const keyA = generatePrivateKey();
const A = privateKeyToAccount(keyA);
const account = A.address.toLowerCase();
const localSigner = (k = keyA, kind: Signer['kind'] = 'wallet', expiresAt?: number): Signer => {
  const acct = privateKeyToAccount(k);
  return { kind, address: acct.address.toLowerCase(), expiresAt, sign: (d) => acct.signTypedData(forViem(d) as never) };
};
const btc = market('BTC-USD');
const order = (over: Partial<Message> = {}): Message => ({
  account,
  nonce: String(NOW),
  marketId: 1,
  isBuy: true,
  orderType: 0,
  timeInForce: 0,
  price: (67_000n * 10n ** 8n).toString(),
  size: (btc.step * 10n).toString(),
  reduceOnly: false,
  triggerPrice: '0',
  clientOrderId: `0x${'11'.repeat(32)}`,
  ...over,
});

describe('refusals', () => {
  it('accepts a well-formed limit order under an open region', () => {
    expect(refusals('PlaceOrder', order(), localSigner(), guard(account))).toEqual([]);
  });
  it('refuses without a wallet and without a signer', () => {
    expect(refusals('PlaceOrder', order(), null, guard('', { account: null })).join(' ')).toMatch(/Connect a wallet.*No key/);
  });
  it('refuses a message naming another account', () => {
    const other = privateKeyToAccount(generatePrivateKey()).address.toLowerCase();
    expect(refusals('PlaceOrder', order({ account: other }), localSigner(), guard(account))).toContain('The request names another account than the connected wallet.');
  });
  it('refuses a wallet signer that is not the connected account', () => {
    expect(refusals('PlaceOrder', order(), localSigner(generatePrivateKey()), guard(account))).toContain('The wallet account changed. Review again.');
  });
  it('refuses before the typed-data check and when it differs', () => {
    expect(refusals('PlaceOrder', order(), localSigner(), guard(account, { eip712Problem: undefined })).join()).toMatch(/not been checked/);
    expect(refusals('PlaceOrder', order(), localSigner(), guard(account, { eip712Problem: 'type PlaceOrder differs' })).join()).toMatch(/out of date/);
  });
  it('refuses a market id the exchange does not publish, and one whose signing fields differ', () => {
    expect(refusals('PlaceOrder', order({ marketId: 9 }), localSigner(), guard(account)).join()).toMatch(/#9 is not published/);
    const renumbered = markets().map((m) => (m.symbol === 'BTC-USD' ? { ...m, priceDecimals: 2 } : m));
    expect(refusals('PlaceOrder', order(), localSigner(), guard(account, { markets: renumbered })).join()).toMatch(/decimals differs/);
    const dup = new Map([['BTC-USD', 'BTC-USD and ETH-USD both claim market id 1']]);
    expect(refusals('PlaceOrder', order(), localSigner(), guard(account, { tableProblems: dup })).join()).toMatch(/both claim/);
  });
  it('an agent key can never sign Withdraw, ApproveAgent or RevokeAgent, nor sign after expiry', () => {
    const agentKey = generatePrivateKey();
    const agent = localSigner(agentKey, 'agent', NOW + 3_600_000);
    const w = { account, nonce: '1', amount: '1000000', recipient: account };
    expect(refusals('Withdraw', w, agent, guard(account)).join()).toMatch(/needs your main wallet/);
    expect(refusals('RevokeAgent', { account, nonce: '1', agent: agent.address }, agent, guard(account)).join()).toMatch(/needs your main wallet/);
    expect(refusals('PlaceOrder', order(), agent, guard(account))).toEqual([]);
    const expired = localSigner(agentKey, 'agent', NOW);
    expect(refusals('PlaceOrder', order(), expired, guard(account)).join()).toMatch(/expired/);
  });
  it('restricted or unknown region: only exit actions', () => {
    const g = guard(account, { geo: RESTRICTED_GEO });
    expect(refusals('PlaceOrder', order(), localSigner(), g).join()).toMatch(/not available in your region/);
    expect(refusals('PlaceOrder', order({ reduceOnly: true }), localSigner(), g)).toEqual([]);
    expect(refusals('CancelAll', { account, nonce: '1', marketId: 0 }, localSigner(), g)).toEqual([]);
    expect(refusals('Withdraw', { account, nonce: '1', amount: '5', recipient: account }, localSigner(), g)).toEqual([]);
    expect(refusals('UpdateLeverage', { account, nonce: '1', marketId: 1, leverage: 5, isolated: false }, localSigner(), g).join()).toMatch(/region/);
    expect(refusals('PlaceOrder', order(), localSigner(), guard(account, { geo: null })).join()).toMatch(/could not be confirmed/);
  });
  it('sanctioned: everything refused, exits included', () => {
    const g = guard(account, { geo: { ...RESTRICTED_GEO, sanctioned: true } });
    expect(refusals('Withdraw', { account, nonce: '1', amount: '5', recipient: account }, localSigner(), g).join()).toMatch(/cannot use/);
  });
  it('halted: only cancels; close-only: cancels and reduce-only', () => {
    const halted = markets().map((m) => (m.marketId === 1 ? { ...m, status: 'halted' as const } : m));
    expect(refusals('PlaceOrder', order({ reduceOnly: true }), localSigner(), guard(account, { markets: halted })).join()).toMatch(/halted/);
    expect(refusals('CancelOrder', { account, nonce: '1', marketId: 1, orderId: '5' }, localSigner(), guard(account, { markets: halted }))).toEqual([]);
    const closeOnly = markets().map((m) => (m.marketId === 1 ? { ...m, status: 'closeOnly' as const } : m));
    expect(refusals('PlaceOrder', order(), localSigner(), guard(account, { markets: closeOnly })).join()).toMatch(/close-only/);
    expect(refusals('PlaceOrder', order({ reduceOnly: true }), localSigner(), guard(account, { markets: closeOnly }))).toEqual([]);
  });
  it('clock outside the SPEC 2.5 nonce window (vs the exchange clock): refused before signing', () => {
    expect(refusals('PlaceOrder', order(), localSigner(), guard(account, { clockSkewMs: 4 * 3_600_000 })).join()).toMatch(/behind the exchange/);
    expect(refusals('PlaceOrder', order(), localSigner(), guard(account, { clockSkewMs: -31_000 })).join()).toMatch(/ahead of the exchange/);
    expect(refusals('PlaceOrder', order(), localSigner(), guard(account, { clockSkewMs: 119_000 }))).toEqual([]);
    expect(refusals('PlaceOrder', order(), localSigner(), guard(account, { clockSkewMs: -29_000 }))).toEqual([]);
  });
  it('sequencer not running: refused', () => {
    expect(refusals('PlaceOrder', order(), localSigner(), guard(account, { status: status('recovering') })).join()).toMatch(/sequencer recovering/);
  });
});

describe('value refusals', () => {
  const v = (m: Message) => valueProblems('PlaceOrder', m, markets(), NOW).join(' | ');
  it('a market order without a worst price is refused', () => {
    expect(v(order({ orderType: 1, timeInForce: 1, price: '0' }))).toMatch(/positive price/);
  });
  it('price off tick, size off step, below minimum, above maximum', () => {
    expect(v(order({ price: (67_000n * 10n ** 8n + 1n).toString() }))).toMatch(/tick/);
    expect(v(order({ size: (btc.step + 1n).toString() }))).toMatch(/step/);
    expect(v(order({ size: (btc.maxSize + btc.step).toString() }))).toMatch(/maximum/);
  });
  it('market orders must be IOC, post-only only for limits, triggers only on trigger kinds', () => {
    expect(v(order({ orderType: 1, timeInForce: 0 }))).toMatch(/immediate-or-cancel/);
    expect(v(order({ orderType: 1, timeInForce: 2 }))).toMatch(/post-only/);
    expect(v(order({ triggerPrice: '100' }))).toMatch(/only stop and take-profit/);
    expect(v(order({ orderType: 2, timeInForce: 1, triggerPrice: '0' }))).toMatch(/trigger price must be positive/);
  });
  it('uint64 overflow, bad address, bad bytes32', () => {
    expect(v(order({ size: (1n << 64n).toString() }))).toMatch(/size is not a 64-bit/);
    expect(v(order({ account: '0x1234' }))).toMatch(/account is not an address/);
    expect(v(order({ clientOrderId: '0x12' }))).toMatch(/32 bytes/);
  });
  it('withdrawals: positive amount, real recipient', () => {
    expect(valueProblems('Withdraw', { account, nonce: '1', amount: '0', recipient: account }, [], NOW).join()).toMatch(/positive/);
    expect(valueProblems('Withdraw', { account, nonce: '1', amount: '1', recipient: `0x${'0'.repeat(40)}` }, [], NOW).join()).toMatch(/recipient is not an address/);
  });
  it('agent approvals: not the account, in the future, at most 180 days, a plain name', () => {
    // SPEC 2.3: expiresAt in milliseconds, T < expiresAt <= T + 180 days.
    const agent = privateKeyToAccount(generatePrivateKey()).address.toLowerCase();
    const a = (over: Partial<Message>) => valueProblems('ApproveAgent', { account, nonce: '1', agent, name: 'bot', expiresAt: String(NOW + 3_600_000), ...over }, [], NOW).join(' | ');
    expect(a({})).toBe('');
    expect(a({ agent: account })).toMatch(/not be the account/);
    expect(a({ expiresAt: String(NOW) })).toMatch(/future/);
    expect(a({ expiresAt: String(NOW + 180 * 86_400_000) })).toBe('');
    expect(a({ expiresAt: String(NOW + 180 * 86_400_000 + 1) })).toMatch(/180 days/);
    // A value in seconds (the pre-SPEC unit) is in the past in milliseconds: refused, never read as a far future.
    expect(a({ expiresAt: String(Math.floor(NOW / 1000) + 3600) })).toMatch(/future/);
    expect(a({ name: 'bot‮' })).toMatch(/plain characters/);
  });
  it('leverage bounds', () => {
    expect(valueProblems('UpdateLeverage', { account, nonce: '1', marketId: 3, leverage: 21, isolated: false }, markets(), NOW).join()).toMatch(/1 to 20/);
  });
});

describe('signature form', () => {
  it('65 bytes, low-s, v 27/28', () => {
    const r = '1'.padStart(64, '0');
    const s = '2'.padStart(64, '0');
    expect(signatureProblem(`0x${r}${s}1b`)).toBeNull();
    expect(signatureProblem(`0x${r}${s}`)).toMatch(/65 bytes/);
    expect(signatureProblem(`0x${r}${s}01`)).toMatch(/27 or 28/);
    const high = (0xfffffffffffffffffffffffffffffffebaaedce6af48a03bbfd25e8cd0364141n - 1n).toString(16);
    expect(signatureProblem(`0x${r}${high}1b`)).toMatch(/low-s/);
    expect(signatureProblem(`0x${'0'.repeat(64)}${s}1b`)).toMatch(/out of range/);
  });
});

describe('signAndSubmit', () => {
  it('signs the pinned typed data, verifies it recovers to the signer, then submits', async () => {
    const prepared = prepare('PlaceOrder', order(), localSigner(), guard(account));
    expect(prepared.refusals).toEqual([]);
    const submit = vi.fn(async (body: unknown) => body);
    const sent = (await signAndSubmit(prepared, localSigner(), () => guard(account), submit)) as { signature: string; type: string };
    expect(submit).toHaveBeenCalledOnce();
    expect(sent.type).toBe('PlaceOrder');
    expect(signatureProblem(sent.signature)).toBeNull();
  });
  it('re-checks the guard at signing time: a region change after the review stops the signature', async () => {
    const prepared = prepare('PlaceOrder', order(), localSigner(), guard(account));
    const signer = localSigner();
    const sign = vi.spyOn(signer, 'sign');
    const submit = vi.fn();
    await expect(signAndSubmit(prepared, signer, () => guard(account, { geo: RESTRICTED_GEO }), submit)).rejects.toThrow(/region/);
    expect(sign).not.toHaveBeenCalled();
    expect(submit).not.toHaveBeenCalled();
  });
  it('a wallet that signs with another key is caught before anything is submitted', async () => {
    const prepared = prepare('PlaceOrder', order(), localSigner(), guard(account));
    const other = privateKeyToAccount(generatePrivateKey());
    const lying: Signer = { kind: 'wallet', address: account, sign: (d) => other.signTypedData(forViem(d) as never) };
    const submit = vi.fn();
    await expect(signAndSubmit(prepared, lying, () => guard(account), submit)).rejects.toThrow(/another account/);
    expect(submit).not.toHaveBeenCalled();
  });
  it('a malformed wallet answer is refused', async () => {
    const prepared = prepare('PlaceOrder', order(), localSigner(), guard(account));
    const broken: Signer = { kind: 'wallet', address: account, sign: async () => '0xdeadbeef' };
    await expect(signAndSubmit(prepared, broken, () => guard(account), vi.fn())).rejects.toThrow(/unusable signature/);
  });
});

describe('pinned typed data', () => {
  const MOCK_DOMAIN = domainFor(published.domain.verifyingContract)!;
  it('the stand-in mock publishes exactly the pin (SPEC 0.1.1 section 2)', () => {
    expect(compareWithNode(published, MOCK_DOMAIN)).toBeNull();
  });
  it('no vault in the build: no domain, nothing can be prepared', () => {
    expect(domainFor(null)).toBeNull();
    expect(domainFor(`0x${'0'.repeat(40)}`)).toBeNull();
    expect(compareWithNode(published, null)).toMatch(/no signing domain/);
    expect(() => prepare('PlaceOrder', order(), localSigner(), guard(account, { domain: null }))).toThrow(/vault is not deployed/);
  });
  it('a signature for another deployment (other verifyingContract) is refused at signing time', async () => {
    const prepared = prepare('PlaceOrder', order(), localSigner(), guard(account));
    const other = domainFor('0x00000000000000000000000000000000000f0f02');
    await expect(signAndSubmit(prepared, localSigner(), () => guard(account, { domain: other }), vi.fn())).rejects.toThrow(/verifying contract/);
    expect(domainProblem({ ...TEST_DOMAIN }, other)).toMatch(/another deployment/);
    expect(compareWithNode({ ...published, domain: { ...published.domain, verifyingContract: '0x00000000000000000000000000000000000f0f02' } }, MOCK_DOMAIN)).toMatch(/verifying contract/);
  });
  it('one changed field type, an added field, a renamed domain or a verifyingContract is a difference', () => {
    const t = structuredClone(published) as { domain: Record<string, unknown>; types: Record<string, { name: string; type: string }[]> };
    t.types.UpdateMargin = [...t.types.UpdateMargin, { name: 'add', type: 'bool' }];
    expect(compareWithNode(t, MOCK_DOMAIN)).toMatch(/UpdateMargin differs/);
    expect(compareWithNode({ ...published, domain: { ...published.domain, chainId: 7007701 } }, MOCK_DOMAIN)).toMatch(/chain id/);
    expect(compareWithNode({ ...published, domain: { ...published.domain, salt: '0x' } }, MOCK_DOMAIN)).toMatch(/unexpected domain fields/);
    const u = structuredClone(published) as typeof t;
    u.types.PlaceOrder[6] = { name: 'price', type: 'uint128' };
    expect(compareWithNode(u, MOCK_DOMAIN)).toMatch(/PlaceOrder differs/);
  });
  it('the domain is PosternPerps / 1 / 700770 / the vault (SPEC 2.1); UpdateMargin is uint64 + bool', () => {
    expect(TEST_DOMAIN).toEqual({ name: 'PosternPerps', version: '1', chainId: 700770, verifyingContract: '0x00000000000000000000000000000000000f0f01' });
    expect(TYPES.UpdateMargin.map((f) => `${f.type} ${f.name}`)).toEqual(['address account', 'uint64 nonce', 'uint32 marketId', 'uint64 amount', 'bool add']);
    expect(Object.keys(TYPES)).toEqual(['PlaceOrder', 'CancelOrder', 'CancelAll', 'UpdateLeverage', 'UpdateMargin', 'Withdraw', 'ApproveAgent', 'RevokeAgent']);
  });
  it('exit actions are those of docs/API.md section 6 plus RevokeAgent (397 proposal)', () => {
    expect(isExitAction('CancelOrder', {})).toBe(true);
    expect(isExitAction('Withdraw', {})).toBe(true);
    expect(isExitAction('PlaceOrder', { reduceOnly: true })).toBe(true);
    expect(isExitAction('PlaceOrder', { reduceOnly: false })).toBe(false);
    expect(isExitAction('UpdateMargin', {})).toBe(false);
    expect(isExitAction('ApproveAgent', {})).toBe(false);
    expect(isExitAction('RevokeAgent', {})).toBe(true);
  });
});

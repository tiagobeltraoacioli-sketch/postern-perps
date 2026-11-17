// The conformance suite must catch real faults: each case breaks one rule in a running mock and asserts that the
// named checks fail (no vacuous checks). UNAUDITED prototype.
import { test } from 'node:test';
import assert from 'node:assert/strict';
import { startMock } from '../server.mjs';
import { runConformance } from '../conformance/run.mjs';

async function withMutation(mutate, expectFailing) {
  const m = await startMock({ port: 0, static: true, rateLimit: false });
  try {
    mutate(m.x);
    const r = await runConformance(m.url, { mock: true, runId: 'mut' });
    const failedNames = r.failed.map((f) => f.name);
    for (const pattern of expectFailing) assert.ok(failedNames.some((n) => pattern.test(n)), `expected a failing check matching ${pattern}; failed: ${failedNames.join(' | ')}`);
  } finally {
    await m.close();
  }
}

test('a checkpoint signed by the wrong key is caught', { timeout: 240_000 }, () =>
  withMutation((x) => {
    const orig = x.checkpointSignatures.bind(x);
    x.checkpointSignatures = (cp) => orig(cp).map((s, i) => (i === 0 ? { ...s, validator: x.committee.find((c) => c.address !== s.validator).address } : s));
  }, [/checkpoint signature recovers/, /strictly ascending|recovers/]));

test('a forgotten nonce (replay accepted) is caught', { timeout: 240_000 }, () =>
  withMutation((x) => {
    const map = x.nonceBySigner;
    x.nonceBySigner = { get: () => undefined, set: () => {} };
    void map;
  }, [/replayed action: HTTP 400/]));

test('a broken log-hash chain is caught', { timeout: 240_000 }, () =>
  withMutation((x) => {
    for (const b of x.blocks.values()) b.logHash = '0x' + 'ab'.repeat(32);
  }, [/logHash = keccak/]));

test('a crossed book is caught', { timeout: 240_000 }, () =>
  withMutation((x) => {
    const m = x.markets.values().next().value;
    const ask = x.best(m, 'asks');
    m.bids.set(ask + m.tick * 10n, [{ maker: true, account: x.liquidity, remaining: m.step, isBuy: true }]);
    x.requote = () => {};
  }, [/book not crossed/]));

test('a server that breaks an assumption fails the run with a reason, never aborts it', { timeout: 240_000 }, () =>
  withMutation((x) => {
    for (const cp of x.checkpoints) cp.withdrawalsRoot = undefined;
  }, [/checkpoint digest recomputed|matches Checkpoint/]));

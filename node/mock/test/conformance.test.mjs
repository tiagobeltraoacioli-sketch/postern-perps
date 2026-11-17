// The shared conformance suite run against the mock in static mode (every check, including the mock-only ones).
import { test } from 'node:test';
import assert from 'node:assert/strict';
import { startMock } from '../server.mjs';
import { runConformance } from '../conformance/run.mjs';

test('conformance suite passes against the mock (static mode)', { timeout: 240_000 }, async () => {
  const m = await startMock({ port: 0, static: true, rateLimit: false });
  try {
    const r = await runConformance(m.url, { mock: true, runId: 'test' });
    for (const f of r.failed) console.log(`FAIL ${f.name}: ${f.detail}`);
    console.log(`conformance: ${r.passed}/${r.total}`);
    assert.equal(r.failed.length, 0);
    assert.ok(r.total >= 150, `expected at least 150 checks, got ${r.total}`);
  } finally {
    await m.close();
  }
});

test('real-node mode: checks needing collateral are SKIPPED without --funded (never passed) and run with it', { timeout: 240_000 }, async () => {
  const m = await startMock({ port: 0, static: true, rateLimit: false });
  try {
    const bare = await runConformance(m.url, { mock: false, runId: 'bare' });
    assert.equal(bare.failed.length, 0, bare.failed.map((f) => f.name).join(' | '));
    assert.equal(bare.skipped.length, 3);
    assert.equal(bare.passed + bare.skipped.length, bare.total, 'a skipped check is not counted as passed');
    const funded = await runConformance(m.url, { mock: false, runId: 'funded', funded: 'test-funded' });
    assert.equal(funded.failed.length, 0, funded.failed.map((f) => f.name).join(' | '));
    assert.equal(funded.skipped.length, 0);
    assert.ok(funded.results.some((r) => /RevokeAgent included and applied/.test(r.name) && r.ok === true));
  } finally {
    await m.close();
  }
});

// Phase L: the shared API conformance suite against the running stack (real node, funded account). UNAUDITED prototype.
import { expect } from '../check.mjs';
import { runConformance } from '../../../node/mock/conformance/run.mjs';

export default async function (ctx) {
  await ctx.check('L-01', 'the whole API conformance suite passes against this stack with two funded accounts, nothing skipped', async () => {
    const r = await runConformance(ctx.stack.api, { funded: 'funded', funded2: 'funded2' });
    const ev = { total: r.total, passed: r.passed, failed: r.failed.map((f) => `${f.name}: ${f.detail}`.slice(0, 200)), skipped: r.skipped.map((s) => s.name) };
    ctx.state.conformance = ev;
    expect(r.failed.length === 0 && r.skipped.length === 0, `${r.passed}/${r.total}; failed: ${ev.failed.slice(0, 4).join(' | ')}; skipped: ${ev.skipped.length}`);
    return ev;
  });
}

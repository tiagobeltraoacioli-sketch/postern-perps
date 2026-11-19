// Phase J: a geoblocked country is refused; the exit stays open. UNAUDITED prototype. Everything legal about the
// list is for counsel; this phase checks only that the node enforces what it is given.
import { expect, blockedIf } from '../check.mjs';
import { onTick } from '../util.mjs';

export default async function (ctx) {
  const { api } = ctx;
  const btc = api.market('BTC-USD');
  const alice = ctx.who('alice');
  const ref = ctx.state.ref ?? '67250';
  await ctx.check('J-01', 'an opening order from Brazil is refused 451 GEO_RESTRICTED and never enters a block', async () => {
    const h0 = (await api.json('/v1/status')).latestBlock.height;
    const r = await api.order(alice, btc, { buy: true, price: onTick(btc, ref, -200), size: '0.01' }, { country: 'BR', wait: false });
    expect(r.status === 451 && r.body?.error?.code === 'GEO_RESTRICTED', `${r.status} ${JSON.stringify(r.body)}`);
    const acc = await api.account(alice.address);
    expect(acc.openOrders === 0, `open orders ${acc.openOrders}`);
    return { http: r.status, error: r.body.error, heightBefore: h0 };
  });
  await ctx.check('J-02', 'the same user may still leave from Brazil: CancelAll is admitted (exit-only), UpdateLeverage is not', async () => {
    const c = await api.cancelAll(alice, 0, { country: 'BR' });
    expect(c.status === 200, `CancelAll: ${c.status} ${JSON.stringify(c.body)}`);
    const u = await api.act(alice, 'UpdateLeverage', { marketId: btc.marketId, leverage: 2, isolated: false }, { country: 'BR', wait: false });
    expect(u.status === 451, `UpdateLeverage: ${u.status}`);
    return { cancelAll: c.status, updateLeverage: u.status };
  });
  await ctx.check('J-03', 'a country the list does not name is refused (deny by default); /v1/geo says so', async () => {
    const r = await api.order(alice, btc, { buy: true, price: onTick(btc, ref, -200), size: '0.01' }, { country: 'QQ', wait: false });
    expect(r.status === 451, `${r.status} ${JSON.stringify(r.body)}`);
    const g = await api.get('/v1/geo', { 'x-mock-country': 'BR' });
    expect(g.body.restricted === true && g.body.country === 'BR', JSON.stringify(g.body));
    return { unknownCountry: r.status, geo: g.body };
  });
  await ctx.blocked('J-04', 'with an edge secret configured, an action without the edge\'s per-action assertion is refused (397\'s design, fail closed)', 'the stack runs the node without --edge-secret-file: the edge module (compliance/geoblock/edge) is not yet a process of the stack');
}

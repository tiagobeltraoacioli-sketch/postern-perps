#!/usr/bin/env node
// Shared API conformance tests for the PosternDEX Perpetuals node: run against the mock (node/mock/server.mjs) or
// the real node in dev mode, unchanged. UNAUDITED prototype.
//
//   node node/mock/conformance/run.mjs --base http://127.0.0.1:49300 [--mock] [--schema FILE] [--json]
//
// Every response is validated against the API schema (node/api/schema.json when it exists, else the draft
// node/mock/schema/api.schema.json) and against semantic rules the schema cannot express (book order, tick and
// step, candle OHLC, pagination, log-hash chain, checkpoint signatures recomputed and recovered, admission
// refusals). Checks that need scenario switches run only with --mock. Keys used are throwaway keys derived from
// public labels. Node.js 22 or newer (global fetch and WebSocket).
import { readFileSync, existsSync } from 'node:fs';
import { fileURLToPath } from 'node:url';
import { dirname, join, resolve } from 'node:path';
import { Validator } from '../lib/schema.mjs';
import { keccak256, hex, unhex, concat } from '../lib/keccak.mjs';
import { sign, recover, addressOf, N } from '../lib/secp256k1.mjs';
import { typedDataHash } from '../lib/eip712.mjs';

const HERE = dirname(fileURLToPath(import.meta.url));
const REPO = resolve(HERE, '..', '..', '..');
const enc = new TextEncoder();
const keyOf = (label) => keccak256(enc.encode(`postern-perps-conformance/${label}`));
const be64 = (v) => {
  const b = new Uint8Array(8);
  new DataView(b.buffer).setBigUint64(0, BigInt(v));
  return b;
};
const CHECKPOINT_FIELDS = [
  { name: 'height', type: 'uint64' }, { name: 'time', type: 'uint64' }, { name: 'stateRoot', type: 'bytes32' },
  { name: 'logHash', type: 'bytes32' }, { name: 'withdrawalsRoot', type: 'bytes32' },
  { name: 'depositsCredited', type: 'uint64' }, { name: 'forcedProcessed', type: 'uint64' }];

export function schemaPath(explicit) {
  if (explicit) return explicit;
  const shared = join(REPO, 'node', 'api', 'schema.json');
  return existsSync(shared) ? shared : join(HERE, '..', 'schema', 'api.schema.json');
}

export async function runConformance(base, { mock = false, schema, runId = String(Date.now()) } = {}) {
  const V = new Validator(JSON.parse(readFileSync(schemaPath(schema), 'utf8')));
  const results = [];
  const ok = (name, cond, detail = '') => {
    results.push({ name, ok: !!cond, detail: cond ? '' : String(detail).slice(0, 400) });
    return !!cond;
  };
  const valid = (name, def, body) => {
    const e = V.validateDef(def, body);
    return ok(`${name} matches ${def}`, e.length === 0, e.slice(0, 5).join('; '));
  };
  const get = async (path, headers = {}) => {
    const r = await fetch(base + path, { headers });
    const text = await r.text();
    let body = null;
    try {
      body = JSON.parse(text);
    } catch {
      body = text;
    }
    return { status: r.status, body, headers: r.headers };
  };
  const post = async (path, body, headers = {}) => {
    const r = await fetch(base + path, { method: 'POST', headers: { 'content-type': 'application/json', ...headers }, body: typeof body === 'string' ? body : JSON.stringify(body) });
    return { status: r.status, body: await r.json().catch(() => null) };
  };
  const expectError = async (name, res, http, code) => {
    const r = await res;
    ok(`${name}: HTTP ${http}`, r.status === http, `got ${r.status} ${JSON.stringify(r.body).slice(0, 200)}`);
    ok(`${name}: code ${code}`, r.body?.error?.code === code, `got ${JSON.stringify(r.body).slice(0, 200)}`);
    valid(name, 'Error', r.body);
    return r;
  };

  // ---------- node, markets ----------
  const status = await get('/v1/status');
  valid('GET /v1/status', 'Status', status.body);
  const eip = await get('/v1/eip712');
  valid('GET /v1/eip712', 'Eip712', eip.body);
  const domain = eip.body.domain;
  ok('status.network.eip712 equals /v1/eip712 domain', JSON.stringify(status.body.network.eip712) === JSON.stringify(domain));
  ok('every agentAllowed action is an action and none moves funds', eip.body.agentAllowed.every((a) => eip.body.actions.includes(a)) && !eip.body.agentAllowed.some((a) => ['Withdraw', 'ApproveAgent', 'RevokeAgent'].includes(a)));
  const markets = await get('/v1/markets');
  valid('GET /v1/markets', 'MarketList', markets.body);
  const M = markets.body.markets;
  ok('marketIds unique', new Set(M.map((m) => m.marketId)).size === M.length);
  for (const m of M) {
    const dp = (s) => (s.includes('.') ? s.split('.')[1].length : 0);
    ok(`${m.symbol}: priceDecimals = precision of tickSize (SPEC 1)`, dp(m.tickSize) === m.priceDecimals, `${m.tickSize} vs ${m.priceDecimals}`);
    ok(`${m.symbol}: sizeDecimals = precision of stepSize (SPEC 1)`, dp(m.stepSize) === m.sizeDecimals, `${m.stepSize} vs ${m.sizeDecimals}`);
    const one = await get(`/v1/markets/${m.symbol}`);
    valid(`GET /v1/markets/${m.symbol}`, 'Market', one.body);
    ok(`${m.symbol}: single market equals list entry`, JSON.stringify(one.body) === JSON.stringify(m));
  }
  const tickers = await get('/v1/tickers');
  valid('GET /v1/tickers', 'TickerList', tickers.body);
  const sym = M[0].symbol;
  const mk = M[0];
  const scaleInt = (s, dp) => {
    const [i, f = ''] = s.replace('-', '').split('.');
    return BigInt(i + f.padEnd(dp, '0').slice(0, dp));
  };
  for (const m of M) {
    const t = await get(`/v1/markets/${m.symbol}/ticker`);
    valid(`GET ticker ${m.symbol}`, 'Ticker', t.body);
    const book = await get(`/v1/markets/${m.symbol}/book?depth=50`);
    valid(`GET book ${m.symbol}`, 'Book', book.body);
    const tick = scaleInt(m.tickSize, m.priceDecimals), step = scaleInt(m.stepSize, m.sizeDecimals);
    const bids = book.body.bids.map(([p, s]) => [scaleInt(p, m.priceDecimals), scaleInt(s, m.sizeDecimals)]);
    const asks = book.body.asks.map(([p, s]) => [scaleInt(p, m.priceDecimals), scaleInt(s, m.sizeDecimals)]);
    ok(`${m.symbol}: bids strictly descending`, bids.every((b, i) => i === 0 || b[0] < bids[i - 1][0]));
    ok(`${m.symbol}: asks strictly ascending`, asks.every((a, i) => i === 0 || a[0] > asks[i - 1][0]));
    ok(`${m.symbol}: book not crossed (SPEC I-3)`, !bids.length || !asks.length || bids[0][0] < asks[0][0]);
    ok(`${m.symbol}: every level on the tick and step, size > 0`, [...bids, ...asks].every(([p, s]) => p % tick === 0n && s % step === 0n && s > 0n));
    const d1 = await get(`/v1/markets/${m.symbol}/book?depth=1`);
    ok(`${m.symbol}: depth=1 gives at most one level per side`, d1.body.bids.length <= 1 && d1.body.asks.length <= 1);
    const candles = await get(`/v1/markets/${m.symbol}/candles?interval=1m&limit=50`);
    valid(`GET candles ${m.symbol}`, 'CandleList', candles.body);
    const cs = candles.body.candles;
    ok(`${m.symbol}: candles oldest first, aligned to 1m`, cs.every((c, i) => c.t % 60000 === 0 && (i === 0 || c.t > cs[i - 1].t)));
    ok(`${m.symbol}: candle h >= o,c >= l`, cs.every((c) => { const [o, h, l, cl] = [c.o, c.h, c.l, c.c].map((v) => scaleInt(v, m.priceDecimals)); return h >= o && h >= cl && l <= o && l <= cl && l > 0n; }));
    const oracle = await get(`/v1/oracle/${m.symbol}`);
    valid(`GET oracle ${m.symbol}`, 'OracleStatus', oracle.body);
    const funding = await get(`/v1/markets/${m.symbol}/funding?limit=5`);
    valid(`GET funding ${m.symbol}`, 'FundingRateList', funding.body);
  }
  for (const iv of ['1m', '5m', '15m', '1h', '4h', '1d']) {
    const c = await get(`/v1/markets/${sym}/candles?interval=${iv}&limit=3`);
    valid(`GET candles ${iv}`, 'CandleList', c.body);
  }
  // Pagination of trades: newest first, the cursor continues without overlap.
  const t1 = await get(`/v1/markets/${sym}/trades?limit=5`);
  valid('GET trades', 'TradeList', t1.body);
  const ids1 = t1.body.trades.map((t) => BigInt(t.tradeId));
  ok('trades newest first', ids1.every((v, i) => i === 0 || v < ids1[i - 1]));
  if (t1.body.nextCursor) {
    const t2 = await get(`/v1/markets/${sym}/trades?limit=5&before=${t1.body.nextCursor}`);
    valid('GET trades page 2', 'TradeList', t2.body);
    const ids2 = t2.body.trades.map((t) => BigInt(t.tradeId));
    ok('trades page 2 strictly older than page 1', ids2.length > 0 && ids2.every((v) => v < ids1.at(-1)));
  }
  await expectError('unknown market', get('/v1/markets/NOPE-USD'), 404, 'NOT_FOUND');
  await expectError('unknown endpoint', get('/v1/nope'), 404, 'NOT_FOUND');
  await expectError('depth out of range', get(`/v1/markets/${sym}/book?depth=501`), 400, 'MALFORMED');
  await expectError('unknown interval', get(`/v1/markets/${sym}/candles?interval=2m`), 400, 'MALFORMED');

  // ---------- finality and the input log ----------
  const committee = await get('/v1/committee');
  valid('GET /v1/committee', 'Committee', committee.body);
  const cp = await get('/v1/finality/latest');
  valid('GET /v1/finality/latest', 'Checkpoint', cp.body);
  const C = cp.body;
  const vaultDomain = { name: 'PosternPerpsVault', version: '1', chainId: 700770, verifyingContract: status.body.network.vault };
  const digest = hex(typedDataHash(vaultDomain, 'Checkpoint', CHECKPOINT_FIELDS, { height: String(C.height), time: String(C.time), stateRoot: C.stateRoot, logHash: C.logHash, withdrawalsRoot: C.withdrawalsRoot, depositsCredited: String(C.depositsCredited), forcedProcessed: String(C.forcedProcessed) }));
  ok('checkpoint digest recomputed from its fields (vault domain)', digest === C.digest, `${digest} vs ${C.digest}`);
  ok('checkpoint has at least threshold signatures', C.signatures.length >= committee.body.threshold);
  ok('checkpoint signatures strictly ascending by validator', C.signatures.every((s, i) => i === 0 || s.validator > C.signatures[i - 1].validator));
  ok('every checkpoint signer is in the committee', C.signatures.every((s) => committee.body.validators.includes(s.validator)));
  let recovered = 0;
  for (const s of C.signatures) {
    try {
      if (recover(unhex(C.digest), unhex(s.signature)) === s.validator) recovered++;
    } catch {}
  }
  ok('every checkpoint signature recovers to its validator (low-s, v 27/28)', recovered === C.signatures.length, `${recovered}/${C.signatures.length}`);
  const cpAt = await get(`/v1/finality/${C.height}`);
  ok('finality/{height} returns the checkpoint at that height', cpAt.status === 200 && cpAt.body.height === C.height);
  const blocks = await get(`/v1/blocks?from=${Math.max(1, C.height - 4)}&limit=5`);
  valid('GET /v1/blocks', 'BlockList', blocks.body);
  const B = blocks.body.blocks;
  ok('blocks consecutive, time strictly increasing', B.every((b, i) => i === 0 || (b.height === B[i - 1].height + 1 && b.time > B[i - 1].time)));
  ok('each block prevLogHash = previous logHash', B.every((b, i) => i === 0 || b.prevLogHash === B[i - 1].logHash));
  ok('logHash = keccak(prev || be64(h) || be64(time) || keccak(inputsEncoded)) (API 3.3, SPEC 3.1)', B.every((b) => hex(keccak256(concat(unhex(b.prevLogHash), be64(b.height), be64(b.time), keccak256(unhex(b.inputsEncoded))))) === b.logHash));
  const atCp = B.find((b) => b.height === C.height);
  if (atCp) ok('checkpoint logHash and stateRoot equal the block\'s', atCp.logHash === C.logHash && atCp.stateRoot === C.stateRoot);
  const one = await get(`/v1/blocks/${B[0].height}`);
  valid('GET /v1/blocks/{h}', 'Block', one.body);
  const stream = await fetch(`${base}/v1/blocks/stream?from=${status.body.latestBlock.height - 2}`);
  const lines = (await stream.text()).trim().split('\n').map((l) => JSON.parse(l));
  ok('blocks/stream is ndjson of Block', stream.headers.get('content-type')?.startsWith('application/x-ndjson') && lines.length >= 1 && lines.every((l) => V.validateDef('Block', l).length === 0));

  // ---------- accounts ----------
  const info = mock ? (await get('/mock/info')).body : null;
  const demo = info?.demoAccount ?? addressOf(keyOf('reader'));
  const acc = await get(`/v1/accounts/${demo}`);
  valid('GET /v1/accounts/{a}', 'Account', acc.body);
  const usd = (s) => scaleInt(s, 6) * (s.startsWith('-') ? -1n : 1n);
  const iso = acc.body.positions.reduce((s, p) => s + (p.isolatedMargin ? usd(p.isolatedMargin) : 0n), 0n);
  ok('equity = collateral + unrealizedPnl + isolated margins', usd(acc.body.equity) === usd(acc.body.collateral) + usd(acc.body.unrealizedPnl) + iso, JSON.stringify(acc.body).slice(0, 200));
  ok('withdrawable <= collateral', usd(acc.body.withdrawable) <= usd(acc.body.collateral) || usd(acc.body.collateral) < 0n);
  for (const [path, def] of [['orders?status=open', 'OrderList'], ['orders?status=all&limit=5', 'OrderList'], ['fills?limit=5', 'FillList'], ['funding?limit=5', 'FundingPaymentList'], ['deposits', 'DepositList'], ['withdrawals', 'WithdrawalList'], ['agents', 'AgentList'], ['nonce', 'NonceInfo']]) {
    const r = await get(`/v1/accounts/${demo}/${path}`);
    valid(`GET /v1/accounts/{a}/${path}`, def, r.body);
  }
  const zero = await get(`/v1/accounts/${addressOf(keyOf(`unknown/${runId}`))}`);
  ok('unknown address returns 200 (an account, not 404)', zero.status === 200 && V.validateDef('Account', zero.body).length === 0);
  const proof = await get(`/v1/accounts/${demo}/proof`);
  if (proof.status === 200) valid('GET /v1/accounts/{a}/proof', 'AccountProof', proof.body);
  const wds = (await get(`/v1/accounts/${demo}/withdrawals`)).body.withdrawals;
  for (const w of wds.filter((w) => w.certificate)) {
    const c = w.certificate;
    let n = 0;
    for (const s of c.signatures) {
      try {
        if (recover(unhex(c.checkpoint.digest), unhex(s.signature)) === s.validator && committee.body.validators.includes(s.validator)) n++;
      } catch {}
    }
    ok(`withdrawal ${w.withdrawalId} (${w.status}): certificate has threshold valid committee signatures`, n >= committee.body.threshold, `${n}`);
    const one = await get(`/v1/withdrawals/${w.withdrawalId}`);
    valid(`GET /v1/withdrawals/${w.withdrawalId}`, 'Withdrawal', one.body);
  }
  ok('no certificate before "certified" (API 4.1)', wds.every((w) => (['requested', 'included', 'finalized', 'rejected'].includes(w.status) ? w.certificate === null : true)));
  const deps = (await get(`/v1/accounts/${demo}/deposits`)).body.deposits;
  if (deps.length) {
    const d = await get(`/v1/deposits/${deps[0].l2TxHash}`);
    valid('GET /v1/deposits/{tx}', 'DepositList', d.body);
  }
  const forced = await get(`/v1/forced?account=${demo}`);
  valid('GET /v1/forced', 'ForcedList', forced.body);
  const geo = await get('/v1/geo');
  valid('GET /v1/geo', 'GeoStatus', geo.body);

  // ---------- actions: admission ----------
  const main = keyOf(`main/${runId}`), mainAddr = addressOf(main);
  const other = keyOf(`other/${runId}`);
  const T = () => get('/v1/status').then((r) => r.body.time);
  const typed = (type, msg) => typedDataHash(domain, type, eip.body.types[type], msg);
  const signed = (type, msg, key) => ({ type, message: msg, signature: hex(sign(typed(type, msg), key)), wire: true });
  const hdr = mock ? { 'x-mock-country': 'ZZ' } : {};
  let nonce = BigInt(await T());
  const nextNonce = () => String(nonce++);
  const cancelAll = (key, acct) => ({ account: acct, nonce: nextNonce(), marketId: 0 });
  // 1. a valid action from a fresh key is accepted (exit-only type, so geoblocking cannot refuse it)
  const m1 = cancelAll(main, mainAddr);
  const r1 = await post('/v1/actions', signed('CancelAll', m1, main), hdr);
  ok('valid CancelAll accepted (200)', r1.status === 200, JSON.stringify(r1.body));
  valid('POST /v1/actions ack', 'ActionAck', r1.body);
  const expectedHash = hex(keccak256(concat(typed('CancelAll', m1), unhex(mainAddr))));
  ok('actionHash = keccak256(digest || signer) (SPEC 2.2)', r1.body?.actionHash === expectedHash, `${r1.body?.actionHash} vs ${expectedHash}`);
  const ackGet = await get(`/v1/actions/${r1.body?.actionHash}`);
  ok('GET /v1/actions/{hash} 200', ackGet.status === 200);
  valid('GET /v1/actions/{hash}', 'ActionAck', ackGet.body);
  // 2. replay
  await expectError('replayed action', post('/v1/actions', signed('CancelAll', m1, main), hdr), 400, 'NONCE_USED');
  // 3. nonce window
  await expectError('nonce far in the future', post('/v1/actions', signed('CancelAll', { account: mainAddr, nonce: String(BigInt(await T()) + 3_600_000n), marketId: 0 }, main), hdr), 400, 'NONCE_WINDOW');
  await expectError('nonce far in the past', post('/v1/actions', signed('CancelAll', { account: mainAddr, nonce: String(BigInt(await T()) - 3_600_000n), marketId: 0 }, main), hdr), 400, 'NONCE_WINDOW');
  // 4. non-canonical signatures
  const m4 = cancelAll(main, mainAddr);
  const good = signed('CancelAll', m4, main);
  const sig = unhex(good.signature);
  const high = sig.slice();
  const s = BigInt(hex(sig.slice(32, 64)));
  high.set(unhex('0x' + (N - s).toString(16).padStart(64, '0')), 32);
  high[64] = sig[64] === 27 ? 28 : 27;
  await expectError('high-s signature', post('/v1/actions', { ...good, signature: hex(high) }, hdr), 400, 'BAD_SIGNATURE');
  const v0 = sig.slice();
  v0[64] -= 27;
  await expectError('v = 0/1 signature', post('/v1/actions', { ...good, signature: hex(v0) }, hdr), 400, 'BAD_SIGNATURE');
  await expectError('64-byte signature', post('/v1/actions', { ...good, signature: hex(sig.slice(0, 64)) }, hdr), 400, 'BAD_SIGNATURE');
  // 5. another key signing for the account
  await expectError('signature by a stranger', post('/v1/actions', signed('CancelAll', cancelAll(other, mainAddr), other), hdr), 403, 'UNKNOWN_SIGNER');
  // 6. malformed
  await expectError('unknown action type', post('/v1/actions', { ...good, type: 'Teleport' }, hdr), 400, 'MALFORMED');
  await expectError('unknown message field', post('/v1/actions', { ...good, message: { ...m4, extra: 1 } }, hdr), 400, 'MALFORMED');
  await expectError('non-canonical integer', post('/v1/actions', { ...good, message: { ...m4, nonce: '0' + m4.nonce } }, hdr), 400, 'MALFORMED');
  await expectError('body not JSON', post('/v1/actions', '{nope', hdr), 400, 'MALFORMED');
  await expectError('body over 16 KiB', post('/v1/actions', JSON.stringify({ ...good, pad: 'x'.repeat(17000) }), hdr), 413, 'TOO_LARGE');
  // 7. a signed field changed after signing never verifies as the same signer
  const tampered = { ...good, message: { ...m4, marketId: 2 } };
  const rt = await post('/v1/actions', tampered, hdr);
  ok('tampered message refused (signature binds the typed fields)', rt.status === 403 || rt.status === 400, `${rt.status}`);
  // 8. PlaceOrder of an unfunded account: admitted, then the engine's outcome is a business rejection
  const isBuyPrice = (m) => {
    const t = scaleInt(m.tickSize, 8);
    const best = scaleInt(tickers.body.tickers.find((x) => x.symbol === m.symbol).markPrice, 8);
    return String(((best / 2n) / t) * t);
  };
  const po = { account: mainAddr, nonce: nextNonce(), marketId: mk.marketId, isBuy: true, orderType: 0, timeInForce: 0, price: isBuyPrice(mk), size: String(scaleInt(mk.minOrderSize, 8)), reduceOnly: false, triggerPrice: '0', clientOrderId: '0x' + '00'.repeat(32) };
  const rpo = await post('/v1/actions?wait=included', signed('PlaceOrder', po, main), hdr);
  valid('PlaceOrder ack', 'ActionAck', rpo.body);
  ok('PlaceOrder included within 2 s with an engine result', rpo.body?.status === 'included' && typeof rpo.body?.result?.ok === 'boolean', JSON.stringify(rpo.body));
  // 9. price 0 is MALFORMED (SPEC 2.3: no "any price" order)
  const p0 = { ...po, nonce: nextNonce(), orderType: 1, timeInForce: 1, price: '0' };
  const r0 = await post('/v1/actions?wait=included', signed('PlaceOrder', p0, main), hdr);
  ok('market order with price 0 never executes (MALFORMED)', r0.body?.error?.code === 'MALFORMED' || r0.body?.result?.error === 'MALFORMED', JSON.stringify(r0.body));
  // 10. agents: approve, act, and the agent cannot withdraw
  const agentKey = keyOf(`agent/${runId}`), agentAddr = addressOf(agentKey);
  const T0 = BigInt(await T());
  const ra = await post('/v1/actions?wait=included', signed('ApproveAgent', { account: mainAddr, nonce: nextNonce(), agent: agentAddr, name: 'conformance', expiresAt: String(T0 + 86_400_000n) }, main), hdr);
  ok('ApproveAgent included and applied', ra.body?.status === 'included' && ra.body?.result?.ok === true, JSON.stringify(ra.body));
  let an = BigInt(await T());
  const ra2 = await post('/v1/actions', signed('CancelAll', { account: mainAddr, nonce: String(an++), marketId: 0 }, agentKey), hdr);
  ok('agent CancelAll accepted', ra2.status === 200, JSON.stringify(ra2.body));
  await expectError('agent Withdraw', post('/v1/actions', signed('Withdraw', { account: mainAddr, nonce: String(an++), amount: '2000000', recipient: agentAddr }, agentKey), hdr), 403, 'AGENT_NOT_ALLOWED');
  await expectError('agent ApproveAgent', post('/v1/actions', signed('ApproveAgent', { account: mainAddr, nonce: String(an++), agent: addressOf(keyOf(`agent2/${runId}`)), name: 'x', expiresAt: String(T0 + 1000n) }, agentKey), hdr), 403, 'AGENT_NOT_ALLOWED');
  const ags = await get(`/v1/accounts/${mainAddr}/agents`);
  ok('approved agent listed', ags.body?.agents?.some((g) => g.agent === agentAddr));

  // ---------- geoblocking (mock and dev: the country comes from X-Mock-Country) ----------
  if (mock) {
    const br = { 'x-mock-country': 'BR' };
    const g = await get('/v1/geo', br);
    valid('GET /v1/geo (BR)', 'GeoStatus', g.body);
    ok('BR is restricted with the exit-only policy', g.body.restricted === true && g.body.policy === 'exit_only');
    const gUnknown = await get('/v1/geo', { 'x-mock-country': 'QQ' });
    ok('an unlisted country is restricted (deny by default)', gUnknown.body.restricted === true);
    const openOrder = { ...po, nonce: nextNonce() };
    await expectError('BR opening order', post('/v1/actions', signed('PlaceOrder', openOrder, main), br), 451, 'GEO_RESTRICTED');
    const ex = await post('/v1/actions', signed('CancelAll', { account: mainAddr, nonce: nextNonce(), marketId: 0 }, main), br);
    ok('BR CancelAll accepted (exit-only)', ex.status === 200, JSON.stringify(ex.body));
    const ro = await post('/v1/actions', signed('PlaceOrder', { ...po, nonce: nextNonce(), reduceOnly: true }, main), br);
    ok('BR reduce-only PlaceOrder admitted (exit-only)', ro.status === 200, JSON.stringify(ro.body));
    const ua = await post('/v1/actions', signed('UpdateLeverage', { account: mainAddr, nonce: nextNonce(), marketId: mk.marketId, leverage: 2, isolated: false }, main), br);
    ok('BR UpdateLeverage refused (not exit-only)', ua.status === 451, `${ua.status}`);
    // sanctions: frozen for every action, exit-only included
    const sk = keyOf(`sanctioned/${runId}`), sa = addressOf(sk);
    await get(`/mock/scenario?sanction=${sa}`);
    await expectError('sanctioned CancelAll', post('/v1/actions', signed('CancelAll', { account: sa, nonce: nextNonce(), marketId: 0 }, sk), hdr), 451, 'SANCTIONED');
    await get(`/mock/scenario?unsanction=${sa}`);
    // halted market
    await get(`/mock/scenario?halt=${sym}`);
    const h = await get(`/v1/markets/${sym}`);
    ok('halted market reports status halted', h.body.status === 'halted');
    await expectError('PlaceOrder on a halted market', post('/v1/actions', signed('PlaceOrder', { ...po, nonce: nextNonce() }, main), hdr), 409, 'MARKET_HALTED');
    const hc = await post('/v1/actions', signed('CancelAll', { account: mainAddr, nonce: nextNonce(), marketId: mk.marketId }, main), hdr);
    ok('CancelAll on a halted market admitted', hc.status === 200);
    await get(`/mock/scenario?resume=${sym}`);
    // sequencer down
    await get('/mock/scenario?sequencer=recovering');
    const sst = await get('/v1/status');
    ok('status shows sequencer recovering', sst.body.sequencer.state === 'recovering');
    await expectError('action while the sequencer recovers', post('/v1/actions', signed('CancelAll', { account: mainAddr, nonce: nextNonce(), marketId: 0 }, main), hdr), 503, 'SEQUENCER_UNAVAILABLE');
    await get('/mock/scenario?sequencer=running');
    // typed-data drift is visible to clients
    await get('/mock/scenario?eip712=drift');
    const drift = await get('/v1/eip712');
    ok('eip712=drift publishes different typed data', JSON.stringify(drift.body.types) !== JSON.stringify(eip.body.types));
    await get('/mock/scenario?eip712=pinned');
    const back = await get('/mock/info');
    ok('mock conservation invariant holds exactly (SPEC I-2 analogue)', back.body.conservation.ok === true && back.body.conservation.sizesZero === true, JSON.stringify(back.body.conservation));
  }

  // ---------- WebSocket ----------
  await wsChecks(base, V, ok, valid, { sym, demo, mainAddr, signedCancel: () => signed('CancelAll', { account: mainAddr, nonce: nextNonce(), marketId: 0 }, main), mock });

  const failed = results.filter((r) => !r.ok);
  return { total: results.length, passed: results.length - failed.length, failed, results };
}

async function wsChecks(base, V, ok, valid, { sym, demo, mainAddr, signedCancel, mock }) {
  const url = base.replace(/^http/, 'ws') + '/v1/ws' + (mock ? '?country=ZZ' : '');
  const ws = new WebSocket(url);
  const msgs = [];
  const waiters = [];
  ws.onmessage = (e) => {
    const m = JSON.parse(e.data);
    msgs.push(m);
    for (const w of [...waiters]) if (w.pred(m)) { waiters.splice(waiters.indexOf(w), 1); w.res(m); }
  };
  await new Promise((r, j) => { ws.onopen = r; ws.onerror = j; });
  const wait = (pred, ms = 4000) => new Promise((res) => {
    const found = msgs.find(pred);
    if (found) return res(found);
    const w = { pred, res };
    waiters.push(w);
    setTimeout(() => { const i = waiters.indexOf(w); if (i >= 0) { waiters.splice(i, 1); res(null); } }, ms);
  });
  const sub = (o) => ws.send(JSON.stringify({ op: 'subscribe', ...o }));
  sub({ id: 1, channel: 'book', symbol: sym, depth: 20 });
  sub({ id: 2, channel: 'trades', symbol: sym });
  sub({ id: 3, channel: 'ticker', symbol: '*' });
  sub({ id: 4, channel: 'candles', symbol: sym, interval: '1m' });
  sub({ id: 5, channel: 'account', account: demo });
  sub({ id: 6, channel: 'status' });
  sub({ id: 7, channel: 'nope' });
  ws.send(JSON.stringify({ op: 'ping' }));
  for (let i = 1; i <= 6; i++) ok(`ws subscribed id ${i}`, !!(await wait((m) => m.op === 'subscribed' && m.id === i)));
  ok('ws unknown channel is an error', !!(await wait((m) => m.op === 'error' && m.id === 7 && m.error.code === 'MALFORMED')));
  ok('ws pong', !!(await wait((m) => m.op === 'pong')));
  const bookSnap = await wait((m) => m.channel === 'book' && m.type === 'snapshot');
  ok('ws book snapshot', !!bookSnap);
  for (const [ch, type] of [['trades', 'snapshot'], ['candles', 'snapshot'], ['account', 'snapshot'], ['ticker', 'update'], ['status', 'update']]) ok(`ws ${ch} ${type}`, !!(await wait((m) => m.channel === ch && m.type === type)));
  // Produce traffic (in static mode the mock makes a block per action).
  ws.send(JSON.stringify({ op: 'action', id: 9, request: signedCancel() }));
  ok('ws action op answered with actionResult', !!(await wait((m) => m.op === 'actionResult' && m.id === 9)));
  if (mock) await fetch(base + '/mock/scenario?step=10');
  await wait((m) => m.channel === 'book' && m.type === 'delta', 6000);
  // Every message validates; book deltas chain by prevSeq from the snapshot.
  const bad = msgs.map((m) => [m, V.validateDef('WsMessage', m)]).filter(([, e]) => e.length);
  ok(`ws: all ${msgs.length} messages match WsMessage`, bad.length === 0, bad.slice(0, 2).map(([m, e]) => `${JSON.stringify(m).slice(0, 120)} :: ${e.slice(0, 2).join('; ')}`).join(' | '));
  const deltas = msgs.filter((m) => m.channel === 'book' && m.type === 'delta');
  let seq = bookSnap?.data.seq;
  let chain = deltas.length > 0;
  for (const d of deltas) {
    if (d.data.prevSeq !== seq) chain = false;
    seq = d.data.seq;
  }
  ok(`ws book deltas chain by prevSeq from the snapshot (${deltas.length} deltas)`, chain);
  ws.close();
}

// ---------- CLI ----------
if (process.argv[1] && resolve(process.argv[1]) === fileURLToPath(import.meta.url)) {
  const args = process.argv.slice(2);
  const opt = (n, d) => (args.includes(`--${n}`) ? args[args.indexOf(`--${n}`) + 1] : d);
  const r = await runConformance(opt('base', 'http://127.0.0.1:49300'), { mock: args.includes('--mock'), schema: opt('schema', undefined) });
  if (args.includes('--json')) process.stdout.write(JSON.stringify(r, null, 1) + '\n');
  else {
    for (const f of r.failed) process.stdout.write(`FAIL ${f.name}: ${f.detail}\n`);
    process.stdout.write(`conformance: ${r.passed}/${r.total} checks passed (schema ${schemaPath(opt('schema', undefined))})\n`);
  }
  process.exit(r.failed.length ? 1 : 0);
}

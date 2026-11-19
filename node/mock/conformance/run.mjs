#!/usr/bin/env node
// Shared API conformance tests for the PosternDEX Perpetuals node: run against the mock (node/mock/server.mjs) or
// the real node in dev mode, unchanged. UNAUDITED prototype.
//
//   node node/mock/conformance/run.mjs --base http://127.0.0.1:49300 [--mock] [--funded [LABEL]] [--funded2 [LABEL]] [--schema FILE] [--json]
//   node node/mock/conformance/run.mjs --print-accounts [--funded LABEL] [--funded2 LABEL]
//
// --funded2 (real node only, API 0.3): a SECOND funded throwaway account, so the suite can make a real fill between
// two accounts and check fills, trades, positions, orders, candles, the ticker and the fees charged against the
// published fee rates. Without it those checks are SKIPPED with that reason, never passed.
//
// Real node: checks that need an account with collateral (agents, price-0 MALFORMED, book deltas) are SKIPPED, never
// passed, unless --funded: then the suite signs as the throwaway account derived from LABEL (default 'funded';
// address from --print-accounts), which the operator funds through the dev vault on the L2 before the run.
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
import { decodeInputs } from '../lib/encoding.mjs';
import { leafHash, root32, decodeAccountLeaf, encodeWithdrawalLeaf } from '../lib/merkle32.mjs';

const HERE = dirname(fileURLToPath(import.meta.url));
const REPO = resolve(HERE, '..', '..', '..');
const enc = new TextEncoder();
const keyOf = (label) => keccak256(enc.encode(`postern-perps-conformance/${label}`));
const be64 = (v) => {
  const b = new Uint8Array(8);
  new DataView(b.buffer).setBigUint64(0, BigInt(v));
  return b;
};
// docs/VAULT.md v0.2 section 5.2 (390 27c1969): the sixteen fields the committee signs; the first twelve are the
// inputs of SPEC 11's stateRoot. Transcribed independently of the mock's code.
const CHECKPOINT_FIELDS = [
  ['height', 'uint64'], ['time', 'uint64'], ['logHash', 'bytes32'], ['accountsRoot', 'bytes32'], ['marketsRoot', 'bytes32'],
  ['withdrawalsRoot', 'bytes32'], ['nextDepositIndex', 'uint64'], ['nextForcedIndex', 'uint64'], ['nextOrderId', 'uint64'],
  ['totalEscape', 'uint128'], ['dust', 'uint128'], ['auxHash', 'bytes32'], ['depositsHead', 'bytes32'], ['forcedHead', 'bytes32'],
  ['withdrawalCount', 'uint64'], ['withdrawalsTotal', 'uint64']].map(([name, type]) => ({ name, type }));
const be = (v, n) => {
  const b = new Uint8Array(n);
  let x = BigInt(v);
  for (let i = n - 1; i >= 0; i--) { b[i] = Number(x & 255n); x >>= 8n; }
  return b;
};
/** "4.5" (bps, as /v1/markets publishes fee rates) -> 45000 (units of 1e-4 bps = ppm x 100); exact or refused. */
function bpsScaled(s) {
  const [i, f = ''] = s.split('.');
  if (!/^\d+$/.test(i) || !/^\d{0,4}$/.test(f)) throw new Error(`fee rate ${s}`);
  return BigInt(i + f.padEnd(4, '0'));
}
/** SPEC S-9.3 from PUBLISHED values: ceil(price x size x feePpm / 1e6 / 1e10) USDC units, price and size in 1e-8. */
export function publishedFee(price8, size8, bps) {
  return (BigInt(price8) * BigInt(size8) * bpsScaled(bps) + 10n ** 18n - 1n) / 10n ** 18n;
}
export function stateRootOfForTest(c) {
  return stateRootOf(c);
}
/** VAULT.md 5.2 digest of a checkpoint object as the API serves it, under the vault's own domain (for the e2e rehearsal). */
export function checkpointDigest(vault, C, chainId = 700770) {
  const domain = { name: 'PosternPerpsVault', version: '1', chainId, verifyingContract: vault };
  return hex(typedDataHash(domain, 'Checkpoint', CHECKPOINT_FIELDS, Object.fromEntries(CHECKPOINT_FIELDS.map((f) => [f.name, typeof C[f.name] === 'number' ? String(C[f.name]) : C[f.name]]))));
}
export { CHECKPOINT_FIELDS };
function stateRootOf(c) {
  const sys = keccak256(concat(enc.encode('PPv1sys'), be(c.nextDepositIndex, 8), be(c.nextForcedIndex, 8), be(c.nextOrderId, 8), be(c.totalEscape, 16), be(c.dust, 16), unhex(c.auxHash), be(c.withdrawalCount, 8), be(c.withdrawalsTotal, 8)));
  return hex(keccak256(concat(enc.encode('PosternPerps/state/v1'), be(c.height, 8), be(c.time, 8), unhex(c.accountsRoot), unhex(c.marketsRoot), unhex(c.withdrawalsRoot), sys)));
}


export function schemaPath(explicit) {
  if (explicit) return explicit;
  const shared = join(REPO, 'node', 'api', 'schema.json');
  return existsSync(shared) ? shared : join(HERE, '..', 'schema', 'api.schema.json');
}

export async function runConformance(base, opts = {}) {
  const results = [];
  try {
    await suite(base, opts, results);
  } catch (e) {
    // A server that breaks an assumption the suite needs (a missing field, a refused request) fails the run with
    // the reason; it never passes by aborting.
    results.push({ name: 'suite aborted', ok: false, detail: String(e?.stack ?? e).slice(0, 400) });
  }
  const failed = results.filter((r) => r.ok === false);
  const skipped = results.filter((r) => r.ok === null);
  return { total: results.length, passed: results.filter((r) => r.ok === true).length, failed, skipped, results };
}

/** The deterministic account a real-node run uses with --funded (fund it on the L2 vault first). */
export const fundedAccount = (label = 'funded') => addressOf(keyOf(label));

/** The second deterministic account of a real-node run (--funded2). */
export const fundedAccount2 = (label = 'funded2') => addressOf(keyOf(label));

async function suite(base, { mock = false, schema, runId = String(Date.now()), funded = null, funded2 = null } = {}, results) {
  const V = new Validator(JSON.parse(readFileSync(schemaPath(schema), 'utf8')));
  const ok = (name, cond, detail = '') => {
    results.push({ name, ok: !!cond, detail: cond ? '' : String(detail).slice(0, 400) });
    return !!cond;
  };
  // A check that needs a funded account on a real node: SKIPPED (never passed) without --funded.
  const canFund = mock || funded !== null;
  const needsFunds = (name) => {
    if (canFund) return true;
    results.push({ name: `${name} (needs a funded account: --funded)`, ok: null, detail: 'skipped' });
    return false;
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
  // A real node makes blocks only from inputs and checkpoints every few blocks: with a funded account the suite
  // produces blocks (CancelAll, an exit-only action) until a checkpoint exists, at most 40 actions / 60 s.
  let cp = await get('/v1/finality/latest');
  if (cp.status === 404 && canFund && !mock) {
    const wk = keyOf(funded), wa = addressOf(wk);
    let wn = BigInt((await get('/v1/status')).body.time);
    for (let i = 0; i < 40 && cp.status === 404; i++) {
      const msg = { account: wa, nonce: String(wn++), marketId: 0 };
      await post('/v1/actions?wait=included', { type: 'CancelAll', message: msg, signature: hex(sign(typedDataHash(domain, 'CancelAll', eip.body.types.CancelAll, msg), wk)), wire: true }, {});
      await new Promise((r) => setTimeout(r, 1000));
      cp = await get('/v1/finality/latest');
    }
  }
  if (!ok('a finalized checkpoint exists (GET /v1/finality/latest 200)', cp.status === 200, `HTTP ${cp.status}`)) throw new Error('no checkpoint: the finality checks cannot run');
  valid('GET /v1/finality/latest', 'Checkpoint', cp.body);
  const C = cp.body;
  const vaultDomain = { name: 'PosternPerpsVault', version: '1', chainId: 700770, verifyingContract: status.body.network.vault };
  let digest = null, why = '';
  try {
    digest = hex(typedDataHash(vaultDomain, 'Checkpoint', CHECKPOINT_FIELDS, Object.fromEntries(CHECKPOINT_FIELDS.map((f) => [f.name, typeof C[f.name] === 'number' ? String(C[f.name]) : C[f.name]]))));
  } catch (e) {
    why = `cannot recompute: ${e.message}`;
  }
  ok('checkpoint digest recomputed from its sixteen fields (VAULT.md 5.2, vault domain)', digest !== null && digest === C.digest, why || `${digest} vs ${C.digest}`);
  let sr = null;
  try {
    sr = stateRootOf(C);
  } catch (e) {
    why = e.message;
  }
  ok('checkpoint stateRoot = SPEC 11 equation (systemHash with withdrawalCount and withdrawalsTotal, VAULT.md 64072cb)', sr !== null && sr === C.stateRoot, `${sr} vs ${C.stateRoot}`);
  ok('withdrawalCount 0 means withdrawalsTotal 0 (VAULT.md 5.3 rule 5)', C.withdrawalCount !== 0 || C.withdrawalsTotal === '0');
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
  // inputsEncoded (SPEC 3.2 canonical bytes) says exactly what `inputs` says; every action signature recovers to
  // the block's named signer under the published domain.
  const checkInputs = async (label, needActions) => {
  let decodedOk = true, sigOk = true, nActions = 0, nOracle = 0, why = '';
  const recent = (await get(`/v1/blocks?from=${Math.max(1, (await get('/v1/status')).body.latestBlock.height - 60)}&limit=61`)).body.blocks;
  for (const b of recent) {
    let dec;
    try {
      dec = decodeInputs(unhex(b.inputsEncoded), eip.body);
    } catch (e) {
      decodedOk = false;
      why = `${b.height}: ${e.message}`;
      continue;
    }
    if (dec.length !== b.inputs.length) { decodedOk = false; why = `${b.height}: count`; continue; }
    dec.forEach((d, i) => {
      const j = b.inputs[i];
      const same = d.kind === j.kind && (d.kind === 'action' ? d.type === j.type && JSON.stringify(d.message) === JSON.stringify(j.message) && d.signature === j.signature : JSON.stringify(d.payload) === JSON.stringify(j.payload));
      if (!same) { decodedOk = false; why = `${b.height}[${i}]: ${JSON.stringify(d).slice(0, 120)} vs ${JSON.stringify(j).slice(0, 120)}`; }
      if (d.kind === 'oracle') nOracle++;
      if (d.kind === 'action') {
        nActions++;
        try {
          const who = recover(typedDataHash(domain, d.type, eip.body.types[d.type], d.message), unhex(d.signature));
          if (j.signer ? who !== j.signer : who !== d.message.account) sigOk = false;
        } catch {
          sigOk = false;
        }
      }
    });
  }
  ok(`${label}: inputsEncoded decodes (SPEC 3.2, strict) to exactly the block's inputs (${recent.length} blocks, ${nActions} actions, ${nOracle} oracle)`, decodedOk, why);
  ok(`${label}: every action input recovers to its signer under the published domain`, sigOk);
  if (needActions) ok(`${label}: the recent log carries action inputs`, nActions > 0);
  };

  ok('blocks consecutive, time strictly increasing', B.every((b, i) => i === 0 || (b.height === B[i - 1].height + 1 && b.time > B[i - 1].time)));
  ok('each block prevLogHash = previous logHash', B.every((b, i) => i === 0 || b.prevLogHash === B[i - 1].logHash));
  ok('logHash = keccak(prev || be64(h) || be64(time) || keccak(inputsEncoded)) (API 3.3, SPEC 3.1)', B.every((b) => hex(keccak256(concat(unhex(b.prevLogHash), be64(b.height), be64(b.time), keccak256(unhex(b.inputsEncoded))))) === b.logHash));
  const atCp = B.find((b) => b.height === C.height);
  if (atCp) ok('checkpoint logHash and stateRoot equal the block\'s', atCp.logHash === C.logHash && atCp.stateRoot === C.stateRoot);
  await checkInputs('log', false);
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
  // The account proof is the escape hatch's input (VAULT.md claimEscape): checked bit by bit, never only by shape.
  const proofOf = funded !== null && !mock ? addressOf(keyOf(funded)) : demo;
  const proof = await get(`/v1/accounts/${proofOf}/proof`);
  if (proof.status === 200) {
    valid('GET /v1/accounts/{a}/proof', 'AccountProof', proof.body);
    const P = proof.body;
    let dec = null, root = null, why = '';
    try {
      dec = decodeAccountLeaf(unhex(P.leafData.encoded));
      root = root32(leafHash(unhex(P.leafData.encoded)), P.index, P.proof);
    } catch (e) {
      why = e.message;
    }
    ok('AccountProof: leafData.encoded is the 88-byte SPEC 11 account leaf of exactly the named fields', dec !== null && dec.account === P.leafData.account && dec.account === proofOf && dec.index === P.leafData.index && dec.balance === P.leafData.balance && dec.escapeValue === P.leafData.escapeValue && dec.positionsHash === P.leafData.positionsHash, why || JSON.stringify([dec, P.leafData]).slice(0, 300));
    ok('AccountProof: leaf = keccak256(0x00 || encoded) and index = the leaf\'s own index', why === '' && hex(leafHash(unhex(P.leafData.encoded))) === P.leaf && String(P.leafData.index) === P.index, why);
    ok('AccountProof: the 32 siblings walk from the leaf to the checkpoint\'s accountsRoot', root !== null && root === P.checkpoint.accountsRoot, why || `${root} vs ${P.checkpoint.accountsRoot}`);
    ok('AccountProof: its checkpoint is final and its state root follows from its fields', P.checkpoint.final === true && stateRootOf(P.checkpoint) === P.checkpoint.stateRoot && P.checkpoint.signatures.length >= committee.body.threshold);
    ok('AccountProof: escapeValue fits the checkpoint (not above totalEscape)', BigInt(P.leafData.escapeValue) <= BigInt(P.checkpoint.totalEscape));
  } else if (canFund && status.body.mode !== 'mock') {
    // (a mock creates the account at its first read, after its last checkpoint: only a real node must answer)
    ok('GET /v1/accounts/{a}/proof of the funded account answers 200 once a checkpoint is final', false, `HTTP ${proof.status} ${JSON.stringify(proof.body).slice(0, 200)}`);
  }
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
  const main = funded !== null && !mock ? keyOf(funded) : keyOf(`main/${runId}`), mainAddr = addressOf(main);
  if (funded !== null && !mock) {
    const fa = await get(`/v1/accounts/${mainAddr}`);
    ok(`--funded account ${mainAddr} has collateral (fund it on the vault before the run)`, fa.status === 200 && !/^-|^0(\.0*)?$/.test(fa.body?.collateral ?? '0'), JSON.stringify(fa.body).slice(0, 200));
  }
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
    // Half the mark, on the tick; one tick when the node has no mark yet (no oracle input): never price 0.
    const half = ((best / 2n) / t) * t;
    return String(half >= t ? half : t);
  };
  const po = { account: mainAddr, nonce: nextNonce(), marketId: mk.marketId, isBuy: true, orderType: 0, timeInForce: 0, price: isBuyPrice(mk), size: String(scaleInt(mk.minOrderSize, 8)), reduceOnly: false, triggerPrice: '0', clientOrderId: '0x' + '00'.repeat(32) };
  const rpo = await post('/v1/actions?wait=included', signed('PlaceOrder', po, main), hdr);
  valid('PlaceOrder ack', 'ActionAck', rpo.body);
  ok('PlaceOrder included within 2 s with an engine result', rpo.body?.status === 'included' && typeof rpo.body?.result?.ok === 'boolean', JSON.stringify(rpo.body));
  // The market table the API publishes is the one the engine enforces: an order on the published tick, step and
  // minimum size is never refused for TICK, STEP or SIZE_BOUNDS (a client that follows /v1/markets must not be
  // refused for following it).
  if (canFund) ok('an order on /v1/markets\' tick, step and minimum is not refused TICK/STEP/SIZE_BOUNDS', !['TICK', 'STEP', 'SIZE_BOUNDS'].includes(rpo.body?.result?.error), JSON.stringify(rpo.body?.result));
  // 9. price 0 is MALFORMED (SPEC 2.3: no "any price" order); a real engine checks the account first
  if (needsFunds('market order with price 0 never executes (MALFORMED)')) {
    const p0 = { ...po, nonce: nextNonce(), orderType: 1, timeInForce: 1, price: '0' };
    const r0 = await post('/v1/actions?wait=included', signed('PlaceOrder', p0, main), hdr);
    ok('market order with price 0 never executes (MALFORMED)', r0.body?.error?.code === 'MALFORMED' || r0.body?.result?.error === 'MALFORMED', JSON.stringify(r0.body));
  }
  // 10. agents: approve, act, and the agent cannot withdraw; revoked at the end (at most 4 agents per account)
  if (needsFunds('agent approval, agent rights and listing')) {
    const agentKey = keyOf(`agent/${runId}`), agentAddr = addressOf(agentKey);
    const T0 = BigInt(await T());
    const ra = await post('/v1/actions?wait=included', signed('ApproveAgent', { account: mainAddr, nonce: nextNonce(), agent: agentAddr, name: 'conformance', expiresAt: String(T0 + 600_000n) }, main), hdr);
    ok('ApproveAgent included and applied', ra.body?.status === 'included' && ra.body?.result?.ok === true, JSON.stringify(ra.body));
    let an = BigInt(await T());
    const ra2 = await post('/v1/actions', signed('CancelAll', { account: mainAddr, nonce: String(an++), marketId: 0 }, agentKey), hdr);
    ok('agent CancelAll accepted', ra2.status === 200, JSON.stringify(ra2.body));
    await expectError('agent Withdraw', post('/v1/actions', signed('Withdraw', { account: mainAddr, nonce: String(an++), amount: '2000000', recipient: agentAddr }, agentKey), hdr), 403, 'AGENT_NOT_ALLOWED');
    await expectError('agent ApproveAgent', post('/v1/actions', signed('ApproveAgent', { account: mainAddr, nonce: String(an++), agent: addressOf(keyOf(`agent2/${runId}`)), name: 'x', expiresAt: String(T0 + 1000n) }, agentKey), hdr), 403, 'AGENT_NOT_ALLOWED');
    const ags = await get(`/v1/accounts/${mainAddr}/agents`);
    ok('approved agent listed', ags.body?.agents?.some((g) => g.agent === agentAddr));
    // R436-7 (geoblock.json 0.2.2): RevokeAgent is an exit action. A user in a denied country must be able to end
    // an agent key (it only removes authority); approving one stays refused there. Mock and dev: X-Mock-Country.
    const br = { 'x-mock-country': 'BR' };
    await expectError('ApproveAgent from a geo-denied country', post('/v1/actions', signed('ApproveAgent', { account: mainAddr, nonce: nextNonce(), agent: addressOf(keyOf(`agent3/${runId}`)), name: 'x', expiresAt: String(T0 + 600_000n) }, main), br), 451, 'GEO_RESTRICTED');
    const rv = await post('/v1/actions?wait=included', signed('RevokeAgent', { account: mainAddr, nonce: nextNonce(), agent: agentAddr }, main), br);
    ok('RevokeAgent from a geo-denied country is admitted (exit action, R436-7)', rv.status === 200, `${rv.status} ${JSON.stringify(rv.body)}`);
    ok('RevokeAgent included and applied', rv.body?.status === 'included' && rv.body?.result?.ok === true, JSON.stringify(rv.body));
    const ags2 = await get(`/v1/accounts/${mainAddr}/agents`);
    ok('revoked agent no longer listed', !ags2.body?.agents?.some((g) => g.agent === agentAddr));
  }
  // ---------- a real fill between two funded accounts, and a proven withdrawal (real node, API 0.3) ----------
  // Not run against a server that says it is a mock (its book is moved by simulated makers and takers, so which
  // maker a taker meets is not the suite's choice; the mock's own tests cover its matching).
  if (status.body.mode !== 'mock') {
    const two = funded !== null && funded2 !== null;
    const FILL = 'a fill between two funded accounts: fills, trade, positions, order, candle, ticker, fees';
    if (!two) results.push({ name: `${FILL} (needs two funded accounts: --funded and --funded2)`, ok: null, detail: 'skipped' });
    else {
      const A = { key: main, addr: mainAddr }, B = { key: keyOf(funded2), addr: addressOf(keyOf(funded2)) };
      const tick8 = scaleInt(mk.tickSize, 8), size8 = scaleInt(mk.minOrderSize, 8);
      const book0 = (await get(`/v1/markets/${sym}/book?depth=1`)).body;
      const tk0 = (await get(`/v1/markets/${sym}/ticker`)).body;
      const bid0 = book0.bids[0] ? scaleInt(book0.bids[0][0], 8) : null, ask0 = book0.asks[0] ? scaleInt(book0.asks[0][0], 8) : null;
      const mark8 = (scaleInt(tk0.markPrice, 8) / tick8) * tick8;
      // a price strictly inside the spread (or at the mark on an empty side): B's order is then FIRST at the best price
      let P = mark8;
      if (ask0 !== null && P >= ask0) P = ask0 - tick8;
      if (bid0 !== null && P <= bid0) P = bid0 + tick8;
      const room = P > 0n && (ask0 === null || P < ask0) && (bid0 === null || P > bid0);
      const balB0 = (await get(`/v1/accounts/${B.addr}`)).body;
      ok(`--funded2 account ${B.addr} has collateral (fund it on the vault before the run)`, !/^-|^0(\.0*)?$/.test(balB0?.collateral ?? '0'), JSON.stringify(balB0).slice(0, 160));
      if (!ok('there is a price strictly inside the spread for the two-account fill (a quiet book)', room && mark8 > 0n, JSON.stringify({ book0, mark: tk0.markPrice }))) {
        // nothing more can be checked without a fill; the failed check above says why
      } else {
        const dec8 = (v, dp) => { const t = String(v).padStart(9, '0'); const i = t.slice(0, -8), f = t.slice(-8).slice(0, dp); return dp ? `${i}.${f}` : i; };
        const num = (x) => Number(x);
        const place = (who, isBuy, tif, extra = {}) => post('/v1/actions?wait=included', signed('PlaceOrder', { account: who.addr, nonce: nextNonce(), marketId: mk.marketId, isBuy, orderType: 0, timeInForce: tif, price: String(P), size: String(size8), reduceOnly: false, triggerPrice: '0', clientOrderId: '0x' + '00'.repeat(32), ...extra }, who.key), hdr);
        const bps = bpsScaled;
        const feeOf = (rate) => publishedFee(P, size8, rate);
        const t0 = (await get('/v1/status')).body.time;
        const acc0 = { A: (await get(`/v1/accounts/${A.addr}`)).body, B: balB0 };
        const pos = (a) => a.positions.find((p) => p.symbol === sym);
        ok('both accounts start without a position in the market (the suite leaves them flat)', !pos(acc0.A) && !pos(acc0.B));
        // 1. B rests a post-only bid; A sells into it
        const rb = await place(B, true, 2);
        ok('maker bid: included, result carries orderId and orderStatus open', rb.body?.result?.ok === true && /^[1-9]\d*$/.test(rb.body?.result?.orderId ?? '') && rb.body?.result?.orderStatus === 'open', JSON.stringify(rb.body));
        const oid = rb.body?.result?.orderId;
        const o1 = await get(`/v1/orders/${oid}`);
        valid('GET /v1/orders/{id}', 'Order', o1.body);
        ok('the resting order is open at its price and size', o1.body?.status === 'open' && scaleInt(o1.body.price, 8) === P && scaleInt(o1.body.size, 8) === size8 && o1.body.account === B.addr, JSON.stringify(o1.body));
        const ra = await place(A, false, 1);
        ok('taker sell: included, filled in full', ra.body?.result?.ok === true && ra.body?.result?.orderStatus === 'filled' && scaleInt(ra.body?.result?.filledSize ?? '0', 8) === size8, JSON.stringify(ra.body));
        const fa = (await get(`/v1/accounts/${A.addr}/fills?limit=1`)).body.fills?.[0], fb = (await get(`/v1/accounts/${B.addr}/fills?limit=1`)).body.fills?.[0];
        ok('one fill for each side, same trade: taker sell, maker buy, at the maker price and size', !!fa && !!fb && fa.tradeId === fb.tradeId && fa.liquidity === 'taker' && fb.liquidity === 'maker' && fa.side === 'sell' && fb.side === 'buy' && [fa, fb].every((f) => scaleInt(f.price, 8) === P && scaleInt(f.size, 8) === size8 && f.liquidation === false), JSON.stringify([fa, fb]));
        ok('the fees charged are the published rates: taker ceil(notional x takerFeeBps), maker ceil(notional x makerFeeBps) (SPEC S-9.3)', !!fa && !!fb && scaleInt(fa.fee, 6) === feeOf(mk.takerFeeBps) && scaleInt(fb.fee, 6) === feeOf(mk.makerFeeBps), `${fa?.fee} / ${fb?.fee} vs ${feeOf(mk.takerFeeBps)} / ${feeOf(mk.makerFeeBps)} units`);
        const tr = (await get(`/v1/markets/${sym}/trades?limit=1`)).body.trades?.[0];
        ok('the public tape shows the trade (taker side sell)', !!tr && tr.tradeId === fa?.tradeId && tr.side === 'sell' && scaleInt(tr.price, 8) === P && scaleInt(tr.size, 8) === size8 && tr.liquidation === false, JSON.stringify(tr));
        const acc1 = { A: (await get(`/v1/accounts/${A.addr}`)).body, B: (await get(`/v1/accounts/${B.addr}`)).body };
        valid('Account with a position', 'Account', acc1.A);
        const pa = pos(acc1.A), pb = pos(acc1.B);
        ok('positions equal and opposite at the fill price (I-1)', !!pa && !!pb && scaleInt(pa.size.replace('-', ''), 8) === size8 && pa.size.startsWith('-') && scaleInt(pb.size, 8) === size8 && scaleInt(pa.entryPrice, 8) === P && scaleInt(pb.entryPrice, 8) === P, JSON.stringify([pa, pb]));
        ok('position notional = |size| x markPrice (to the USDC unit)', !!pa && Math.abs(num(pa.notional) - Math.abs(num(pa.size)) * num(pa.markPrice)) <= 0.000001 + 1e-9 * num(pa.notional), JSON.stringify(pa));
        ok('requirements are positive and ordered: 0 < maintenanceMargin <= initialMargin; liquidationPrice is on the losing side', !!pa && num(pa.maintenanceMargin) > 0 && num(pa.maintenanceMargin) <= num(pa.initialMargin) && (pa.liquidationPrice === null || num(pa.liquidationPrice) > num(pa.entryPrice)) && (pb.liquidationPrice === null || num(pb.liquidationPrice) < num(pb.entryPrice)), JSON.stringify([pa, pb]));
        const o2 = (await get(`/v1/orders/${oid}`)).body;
        ok('the maker order is filled: filledSize = size, avgFillPrice = its price', o2?.status === 'filled' && scaleInt(o2.filledSize, 8) === size8 && scaleInt(o2.avgFillPrice, 8) === P, JSON.stringify(o2));
        const tk1 = (await get(`/v1/markets/${sym}/ticker`)).body;
        ok('ticker lastPrice = the trade price; openInterest grew by the size', scaleInt(tk1.lastPrice, 8) === P && scaleInt(tk1.openInterest, 8) - scaleInt(tk0.openInterest, 8) === size8, JSON.stringify([tk0, tk1]));
        const cs = (await get(`/v1/markets/${sym}/candles?interval=1m&limit=5`)).body.candles ?? [];
        const c = cs.find((x) => x.t === Math.floor(tr.time / 60000) * 60000);
        ok('the 1m candle of the trade holds it: l <= price <= h, n >= 1, v >= size', !!c && scaleInt(c.l, 8) <= P && scaleInt(c.h, 8) >= P && c.n >= 1 && scaleInt(c.v, 8) >= size8, JSON.stringify(cs.slice(-2)));
        // 2. unwind at the same price with the roles swapped on the book side: A buys back from B's post-only ask
        const rb2 = await place(B, false, 2);
        const ra2 = await place(A, true, 1);
        ok('unwind: B rests the ask, A buys it back in full', rb2.body?.result?.ok === true && ra2.body?.result?.orderStatus === 'filled', JSON.stringify([rb2.body, ra2.body]));
        const acc2 = { A: (await get(`/v1/accounts/${A.addr}`)).body, B: (await get(`/v1/accounts/${B.addr}`)).body };
        ok('both accounts are flat again', !pos(acc2.A) && !pos(acc2.B), JSON.stringify([acc2.A.positions, acc2.B.positions]));
        // sold and bought at one price: the only balance changes are the fees (and any funding settled meanwhile)
        const fund = async (addr) => ((await get(`/v1/accounts/${addr}/funding?limit=20`)).body.payments ?? []).filter((x) => x.time >= t0).reduce((t, x) => t + scaleInt(x.payment, 6) * (x.payment.startsWith('-') ? -1n : 1n), 0n);
        const dA = usd(acc2.A.collateral) - usd(acc0.A.collateral), dB = usd(acc2.B.collateral) - usd(acc0.B.collateral);
        ok('round trip at one price costs exactly the fees: taker 2 x taker fee, maker 2 x maker fee', dA === -2n * feeOf(mk.takerFeeBps) + (await fund(A.addr)) && dB === -2n * feeOf(mk.makerFeeBps) + (await fund(B.addr)), `${dA} / ${dB} units`);
      }
      // 3. a withdrawal of the funded account, proven against a final checkpoint (the vault's `withdraw` input)
      const fee6 = 1_000_000n;
      const rw = await post('/v1/actions?wait=included', signed('Withdraw', { account: A.addr, nonce: nextNonce(), amount: String(fee6 + 1_000_000n), recipient: B.addr }, A.key), hdr);
      ok('Withdraw included: result carries the withdrawalId', rw.body?.result?.ok === true && /^\d+$/.test(rw.body?.result?.withdrawalId ?? ''), JSON.stringify(rw.body));
      const wid = rw.body?.result?.withdrawalId;
      let w = null;
      for (let i = 0; i < 40 && wid !== undefined; i++) {
        w = (await get(`/v1/withdrawals/${wid}`)).body;
        if (w?.status === 'certified') break;
        await post('/v1/actions', signed('CancelAll', { account: A.addr, nonce: nextNonce(), marketId: 0 }, A.key), hdr);   // blocks, so checkpoints
        await new Promise((r) => setTimeout(r, 1000));
      }
      ok('the withdrawal becomes certified (a final checkpoint commits it)', w?.status === 'certified' && w.certificate !== null, JSON.stringify(w).slice(0, 200));
      if (w?.status === 'certified') {
        valid('certified Withdrawal', 'Withdrawal', w);
        ok('record: net = gross - fee, to the named recipient', scaleInt(w.amount, 6) + scaleInt(w.fee, 6) === fee6 + 1_000_000n && w.recipient === B.addr && w.account === A.addr, JSON.stringify(w).slice(0, 200));
        const cph = w.certificate.checkpoint.height;
        const wp = await get(`/v1/withdrawals/${wid}/proof?height=${cph}`);
        valid('GET /v1/withdrawals/{id}/proof', 'WithdrawalProof', wp.body);
        let root = null, leaf = null, why = '';
        try {
          const r = wp.body.record;
          leaf = hex(leafHash(encodeWithdrawalLeaf({ withdrawalId: r.withdrawalId, account: r.account, recipient: r.recipient, amount: r.amount, height: r.height })));
          root = root32(leaf, wid, wp.body.siblings);
        } catch (e) {
          why = e.message;
        }
        ok('WithdrawalProof: leaf = keccak256(0x00 || "PPv1wdrl" || id || account || recipient || amount || height) of the record', leaf !== null && leaf === wp.body.leaf && String(wp.body.record.withdrawalId) === wid && BigInt(wp.body.record.amount) === scaleInt(w.amount, 6), why || JSON.stringify(wp.body.record));
        ok('WithdrawalProof: the 32 siblings walk to the withdrawalsRoot of the final checkpoint it names', root !== null && root === wp.body.withdrawalsRoot && root === w.certificate.checkpoint.withdrawalsRoot, why || `${root} vs ${w.certificate.checkpoint.withdrawalsRoot}`);
        ok('the certificate\'s checkpoint counts the record (withdrawalCount > id) and is signed by the threshold', BigInt(w.certificate.checkpoint.withdrawalCount) > BigInt(wid) && w.certificate.signatures.length >= committee.body.threshold);
      }
    }
  }
  await checkInputs('after actions', true);

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
  await wsChecks(base, V, ok, valid, { sym, demo, mainAddr, signedCancel: () => signed('CancelAll', { account: mainAddr, nonce: nextNonce(), marketId: 0 }, main), signedRest: () => signed('PlaceOrder', { ...po, nonce: nextNonce() }, main), mock, canFund, needsFunds });

}

async function wsChecks(base, V, ok, valid, { sym, demo, mainAddr, signedCancel, signedRest, mock, canFund, needsFunds }) {
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
  else if (canFund) {
    // A real node's book moves only with actions: rest an order far from the mark (a delta), then cancel it.
    ws.send(JSON.stringify({ op: 'action', id: 10, request: signedRest() }));
    await wait((m) => m.channel === 'book' && m.type === 'delta', 6000);
    ws.send(JSON.stringify({ op: 'action', id: 11, request: signedCancel() }));
  }
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
  if (needsFunds('ws book deltas chain by prevSeq from the snapshot')) ok(`ws book deltas chain by prevSeq from the snapshot (${deltas.length} deltas)`, chain);
  ws.close();
}

// ---------- CLI ----------
if (process.argv[1] && resolve(process.argv[1]) === fileURLToPath(import.meta.url)) {
  const args = process.argv.slice(2);
  const opt = (n, d) => (args.includes(`--${n}`) ? args[args.indexOf(`--${n}`) + 1] : d);
  if (args.includes('--print-accounts')) {
    const lab = (flag, d) => (args.includes(flag) && args[args.indexOf(flag) + 1] && !args[args.indexOf(flag) + 1].startsWith('--') ? args[args.indexOf(flag) + 1] : d);
    process.stdout.write(`${JSON.stringify({ funded: fundedAccount(lab('--funded', 'funded')), funded2: fundedAccount2(lab('--funded2', 'funded2')) })}\n`);
    process.exit(0);
  }
  const fv = args.includes('--funded') ? args[args.indexOf('--funded') + 1] : null;
  const funded = args.includes('--funded') ? (!fv || fv.startsWith('--') ? 'funded' : fv) : null;
  const f2 = args.includes('--funded2') ? args[args.indexOf('--funded2') + 1] : null;
  const funded2 = args.includes('--funded2') ? (!f2 || f2.startsWith('--') ? 'funded2' : f2) : null;
  const r = await runConformance(opt('base', 'http://127.0.0.1:49300'), { mock: args.includes('--mock'), schema: opt('schema', undefined), funded, funded2 });
  if (args.includes('--json')) process.stdout.write(JSON.stringify(r, null, 1) + '\n');
  else {
    for (const f of r.failed) process.stdout.write(`FAIL ${f.name}: ${f.detail}\n`);
    for (const f of r.skipped) process.stdout.write(`SKIP ${f.name}\n`);
    process.stdout.write(`conformance: ${r.passed}/${r.total} checks passed, ${r.failed.length} failed, ${r.skipped.length} skipped (schema ${schemaPath(opt('schema', undefined))})\n`);
  }
  process.exit(r.failed.length ? 1 : 0);
}

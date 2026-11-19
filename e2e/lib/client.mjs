// API client of the rehearsal: REST reads, EIP-712 signing of actions with throwaway keys, waiting helpers.
// UNAUDITED prototype. Copyright (c) 2026 Bloch Inc. All rights reserved.
import { hex, unhex, keccak256, concat } from '../../node/mock/lib/keccak.mjs';
import { sign } from '../../node/mock/lib/secp256k1.mjs';
import { typedDataHash } from '../../node/mock/lib/eip712.mjs';

export const sleep = (ms) => new Promise((r) => setTimeout(r, ms));

/** Exact decimal string -> scaled BigInt (refuses more fractional digits than the scale: SPEC S-1.2). */
export function scale(s, dp) {
  const neg = s.startsWith('-');
  const [i, f = ''] = (neg ? s.slice(1) : s).split('.');
  if (f.length > dp || !/^\d+$/.test(i) || !/^\d*$/.test(f)) throw new Error(`not exact at scale ${dp}: ${s}`);
  const v = BigInt(i + f.padEnd(dp, '0'));
  return neg ? -v : v;
}
export const usdc = (s) => scale(s, 6);
export const px = (s) => scale(s, 8);
export const sz = (s) => scale(s, 8);

export class Api {
  constructor(base, { country = null } = {}) {
    this.base = base;
    this.country = country;
    this.nonces = new Map();
    this.eip = null;
  }
  /** fetch that survives a short loss of the ssh tunnel (it is supervised and comes back): a NETWORK error is retried
   *  for up to 60 s and counted in `networkRetries`; an HTTP answer of any status is never retried. */
  async net(url, init) {
    for (let i = 0; ; i++) {
      try { return await fetch(url, init); } catch (e) {
        if (i >= 30) throw new Error(`no connection to the stack for 60 s (ssh tunnel down?): ${e.message}`);
        this.networkRetries = (this.networkRetries ?? 0) + 1;
        await sleep(2000);
      }
    }
  }
  async get(path, headers = {}) {
    const r = await this.net(this.base + path, { headers });
    const text = await r.text();
    let body;
    try { body = JSON.parse(text); } catch { body = text; }
    return { status: r.status, body };
  }
  async json(path) {
    const r = await this.get(path);
    if (r.status !== 200) throw new Error(`GET ${path}: HTTP ${r.status} ${JSON.stringify(r.body).slice(0, 200)}`);
    return r.body;
  }
  async post(path, body, headers = {}) {
    // a POST is retried only when the connection could not be made at all (nothing reached the node)
    const r = await this.net(this.base + path, { method: 'POST', headers: { 'content-type': 'application/json', ...headers }, body: JSON.stringify(body) });
    return { status: r.status, body: await r.json().catch(() => null) };
  }
  async init() {
    this.eip = await this.json('/v1/eip712');
    this.status0 = await this.json('/v1/status');
    this.markets = (await this.json('/v1/markets')).markets;
    return this;
  }
  market(symbol) { return this.markets.find((m) => m.symbol === symbol); }
  /** max(engine clock ms, last + 1) per signer (SPEC 2.5; 391's rule). */
  async nonce(signer) {
    const t = BigInt((await this.json('/v1/status')).time);
    const last = this.nonces.get(signer) ?? 0n;
    const n = t > last ? t : last + 1n;
    this.nonces.set(signer, n);
    return String(n);
  }
  digest(type, msg) { return typedDataHash(this.eip.domain, type, this.eip.types[type], msg); }
  signedRequest(type, msg, key) { return { type, message: msg, signature: hex(sign(this.digest(type, msg), key)), wire: true }; }
  actionHash(type, msg, signerAddress) { return hex(keccak256(concat(this.digest(type, msg), unhex(signerAddress)))); }
  /** Signs and posts an action, waits for inclusion. Returns {status, body}. `who` = {key, address}; `as` = account. */
  async act(who, type, fields, { account = who.address, country = this.country, wait = true } = {}) {
    const msg = { account, nonce: await this.nonce(who.address), ...fields };
    const headers = country ? { 'x-mock-country': country } : {};
    const r = await this.post(`/v1/actions${wait ? '?wait=included' : ''}`, this.signedRequest(type, msg, who.key), headers);
    if (wait && r.status === 200 && r.body?.status !== 'included') {
      // the 2 s hold elapsed: poll the action until it is in a block
      const h = r.body.actionHash;
      for (let i = 0; i < 40; i++) {
        await sleep(250);
        const g = await this.get(`/v1/actions/${h}`);
        if (g.status === 200 && g.body.status !== 'accepted') return { status: 200, body: g.body, msg };
      }
    }
    return { ...r, msg };
  }
  order(who, m, { buy, price, size, type = 0, tif = 0, reduceOnly = false, trigger = '0' }, opts) {
    return this.act(who, 'PlaceOrder', { marketId: m.marketId, isBuy: buy, orderType: type, timeInForce: tif, price: String(px(price)), size: String(sz(size)), reduceOnly, triggerPrice: String(px(trigger)), clientOrderId: '0x' + '00'.repeat(32) }, opts);
  }
  cancelAll(who, marketId = 0, opts) { return this.act(who, 'CancelAll', { marketId }, opts); }
  withdraw(who, amount, recipient = who.address, opts) { return this.act(who, 'Withdraw', { amount: String(usdc(amount)), recipient }, opts); }
  account(a) { return this.json(`/v1/accounts/${a}`); }
  async until(fn, { ms = 30000, every = 500, what = 'condition' } = {}) {
    const end = Date.now() + ms;
    let last;
    for (;;) {
      try { last = await fn(); if (last) return last; } catch (e) { last = e; }
      if (Date.now() > end) throw new Error(`timeout waiting for ${what}${last instanceof Error ? `: ${last.message}` : ''}`);
      await sleep(every);
    }
  }
}

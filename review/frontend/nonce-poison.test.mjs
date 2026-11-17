// R395-11 demonstration: a hostile or buggy API's nextNonce, accepted up to clock + 1 day by
// frontend/src/lib/nonce.ts, is persisted and then every later nonce sits outside the engine's
// proposed nonce window (T - 120000 <= nonce <= T + 30000; 389, cross-area 03:50), so the user cannot
// sign a valid cancel from this browser for ~a day. Usage: node --experimental-strip-types nonce-poison.test.mjs <path-to-nonce.ts>
import assert from 'node:assert/strict';
import { pathToFileURL } from 'node:url';
globalThis.localStorage = { _m: {}, getItem(k) { return this._m[k] ?? null; }, setItem(k, v) { this._m[k] = v; }, removeItem(k) { delete this._m[k]; } };
const mod = await import(pathToFileURL(process.argv[2]).href);
const T = 1794878412000;
const inWindow = (n, t) => BigInt(t) - 120000n <= n && n <= BigInt(t) + 30000n;
const poisoned = BigInt(T) + 23n * 3600000n;           // API says nextNonce = clock + 23 h
const first = mod.nextNonce('0xabc', T, poisoned);
assert.equal(first, poisoned, 'frontend followed the published nonce');
// One hour later the user tries to cancel; the API is honest again (published = null).
const later = T + 3600000;
const n2 = mod.nextNonce('0xabc', later, null);
console.log('nonce signed one hour later:', n2.toString(), 'in engine window:', inWindow(n2, later));
assert.equal(inWindow(n2, later), false);
console.log('R395-11 DEMONSTRATED: persisted far-ahead nonce locks this signer out of the engine window');

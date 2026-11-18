// L2 side of the rehearsal: JSON-RPC to the local anvil and contract calls through Foundry's `cast` (an ABI
// implementation independent of this repository). Transactions are sent from anvil-impersonated throwaway
// addresses: no private key is used for the L2 at all. Local anvil only: the constructor refuses any other URL.
// UNAUDITED prototype. Copyright (c) 2026 Bloch Inc. All rights reserved.
import { execFile } from 'node:child_process';
import { promisify } from 'node:util';
const exec = promisify(execFile);
// the Mac's Foundry build links libusb from the tools directory (BRIEF-ROUND10)
const ENV = { ...process.env, DYLD_LIBRARY_PATH: `${process.env.HOME}/.local/share/dex-tools/libusb/lib` };
const run = (file, args) => exec(file, args, { env: ENV, maxBuffer: 16 << 20 });

export class Chain {
  constructor(rpc, foundryDir) {
    if (!/^http:\/\/127\.0\.0\.1:\d+$/.test(rpc)) throw new Error('the rehearsal talks to a loopback anvil only');
    this.url = rpc;
    this.cast = `${foundryDir}/cast`;
    this.funded = new Set();
  }
  async rpc(method, params = []) {
    const r = await fetch(this.url, { method: 'POST', headers: { 'content-type': 'application/json' }, body: JSON.stringify({ jsonrpc: '2.0', id: 1, method, params }) });
    const j = await r.json();
    if (j.error) throw new Error(`${method}: ${JSON.stringify(j.error).slice(0, 300)}`);
    return j.result;
  }
  async init() {
    const info = await this.rpc('anvil_nodeInfo');   // only anvil answers this
    this.chainId = Number(await this.rpc('eth_chainId'));
    await this.rpc('anvil_autoImpersonateAccount', [true]);
    return info;
  }
  async block(tag = 'latest') {
    const b = await this.rpc('eth_getBlockByNumber', [tag, false]);
    return b ? { number: Number(b.number), hash: b.hash, time: Number(b.timestamp) } : null;
  }
  /** Read call; returns cast's decoded output lines (one per return value). */
  async call(to, sig, args = []) {
    const { stdout } = await run(this.cast, ['call', '--rpc-url', this.url, to, sig, ...args.map(String)]);
    return stdout.trim().split('\n').map((l) => l.replace(/ \[[^\]]*\]$/, ''));   // strip cast's "[1e6]" hints
  }
  async callOne(to, sig, args = []) { return (await this.call(to, sig, args))[0]; }
  /** Sends a transaction from an impersonated address. Returns {ok, receipt} or {ok:false, error} (revert text). */
  async send(from, to, sig, args = []) {
    if (!this.funded.has(from)) { await this.rpc('anvil_setBalance', [from, '0x56bc75e2d63100000']); this.funded.add(from); }
    try {
      const { stdout } = await run(this.cast, ['send', '--rpc-url', this.url, '--unlocked', '--from', from, '--gas-limit', '5000000', '--json', to, sig, ...args.map(String)]);
      const receipt = JSON.parse(stdout);
      return { ok: receipt.status === '0x1', receipt, block: Number(receipt.blockNumber), tx: receipt.transactionHash, gas: Number(receipt.gasUsed) };
    } catch (e) {
      return { ok: false, error: String(e.stderr || e.message).trim().split('\n').slice(-3).join(' ').slice(0, 400) };
    }
  }
  /** Moves the L2 clock forward (the vault's delays are production-shape: days). */
  async warp(seconds) { await this.rpc('evm_increaseTime', [seconds]); await this.rpc('evm_mine'); }
  async mine(n = 1) { await this.rpc('anvil_mine', ['0x' + n.toString(16)]); }
  async waitFinalized(blockNumber, ms = 60000) {
    const end = Date.now() + ms;
    for (;;) {
      const f = await this.block('finalized');
      if (f && f.number >= blockNumber) return f;
      if (Date.now() > end) throw new Error(`L2 block ${blockNumber} not finalized in ${ms} ms`);
      await new Promise((r) => setTimeout(r, 500));
    }
  }
}

/**
 * Read-only JSON-RPC client of the collateral chain (the Bloch L2). UNAUDITED prototype.
 *
 * The page reads the vault and the token HERE, from the L2 endpoint of the build configuration, never through the
 * exchange API: what the vault holds, owes and accepts is the L2's word, and the exits (forced request, escape) must
 * work while the exchange API is silent or lying. Nothing is ever sent through this client: transactions go only
 * through the user's wallet (vault/tx.ts).
 *
 * Rules: the endpoint's chain id is checked before the first read and must equal the configured one (a wrong RPC is
 * refused, not read); every answer is checked for shape; a group of reads is pinned to ONE block number so a view
 * never mixes two states.
 */
import type { Hex } from 'viem';

export class L2Error extends Error {
  constructor(
    message: string,
    /** Revert data of an eth_call, when the node returned it. */
    readonly revertData: Hex | null = null,
  ) {
    super(message);
  }
}

type Json = null | boolean | number | string | Json[] | { [key: string]: Json };
export type Fetch = (url: string, init: RequestInit) => Promise<Response>;
export type Log = { address: Hex; topics: Hex[]; data: Hex; blockNumber: bigint; transactionHash: Hex; logIndex: number };
export type Receipt = { status: 'success' | 'reverted'; blockNumber: bigint; logs: Log[]; transactionHash: Hex };

const HEX = /^0x[0-9a-fA-F]*$/;
const QUANTITY = /^0x(0|[1-9a-fA-F][0-9a-fA-F]*)$/;
const isHex = (v: unknown): v is Hex => typeof v === 'string' && HEX.test(v) && v.length % 2 === 0;
const quantity = (v: unknown, what: string): bigint => {
  if (typeof v !== 'string' || !QUANTITY.test(v) || v.length > 66) throw new L2Error(`The L2 endpoint returned an unusable ${what}.`);
  return BigInt(v);
};

function revertDataOf(error: unknown): Hex | null {
  // Nodes put revert bytes in error.data, or error.data.data, or (older) inside an object keyed by tx hash.
  const seen = new Set<unknown>();
  const walk = (v: unknown, depth: number): Hex | null => {
    if (depth > 3 || v === null || seen.has(v)) return null;
    if (isHex(v) && v.length >= 10) return v;
    if (typeof v !== 'object') return null;
    seen.add(v);
    for (const inner of Object.values(v as Record<string, unknown>)) {
      const found = walk(inner, depth + 1);
      if (found) return found;
    }
    return null;
  };
  return walk((error as { data?: unknown } | null)?.data, 0);
}

export class L2 {
  private checked: Promise<void> | null = null;
  private id = 0;
  constructor(
    readonly url: string,
    readonly chainId: number,
    private readonly fetchImpl: Fetch = (u, i) => fetch(u, i),
  ) {}

  private async post(body: Json): Promise<unknown> {
    let response: Response;
    try {
      response = await this.fetchImpl(this.url, {
        method: 'POST',
        headers: { 'content-type': 'application/json' },
        body: JSON.stringify(body),
        credentials: 'omit',
        referrerPolicy: 'no-referrer',
        signal: AbortSignal.timeout(15_000),
      });
    } catch {
      throw new L2Error('The Bloch L2 endpoint did not answer.');
    }
    if (!response.ok) throw new L2Error(`The Bloch L2 endpoint answered ${response.status}.`);
    try {
      return await response.json();
    } catch {
      throw new L2Error('The Bloch L2 endpoint did not answer in JSON.');
    }
  }

  private unwrap(answer: unknown): unknown {
    if (!answer || typeof answer !== 'object') throw new L2Error('The Bloch L2 endpoint returned an unusable answer.');
    const { result, error } = answer as { result?: unknown; error?: { message?: unknown } };
    if (error) {
      const data = revertDataOf(error);
      throw new L2Error(data ? 'The call would revert.' : 'The Bloch L2 endpoint refused the request.', data);
    }
    if (result === undefined) throw new L2Error('The Bloch L2 endpoint returned no result.');
    return result;
  }

  /** One request, WITHOUT the chain check (used by the check itself). */
  private async raw(method: string, params: Json[]): Promise<unknown> {
    return this.unwrap(await this.post({ jsonrpc: '2.0', id: ++this.id, method, params }));
  }

  /** Refuses an endpoint that is not the configured chain. Checked once per client; a failure is retried next time. */
  ready(): Promise<void> {
    this.checked ??= (async () => {
      const got = quantity(await this.raw('eth_chainId', []), 'chain id');
      if (got !== BigInt(this.chainId)) throw new L2Error(`The L2 endpoint is chain ${got}, not the Bloch L2 (chain ${this.chainId}). Nothing is read from it.`);
    })().catch((e) => {
      this.checked = null;
      throw e;
    });
    return this.checked;
  }

  async request(method: string, params: Json[]): Promise<unknown> {
    await this.ready();
    return this.raw(method, params);
  }

  /** Several requests in one JSON-RPC batch; answers matched by id, each unwrapped (any error throws). */
  async batch(calls: { method: string; params: Json[] }[]): Promise<unknown[]> {
    await this.ready();
    if (calls.length === 0) return [];
    const first = this.id + 1;
    const body = calls.map((c) => ({ jsonrpc: '2.0', id: ++this.id, method: c.method, params: c.params }));
    const answer = await this.post(body);
    if (!Array.isArray(answer) || answer.length !== calls.length) throw new L2Error('The Bloch L2 endpoint returned an unusable batch answer.');
    const byId = new Map<number, unknown>();
    for (const a of answer) byId.set(Number((a as { id?: unknown })?.id), a);
    return calls.map((_, i) => this.unwrap(byId.get(first + i)));
  }

  /** The newest block: number and timestamp (L2 seconds: the vault's clock). */
  async head(tag: 'latest' | 'finalized' = 'latest'): Promise<{ number: bigint; timestamp: bigint }> {
    const block = (await this.request('eth_getBlockByNumber', [tag, false])) as { number?: unknown; timestamp?: unknown } | null;
    if (!block) throw new L2Error(`The Bloch L2 endpoint has no ${tag} block.`);
    return { number: quantity(block.number, 'block number'), timestamp: quantity(block.timestamp, 'block time') };
  }

  /** eth_call results for `calls`, all at block `at`. */
  async calls(calls: { to: Hex; data: Hex; from?: Hex }[], at: bigint | 'latest'): Promise<Hex[]> {
    const block = at === 'latest' ? 'latest' : `0x${at.toString(16)}`;
    const out = await this.batch(calls.map((c) => ({ method: 'eth_call', params: [{ to: c.to, data: c.data, ...(c.from ? { from: c.from } : {}) }, block] })));
    return out.map((r) => {
      if (!isHex(r)) throw new L2Error('The Bloch L2 endpoint returned an unusable call result.');
      return r;
    });
  }

  async call(call: { to: Hex; data: Hex; from?: Hex }, at: bigint | 'latest' = 'latest'): Promise<Hex> {
    return (await this.calls([call], at))[0];
  }

  /** Gas the call needs from `from` at the newest state (the node's estimate; see vault.ts `gasLimit`). */
  async estimateGas(call: { to: Hex; data: Hex; from: Hex }): Promise<bigint> {
    return quantity(await this.request('eth_estimateGas', [{ from: call.from, to: call.to, data: call.data }]), 'gas estimate');
  }

  private log(l: unknown): Log {
    const v = l as Record<string, unknown>;
    if (!v || !isHex(v.address) || !Array.isArray(v.topics) || !v.topics.every(isHex) || !isHex(v.data) || !isHex(v.transactionHash)) throw new L2Error('The Bloch L2 endpoint returned an unusable log.');
    return { address: v.address.toLowerCase() as Hex, topics: v.topics as Hex[], data: v.data, blockNumber: quantity(v.blockNumber, 'log block'), transactionHash: v.transactionHash, logIndex: Number(quantity(v.logIndex, 'log index')) };
  }

  async logs(filter: { address: Hex; topics: (Hex | null)[]; fromBlock: bigint; toBlock: bigint }): Promise<Log[]> {
    const out = await this.request('eth_getLogs', [{ address: filter.address, topics: filter.topics, fromBlock: `0x${filter.fromBlock.toString(16)}`, toBlock: `0x${filter.toBlock.toString(16)}` }]);
    if (!Array.isArray(out)) throw new L2Error('The Bloch L2 endpoint returned unusable logs.');
    return out.map((l) => this.log(l));
  }

  /** The receipt of `hash`, or null while the transaction is not in a block. */
  async receipt(hash: Hex): Promise<Receipt | null> {
    const r = (await this.request('eth_getTransactionReceipt', [hash])) as Record<string, unknown> | null;
    if (r === null) return null;
    if (typeof r !== 'object' || !Array.isArray(r.logs)) throw new L2Error('The Bloch L2 endpoint returned an unusable receipt.');
    const status = quantity(r.status, 'receipt status');
    if (status !== 0n && status !== 1n) throw new L2Error('The Bloch L2 endpoint returned an unusable receipt status.');
    if (typeof r.transactionHash !== 'string' || r.transactionHash.toLowerCase() !== hash.toLowerCase()) throw new L2Error('The Bloch L2 endpoint returned the receipt of another transaction.');
    return { status: status === 1n ? 'success' : 'reverted', blockNumber: quantity(r.blockNumber, 'receipt block'), logs: r.logs.map((l) => this.log(l)), transactionHash: hash };
  }
}

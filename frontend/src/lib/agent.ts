/**
 * Agent (API) keys: a second key, generated in this browser, that the main wallet approves to trade for the account
 * (docs/API.md 5.2). UNAUDITED prototype.
 *
 * Decided (technical):
 *  - The key is generated here with the platform's CSPRNG (viem `generatePrivateKey`, crypto.getRandomValues) and
 *    never leaves the browser: the exchange only ever sees its address and its signatures.
 *  - Default storage is this tab's memory only: closing the tab forgets it (the approval stays until expiry or
 *    revocation, so the user revokes it on the Agents page). "Keep on this device" stores it in localStorage under
 *    the main account; the page says what that means (anyone with access to this browser profile can trade the
 *    account until expiry; never withdraw).
 *  - An agent signer refuses, by construction, every action outside AGENT_ALLOWED (lib/sign.ts) and an expired key.
 */
import { generatePrivateKey, privateKeyToAccount } from 'viem/accounts';
import { forViem, type Signer } from './sign';
import type { TypedData } from './eip712';

export type AgentRecord = {
  /** Main account, lower case. */
  account: string;
  /** Agent address, lower case. */
  agent: string;
  name: string;
  /** Unix milliseconds (SPEC 2.3). */
  expiresAt: number;
  persisted: boolean;
  key: `0x${string}`;
};

// v2: expiry in milliseconds (SPEC 0.1); v1 records (seconds) are never read.
const KEY = 'postern.perps.agents.v2';
const memory = new Map<string, AgentRecord>();

function readStored(): AgentRecord[] {
  try {
    const parsed: unknown = JSON.parse(localStorage.getItem(KEY) ?? '[]');
    if (!Array.isArray(parsed)) return [];
    return parsed.filter(
      (r): r is AgentRecord =>
        !!r && typeof r === 'object' && /^0x[0-9a-f]{40}$/.test(r.account) && /^0x[0-9a-f]{40}$/.test(r.agent) && /^0x[0-9a-f]{64}$/i.test(r.key) && typeof r.expiresAt === 'number' && typeof r.name === 'string',
    );
  } catch {
    return [];
  }
}
function writeStored(records: AgentRecord[]) {
  try {
    localStorage.setItem(KEY, JSON.stringify(records));
  } catch {
    /* storage unavailable: memory only */
  }
}

/** A new key for `account`, not yet approved (approval is the ApproveAgent action signed by the main wallet). */
export function createAgent(account: string, name: string, expiresAt: number): AgentRecord {
  const key = generatePrivateKey();
  const agent = privateKeyToAccount(key).address.toLowerCase();
  return { account: account.toLowerCase(), agent, name, expiresAt, persisted: false, key };
}

/** Keeps an approved key: in memory always, in localStorage when `persist`. */
export function keepAgent(record: AgentRecord, persist: boolean) {
  const r = { ...record, persisted: persist };
  memory.set(r.account, r);
  const others = readStored().filter((x) => x.account !== r.account);
  writeStored(persist ? [...others, r] : others);
}

/** The usable key of `account` on this device, if any and not expired. */
export function agentFor(account: string, nowMs: number = Date.now()): AgentRecord | null {
  const a = account.toLowerCase();
  const r = memory.get(a) ?? readStored().find((x) => x.account === a) ?? null;
  if (!r) return null;
  if (r.expiresAt <= nowMs) {
    forgetAgent(a);
    return null;
  }
  memory.set(a, r);
  return r;
}

export function forgetAgent(account: string) {
  const a = account.toLowerCase();
  memory.delete(a);
  writeStored(readStored().filter((x) => x.account !== a));
}

export function agentSigner(record: AgentRecord): Signer {
  const local = privateKeyToAccount(record.key);
  return {
    kind: 'agent',
    address: record.agent,
    expiresAt: record.expiresAt,
    sign: (data: TypedData) => local.signTypedData(forViem(data) as never),
  };
}

/** Test hook. */
export function resetAgents() {
  memory.clear();
  try {
    localStorage.removeItem(KEY);
  } catch {
    /* none */
  }
}

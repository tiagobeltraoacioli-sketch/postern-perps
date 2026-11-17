/**
 * Action nonces. docs/API.md 5.1: per signer, strictly increasing, gaps allowed, "a client may use the millisecond
 * clock". UNAUDITED prototype.
 *
 * Rule: next = max(clock ms, last used by this signer in this browser + 1). SPEC 2.5 accepts a nonce x at block time
 * T only when T - 120 s <= x <= T + 30 s, so a nonce above clock + 30 s is never built, followed or stored
 * (R395-11: following a published nonce up to a day ahead and persisting it poisoned this browser's signer for a day,
 * cancels included). The exchange's `nextNonce` is followed only inside that bound; above it signing is refused with
 * a message (the exchange's clock or this device's clock is wrong).
 */
const KEY = 'postern.perps.nonce.v1';
/** SPEC 2.5 future bound of the nonce window (ms). */
export const NONCE_FUTURE_MS = 30_000n;
export const MAX_UINT64 = (1n << 64n) - 1n;

function load(): Record<string, string> {
  try {
    const raw = localStorage.getItem(KEY);
    const parsed: unknown = raw ? JSON.parse(raw) : {};
    return parsed && typeof parsed === 'object' ? (parsed as Record<string, string>) : {};
  } catch {
    return {};
  }
}
const memory: Record<string, string> = load();

export class NonceError extends Error {}

/** The next nonce for `signer`, recorded as used. `published` is the exchange's nextNonce, if read. */
export function nextNonce(signer: string, now: number = Date.now(), published?: bigint | null): bigint {
  const key = signer.toLowerCase();
  const clock = BigInt(Math.floor(now));
  const last = memory[key] ? BigInt(memory[key]) : 0n;
  let next = clock > last ? clock : last + 1n;
  if (published !== undefined && published !== null) {
    if (published > clock + NONCE_FUTURE_MS) throw new NonceError('The exchange expects a nonce ahead of this device clock by more than 30 seconds. Check the clock of this device; nothing was signed.');
    if (published > next) next = published;
  }
  if (next > clock + NONCE_FUTURE_MS) throw new NonceError('This device clock moved back since the last signature. Wait a moment or correct the clock; nothing was signed.');
  if (next > MAX_UINT64) throw new NonceError('Nonce space exhausted.');
  memory[key] = next.toString();
  try {
    localStorage.setItem(KEY, JSON.stringify(memory));
  } catch {
    /* in-memory only: the clock still orders the nonces */
  }
  return next;
}

/** Test hook. */
export function resetNonces() {
  for (const k of Object.keys(memory)) delete memory[k];
  try {
    localStorage.removeItem(KEY);
  } catch {
    /* none */
  }
}

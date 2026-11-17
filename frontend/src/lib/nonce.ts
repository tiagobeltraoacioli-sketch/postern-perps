/**
 * Action nonces. docs/API.md 5.1: per signer, strictly increasing, gaps allowed, "a client may use the millisecond
 * clock". UNAUDITED prototype.
 *
 * Rule: next = max(clock ms, last used by this signer in this browser + 1). The exchange's `nextNonce` is only
 * CHECKED: a value above the clock by more than a day is refused (an API that published a huge nonce would make the
 * signer burn its nonce space; uint64 ends), a value above our candidate but within a day is followed.
 */
const KEY = 'postern.perps.nonce.v1';
const DAY = 86_400_000n;
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
    if (published > clock + DAY) throw new NonceError('The exchange published a nonce far ahead of the clock. Signing is refused; tell the operator.');
    if (published > next) next = published;
  }
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

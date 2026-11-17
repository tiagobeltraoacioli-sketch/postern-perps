/** REST client of docs/API.md. No cookies, no credentials: authority comes only from signatures. */
import { config } from './config';
import type { ActionAck, WireError } from './types';

export class ApiError extends Error {
  constructor(
    readonly status: number,
    readonly code: string,
    message: string,
    readonly details: Record<string, unknown> = {},
  ) {
    super(message);
  }
}

/** Test and harness hook: extra headers for every request (the mock's X-Mock-Country). Never set in production code. */
let extraHeaders: Record<string, string> = {};
export const setExtraHeaders = (h: Record<string, string>) => (extraHeaders = h);

async function request<T>(path: string, init?: RequestInit): Promise<T> {
  let response: Response;
  try {
    response = await fetch(`${config.apiUrl}${path}`, {
      ...init,
      credentials: 'omit',
      referrerPolicy: 'no-referrer',
      headers: { ...(init?.body ? { 'content-type': 'application/json' } : {}), ...extraHeaders },
      signal: init?.signal ?? AbortSignal.timeout(10_000),
    });
  } catch {
    throw new ApiError(0, 'UNREACHABLE', 'The exchange API did not answer.');
  }
  let body: unknown = null;
  try {
    body = await response.json();
  } catch {
    /* handled below */
  }
  if (!response.ok) {
    const e = (body as WireError | null)?.error;
    throw new ApiError(response.status, typeof e?.code === 'string' ? e.code : 'HTTP_ERROR', typeof e?.message === 'string' ? e.message.slice(0, 300) : `The API answered ${response.status}.`, e?.details ?? {});
  }
  if (body === null) throw new ApiError(response.status, 'MALFORMED', 'The API answer was not JSON.');
  return body as T;
}

export const api = {
  get: <T>(path: string, signal?: AbortSignal) => request<T>(path, { signal }),
  action: (body: { type: string; message: Record<string, unknown>; signature: string }) =>
    request<ActionAck>('/v1/actions?wait=included', { method: 'POST', body: JSON.stringify({ ...body, wire: true }) }),
};

/** What a refusal means, in the interface's words. Server text is never shown as an instruction. */
export function errorWords(error: unknown): string {
  if (!(error instanceof ApiError)) return 'Something went wrong before the request was sent. Nothing was signed or submitted.';
  switch (error.code) {
    case 'GEO_RESTRICTED':
      return 'Refused: PosternDEX Perpetuals is not available in your jurisdiction. Cancelling, reducing and withdrawing stay open.';
    case 'SANCTIONED':
      return 'Refused: this address cannot use PosternDEX Perpetuals.';
    case 'BAD_SIGNATURE':
      return 'Refused: the signature did not match the request. Nothing was changed.';
    case 'UNKNOWN_SIGNER':
      return 'Refused: this key is not approved for the account. Approve the agent key again, or sign with your wallet.';
    case 'AGENT_NOT_ALLOWED':
      return 'Refused: this action needs your main wallet. An agent key cannot do it.';
    case 'BAD_NONCE':
      return 'Refused: the request was out of order (nonce). Try again.';
    case 'MARKET_HALTED':
      return 'Refused: this market is halted. Orders can be cancelled; new orders are not accepted until it reopens.';
    case 'RATE_LIMITED':
      return 'Refused: too many requests. Wait a moment and try again.';
    case 'SEQUENCER_UNAVAILABLE':
      return 'The exchange is not accepting actions right now (the sequencer is recovering). Nothing was changed.';
    case 'UNREACHABLE':
      return 'The exchange API did not answer. Nothing was submitted; check your connection and try again.';
    default:
      return `The exchange refused the request (${error.code}). Nothing was changed.`;
  }
}

/** The engine's own rejection names (inside result.error), in words. */
export function engineErrorWords(code: string | null | undefined): string {
  switch (code) {
    case 'INSUFFICIENT_MARGIN':
      return 'Not enough available margin for this order.';
    case 'POST_ONLY_WOULD_CROSS':
      return 'Post-only order rejected: it would have traded at once.';
    case 'REDUCE_ONLY_VIOLATION':
      return 'Reduce-only order rejected: it would have opened or grown a position.';
    case 'ORDER_NOT_FOUND':
      return 'That order is no longer open.';
    case 'MARKET_HALTED':
      return 'The market is halted.';
    case 'NO_LIQUIDITY':
      return 'Nothing filled: no liquidity inside the price limit.';
    case 'INSUFFICIENT_BALANCE':
      return 'Not enough withdrawable balance.';
    default:
      return code ? `Rejected by the exchange: ${code.slice(0, 60)}.` : 'Rejected by the exchange.';
  }
}

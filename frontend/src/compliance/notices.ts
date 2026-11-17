/**
 * Notices shown by the interface. UNAUDITED prototype. EVERY TEXT HERE IS A PLACEHOLDER FOR COUNSEL: the final
 * wording, the terms and the risk disclosure come from agent 392 (compliance/) and counsel. Version ids are
 * recorded with the user's local acknowledgement so a changed text asks again.
 */
export const OPERATOR_LINE = 'PosternDEX Perpetuals by Bloch Inc. This interface is operated by Bloch Inc (Panama).';
export const PROTOTYPE_LINE = 'Unaudited prototype. Not live: no real funds. Nothing here is an offer or advice.';

export const NOTICE_VERSION = 'draft-391-1';

export const RISK_POINTS = [
  'Perpetual futures are leveraged. A small price move can lose your whole margin, and a cross-margin position can use your whole account balance.',
  'A position is liquidated when its margin falls below the maintenance requirement. Liquidation closes it through the book at the price available, with a liquidation fee; in extreme markets the insurance fund and, last, auto-deleveraging of profitable positions are used.',
  'Funding is paid every hour between longs and shorts and can cost you money while you hold a position.',
  'One sequencer orders all actions. It cannot sign for you or move funds out, but it can delay or reorder; the forced-action path on the Bloch L2 bounds that delay.',
  'Collateral is bUSDC on the Bloch L2: it depends on the Bloch bridge and on the L2 as well as on this exchange.',
  'The software is an unaudited prototype and may contain errors that lose funds.',
];

export const ATTESTATION_POINTS = [
  'I am not a resident of, located in, or incorporated in a restricted jurisdiction, and I will not use a VPN or similar tool to hide my location.',
  'I am not a person or entity on a sanctions list, and I am not acting for one.',
  'I have read the risk notice above and I understand that I can lose all the funds I deposit.',
];

/** The restricted-region notice. The list and its sources are compliance/geoblock.json (agent 392). */
export function restrictedText(country: string | null): string {
  return country
    ? `PosternDEX Perpetuals is not available in your region (${country}). You can still cancel orders, reduce or close positions and withdraw.`
    : 'Your region could not be determined, so trading is not available. You can still cancel orders, reduce or close positions and withdraw.';
}

const ACK_KEY = 'postern.perps.attestation.v1';
export type Acknowledgement = { version: string; account: string; at: number };
const session = new Map<string, Acknowledgement>();
export function readAck(account: string): Acknowledgement | null {
  const inMemory = session.get(account.toLowerCase());
  if (inMemory?.version === NOTICE_VERSION) return inMemory;
  try {
    const list: unknown = JSON.parse(localStorage.getItem(ACK_KEY) ?? '[]');
    if (!Array.isArray(list)) return null;
    const a = list.find((x) => x && x.account === account.toLowerCase() && x.version === NOTICE_VERSION);
    return a ?? null;
  } catch {
    return null;
  }
}
export function writeAck(account: string, now: number = Date.now()): Acknowledgement {
  const ack = { version: NOTICE_VERSION, account: account.toLowerCase(), at: now };
  session.set(ack.account, ack);
  try {
    const list: unknown = JSON.parse(localStorage.getItem(ACK_KEY) ?? '[]');
    const rest = Array.isArray(list) ? list.filter((x) => x?.account !== ack.account) : [];
    localStorage.setItem(ACK_KEY, JSON.stringify([...rest, ack]));
  } catch {
    /* memory only: asked again next visit */
  }
  return ack;
}

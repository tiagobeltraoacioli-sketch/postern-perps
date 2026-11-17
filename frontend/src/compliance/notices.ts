/**
 * Notices shown by the interface, copied verbatim from compliance/legal/notices.json (agent 392, NOTICES-0.2,
 * commit 47f95bd; src/test/notices.test.ts pins the copy). DRAFTS FOR COUNSEL; UNAUDITED prototype.
 */
export const OPERATOR_LINE = 'PosternDEX Perpetuals by Bloch Inc. This interface is operated by Bloch Inc (Panama).';
export const PROTOTYPE_LINE = 'Unaudited prototype. Not live: no real funds. Nothing here is an offer or advice.';

/** compliance/legal/notices.json version (agent 392, 47f95bd); a change of text bumps it. */
export const NOTICE_VERSION = "NOTICES-0.2-20261117";
export const DOCUMENTS = {
  "terms": {
    "id": "TOU-0.2-20261117",
    "sha256": "e783388a58cb4c2564a24e13fd88b4a854b92d0058099ab6035393dbda87a1f5"
  },
  "risk": {
    "id": "RISK-0.2-20261117",
    "sha256": "42c25e647676c9b3c067c75e2deb2e8f6533c73955fcaca5b2cd1a345ee3f3af"
  },
  "privacy": {
    "id": "PRIV-0.1-20261117",
    "sha256": "25ea8d189dd653a8e4a7ca317fe2c41ae9580e0c1cb2a9cb955770ec6da06cf7"
  },
  "attestation": {
    "id": "ATTEST-0.1-20261117",
    "sha256": "d7ea07f70a6ab3d93dec0bc8aafb738bb57dd1baac94496cdb2986246c791175"
  }
} as const;

export const RISK_POINTS = [
  "Perpetual futures are leveraged. A small price move can lose your whole margin, and a cross-margin position can use your whole account balance.",
  "A position is liquidated when its margin falls below the maintenance requirement. Liquidation closes it through the book, never worse than the price at which your account reaches zero, with a 0.5% liquidation fee; in extreme markets the backstop, the insurance fund and, last, auto-deleveraging of profitable positions are used.",
  "A market halts when its price feed is stale or jumps. While halted you can only cancel: no closing, no liquidation, no stop-loss or take-profit. It may resume far from where it stopped.",
  "Funding is paid every hour between longs and shorts (capped at 0.05% of position value per hour) and can cost you money while you hold a position.",
  "One sequencer orders all actions. It cannot sign for you or move funds out, but it can delay or reorder; the forced-action path on the Bloch L2 bounds that delay.",
  "Collateral is bUSDC on the Bloch L2: it depends on the Bloch bridge, on the L2 and on USDC (which its issuer can freeze) as well as on this exchange.",
  "The software is an unaudited prototype and may contain errors that lose funds."
];

export const RESTRICTED_SUMMARY = "Not available in, among others, the United States and its territories, Canada, the United Kingdom, Brazil, mainland China, Australia, Singapore, Cuba, Iran, North Korea, Syria, Russia, Belarus, Myanmar and the Crimea, Sevastopol, Donetsk, Luhansk, Zaporizhzhia and Kherson regions of Ukraine; and, until counsel decides, in every other country.";

/** The restricted-region notice. The list and its sources are compliance/geoblock.json (agent 392). */
export function restrictedText(country: string | null): string {
  return country ? "PosternDEX Perpetuals is not available in your region (" + country + "). You can still cancel orders, reduce or close positions and withdraw." : "Your region could not be determined, so trading is not available. You can still cancel orders, reduce or close positions and withdraw.";
}


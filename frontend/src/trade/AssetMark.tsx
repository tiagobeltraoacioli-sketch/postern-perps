/**
 * In-house asset marks: a dressed-stone disc with the asset's monogram and a lamp-coloured ring per asset. Original
 * drawings (no third-party logo), so they can ship under the project's licence.
 */
const TINT: Record<string, string> = { BTC: 'var(--asset-btc)', ETH: 'var(--asset-eth)', SOL: 'var(--asset-sol)' };

export function AssetMark({ asset, size = 24 }: { asset: string; size?: number }) {
  const letters = asset.slice(0, asset.length > 3 ? 2 : 1);
  return (
    <svg className="asset-mark" width={size} height={size} viewBox="0 0 32 32" aria-hidden="true" focusable="false">
      <circle cx="16" cy="16" r="15" fill="var(--tile)" stroke={TINT[asset] ?? 'var(--tile-edge)'} strokeWidth="1.5" />
      <path d="M9 23.5V15a7 7 0 0 1 14 0v8.5" fill="none" stroke={TINT[asset] ?? 'var(--tile-edge)'} strokeOpacity="0.35" strokeWidth="1" />
      <text x="16" y="20.5" textAnchor="middle" fontSize={letters.length > 1 ? 10 : 13} fontFamily="var(--font-display)" fontWeight="600" fill="var(--tile-fg)">
        {letters}
      </text>
    </svg>
  );
}

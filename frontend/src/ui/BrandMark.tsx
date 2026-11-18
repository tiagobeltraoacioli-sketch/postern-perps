/** The Postern mark (from the Bloch DEX design system, src/shell/BrandMark.tsx of postern-dex): wall, gate, lamp. */
export function BrandMark({ size = 30 }: { size?: number }) {
  return (
    <svg className="brand-mark" width={size} height={size} viewBox="0 0 32 32" aria-hidden="true" focusable="false">
      <rect x="0.5" y="0.5" width="31" height="31" rx="6.5" fill="var(--tile)" stroke="var(--tile-edge)" />
      <path d="M10.5 26V16.5a5.5 5.5 0 0 1 11 0V26Z" fill="var(--tile-fg)" />
      <rect x="6" y="26" width="20" height="1.5" rx="0.75" fill="var(--tile-fg)" opacity="0.55" />
      <circle className="brand-lamp" cx="16" cy="6.75" r="1.75" fill="var(--net-bright)" />
    </svg>
  );
}

/** The gate glyph on its own (empty states). */
export function GateGlyph({ size = 18 }: { size?: number }) {
  return (
    <svg className="gate-glyph" width={size} height={size} viewBox="0 0 18 18" fill="none" stroke="currentColor" strokeWidth="1.5" strokeLinecap="round" aria-hidden="true" focusable="false">
      <path d="M4.75 15V8.25a4.25 4.25 0 0 1 8.5 0V15" />
      <path d="M2.5 15h13" />
    </svg>
  );
}

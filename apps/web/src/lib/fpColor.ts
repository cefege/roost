// Deterministic color from a fingerprint string (FNV-1a hash → hue).
// Used by ViewersChip (per-viewer avatar pills). Stable across renders —
// same fp always yields the same hue.

export interface FpColor { hue: number; bg: string; fg: string; }

/** key may be a bare fp or "fp:suffix" (viewerKey) — the suffix is dropped
 *  so a viewer's color tracks the fp, not the per-tab key. */
export function colorForFp(key: string): FpColor {
  const colon = key.indexOf(":");
  const fp = colon >= 0 ? key.slice(0, colon) : key;
  let h = 0x811c9dc5;
  for (let i = 0; i < fp.length; i++) {
    h ^= fp.charCodeAt(i);
    h = (h * 0x01000193) >>> 0;
  }
  const hue = h % 360;
  return {
    hue,
    bg: `hsl(${hue} 60% 28% / 0.85)`,
    fg: `hsl(${hue} 70% 78%)`,
  };
}

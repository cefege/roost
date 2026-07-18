// Canonical theme token contract — the SINGLE source of truth for what a
// theme must define. Every other color token in the app (--md-*,
// --md-sys-color-*, --bg-*, --df-*, catppuccin --base/--text/--blue, status,
// term-bg/fg) is a static `var(--<canonical>)` alias declared once in
// styles/theme-vars.css. A theme supplies ONLY this canonical set and the
// whole alias graph reflows → propagation is structural (lib/theme.ts engine).
//
// Adding a theme = add one Theme object to lib/themes.ts. The
// Record<CanonicalToken,string> type makes omitting any token a compile error.

export const CANONICAL_TOKENS = [
  // Surfaces (low → high elevation) + the terminal's surrounding bg.
  "bg-base", "surface-0", "surface-1", "surface-2", "surface-3", "term-bg",
  // Text (high → low emphasis) + terminal foreground.
  "text-hi", "text-mid", "text-lo", "term-fg",
  // Accent / primary.
  "accent", "on-accent", "accent-container", "on-accent-container",
  // Outlines.
  "border-strong", "border-subtle",
  // Semantic status.
  "status-ok", "status-warn", "status-err", "status-info",
  // Syntax highlighting (code blocks / file viewer).
  "syntax-plain", "syntax-keyword", "syntax-string", "syntax-number", "syntax-comment",
  // Selection / active tint — M3 secondary-container drives every selected
  // state (nav rail, list-row, theme tile). Pairs with on-secondary-container
  // for the text/icon on top. Replaces the old ad-hoc bg/border-selected tints.
  "secondary-container", "on-secondary-container",
  // ANSI 16. Live-wired to the wterm grid: theme-vars.css aliases :root
  // --term-color-0..15 → these --ansi-* tokens, and sidebar.css's .wterm
  // block re-points wterm's own --term-color-N (which it scopes to .wterm,
  // shadowing :root) at the same --ansi-* sources. @wterm/dom's renderer
  // emits inline var(--term-color-N) per cell, so theme switches reflow the
  // grid with no re-render. Also feeds app-chrome consumers that read
  // --term-color-N outside the grid (SessionRow status dots, StatusGlyph).
  "ansi-black", "ansi-red", "ansi-green", "ansi-yellow",
  "ansi-blue", "ansi-magenta", "ansi-cyan", "ansi-white",
  "ansi-bright-black", "ansi-bright-red", "ansi-bright-green", "ansi-bright-yellow",
  "ansi-bright-blue", "ansi-bright-magenta", "ansi-bright-cyan", "ansi-bright-white",
] as const;

type CanonicalToken = (typeof CANONICAL_TOKENS)[number];

type ThemeAppearance = "light" | "dark";
export type ThemeGroup = "System" | "Light" | "Dark" | "Palette";

export interface Theme {
  id: string;
  label: string;
  group: ThemeGroup;
  appearance: ThemeAppearance;
  // Complete canonical palette. The Record type forces every token present.
  tokens: Record<CanonicalToken, string>;
}

// BrandMark — Roost's canonical perched-bird brand mark. Identical geometry to
// apps/web/public/icon.svg (the favicon / PWA manifest / apple-touch source), so
// the tab icon and the in-app wordmark stay one mark. Hand-authored overlapping
// primitives sharing a single fill (clean union, no winding-rule holes).
//
// Convention mirrors AgentMarks.tsx: viewBox 0 0 24 24, fill="currentColor",
// aria-hidden, crisp at 16px. The mark is the FIXED brand identity color
// (--brand-coral #db7556), theme-independent — a brand mark must be stable
// across themes, unlike --accent which retints. Mirrors how the sidebar
// selected-rail tick + terminal-tab glyph use --brand-coral.
//
// Callers: HomeLanding (home wordmark), sidebar/AllView (sidebar brand row).
import type { Component } from "solid-js";
import type { JSX } from "solid-js";

export const BrandMark: Component<{
  size?: number;
  class?: string;
  style?: JSX.CSSProperties;
}> = (props) => {
  const s = () => props.size ?? 24;
  return (
    <svg
      viewBox="0 0 24 24"
      width={s()}
      height={s()}
      fill="currentColor"
      aria-hidden="true"
      class={props.class}
      style={{ color: "var(--brand-coral)", "flex-shrink": "0", ...props.style }}
    >
      {/* body (egg) + head bump + right beak + lower-left tail — same fill unions into one silhouette */}
      <ellipse cx="11.5" cy="12.5" rx="6.2" ry="6.4" />
      <circle cx="14.5" cy="8" r="3.6" />
      <polygon points="17.5,6.4 19.9,8.8 17.5,11.2" />
      <polygon points="8.5,10.5 4.0,18.6 11.0,19.0" />
      {/* perch: separate rounded bar = terminal baseline (the mark's "roost") */}
      <rect x="4.8" y="19" width="14.4" height="1.5" rx="0.75" />
    </svg>
  );
};

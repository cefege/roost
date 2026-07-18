// The ONE folder mark for the whole app — Google/Drive's filled manila-folder
// silhouette (Material "folder" icon, flat fill). Replaces the off-brand
// split-pane rect+path (FolderList pane-count chip) and the generic Lucide
// outline folder (HomeLanding) so every surface shows the same recognizable
// glyph. Callers control hue via `color` / `style` — the fill is
// `currentColor`, no stroke, no gradient, no gloss.

import type { JSX } from "solid-js";

export function FolderGlyph(props: {
  size?: number;
  class?: string;
  style?: JSX.CSSProperties;
  title?: string;
}): JSX.Element {
  return (
    <svg
      class={props.class ?? "df-folder-glyph"}
      width={props.size ?? 14}
      height={props.size ?? 14}
      viewBox="0 0 24 24"
      fill="currentColor"
      aria-hidden={props.title ? undefined : "true"}
      role={props.title ? "img" : undefined}
      style={props.style}
    >
      {props.title ? <title>{props.title}</title> : null}
      <path d="M10 4H4c-1.1 0-1.99.9-1.99 2L2 18c0 1.1.9 2 2 2h16c1.1 0 2-.9 2-2V8c0-1.1-.9-2-2-2h-8l-2-2z" />
    </svg>
  );
}

// The file/document mark for the browse page's view-only file tiles & rows —
// the filled counterpart to FolderGlyph's filled manila folder. Same flat
// `currentColor` fill, no stroke, no gradient: one glyph language across
// folders and files. Callers control hue via `color` / `style`. The path is the
// Material "file" silhouette (body + folded corner) so it reads as a document
// at both the 40px grid size and the 20px list size.

import type { JSX } from "solid-js";

export function FileGlyph(props: {
  size?: number;
  class?: string;
  style?: JSX.CSSProperties;
  title?: string;
}): JSX.Element {
  return (
    <svg
      class={props.class ?? "df-file-glyph"}
      width={props.size ?? 14}
      height={props.size ?? 14}
      viewBox="0 0 24 24"
      fill="currentColor"
      aria-hidden={props.title ? undefined : "true"}
      role={props.title ? "img" : undefined}
      style={props.style}
    >
      {props.title ? <title>{props.title}</title> : null}
      <path d="M14 2H6c-1.1 0-2 .9-2 2v16c0 1.1.9 2 2 2h12c1.1 0 2-.9 2-2V8l-6-6zm-1 7V3.5L18.5 9H13z" />
    </svg>
  );
}

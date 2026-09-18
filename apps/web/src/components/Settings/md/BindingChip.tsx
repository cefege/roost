// BindingChip — the one key-cap rendering for an input binding: a <kbd> in the
// label-small mono ramp. Two consumers, one style: HelpOverlay's shortcut
// catalogue and PadHintBar's controller legend.
// Depends on: theme tokens only (no state, no store reads).

import type { JSX } from "solid-js";

export function BindingChip(props: { children: JSX.Element }) {
  return (
    <kbd style={{
      font: "var(--md-label-s-weight) var(--md-label-s-size)/var(--md-label-s-line) var(--font-mono)",
      padding: "var(--md-space-1) var(--md-space-2)",
      "border-radius": "var(--md-shape-xs)",
      background: "var(--surface-1)",
      border: "var(--workbench-border-width) solid var(--md-sys-color-outline-variant)",
      color: "var(--text-mid)",
      "white-space": "nowrap",
    }}>
      {props.children}
    </kbd>
  );
}

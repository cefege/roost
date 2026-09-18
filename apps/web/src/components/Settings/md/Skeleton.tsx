// Placeholder bar primitive for loading states that must hold a row's shape.
// Consumers render it where text will appear (list-row headlines, inline captions);
// callers may narrow it with a CSS length, and the shared `.md-skeleton` rule owns
// every visual value including the default width.

import { type Component } from "solid-js";

export const Skeleton: Component<{ width?: string; class?: string }> = (props) => (
  <span
    class={props.class ? `md-skeleton ${props.class}` : "md-skeleton"}
    aria-hidden="true"
    style={props.width ? { "inline-size": props.width } : undefined}
  />
);

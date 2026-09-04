// Token-driven Surface is the single owner of panel background, shape, border, and elevation.
// Screens and primitives choose its semantic element and provide accessible labelling.
// Theme variables supply every visual value; callers retain only content layout.

import { type JSX, type Component } from "solid-js";
import { Dynamic } from "solid-js/web";

export const Surface: Component<{
  as?: "div" | "section";
  level?: 0 | 1 | 2 | 3;
  elevation?: 0 | 1 | 2 | 3 | 4 | 5;
  radius?: "xs" | "sm" | "md" | "lg" | "xl" | "full";
  pad?: 1 | 2 | 3 | 4 | 5 | 6 | 7 | 8 | 9;
  border?: boolean;
  class?: string;
  style?: JSX.CSSProperties;
  onClick?: () => void;
  "data-testid"?: string;
  "aria-labelledby"?: string;
  children: JSX.Element;
}> = (props) => (
  <Dynamic
    component={props.as ?? "div"}
    class={props.class}
    onClick={props.onClick}
    attr:data-testid={props["data-testid"]}
    attr:aria-labelledby={props["aria-labelledby"]}
    style={{
      background: `var(--surface-${props.level ?? 1})`,
      "box-shadow": `var(--md-elev-${props.elevation ?? 0})`,
      "border-radius": `var(--md-shape-${props.radius ?? "md"})`,
      ...(props.pad !== undefined ? { padding: `var(--md-space-${props.pad})` } : {}),
      ...(props.border ? { border: "1px solid var(--md-outline-variant)" } : {}),
      ...props.style,
    }}
  >
    {props.children}
  </Dynamic>
);

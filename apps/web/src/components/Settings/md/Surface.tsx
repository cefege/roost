import { type JSX, type Component } from "solid-js";

// ─── Surface — token-driven panel (design-system phase 1) ───────────────────
// The one primitive for "a piece of chrome with a background". Every prop maps
// to a theme token so a Surface can NEVER hardcode a color/shadow/radius the
// way the old AppShell collapsed rail did (`#262626`). Chrome composes this
// instead of hand-rolling <div style>.
export const Surface: Component<{
  level?: 0 | 1 | 2 | 3;                    // background → --surface-N
  elevation?: 0 | 1 | 2 | 3 | 4 | 5;        // box-shadow → --md-elev-N
  radius?: "xs" | "sm" | "md" | "lg" | "xl" | "full";  // → --md-shape-*
  pad?: 1 | 2 | 3 | 4 | 5 | 6 | 7 | 8 | 9;  // padding → --md-space-N
  border?: boolean;                          // 1px --md-outline-variant
  class?: string;
  style?: JSX.CSSProperties;
  onClick?: () => void;
  children: JSX.Element;
}> = (props) => (
  <div
    class={props.class}
    onClick={props.onClick}
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
  </div>
);

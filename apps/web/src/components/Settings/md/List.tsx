import { type JSX, type Component } from "solid-js";

// ─── List ──────────────────────────────────────────────────────────
export const List: Component<{
  contained?: boolean;
  class?: string;
  children: JSX.Element;
}> = (props) => (
  <div class={`md-list ${props.contained ? "md-list--container" : ""} ${props.class ?? ""}`}>
    {props.children}
  </div>
);

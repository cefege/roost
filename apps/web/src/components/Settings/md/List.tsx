import { type JSX, type Component } from "solid-js";

// ─── List ──────────────────────────────────────────────────────────
export const List: Component<{
  contained?: boolean;
  layout?: "stack" | "grid";
  class?: string;
  children: JSX.Element;
}> = (props) => (
  <div
    class={`md-list ${props.contained ? "md-list--container" : ""}${
      props.layout === "grid" ? " md-list--grid" : ""
    } ${props.class ?? ""}`}
  >
    {props.children}
  </div>
);

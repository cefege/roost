import { type JSX, type Component } from "solid-js";
import "./icon.css";

// ─── Material Symbols (Rounded) ─────────────────────────────────────
export const Icon: Component<{
  name: string;
  filled?: boolean;
  size?: "sm" | "md" | "lg";
  class?: string;
  style?: JSX.CSSProperties | string;
}> = (props) => (
  <span
    aria-hidden="true"
    class={`md-icon ${props.filled ? "md-icon--filled" : ""} ${props.size === "sm" ? "md-icon--sm" : props.size === "lg" ? "md-icon--lg" : ""} ${props.class ?? ""}`}
    style={props.style}
  >
    {props.name}
  </span>
);

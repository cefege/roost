// Shared flat content surface for settings and product screens.
// Callers provide optional heading/supporting metadata and trailing actions.
// tokens.css owns the common bordered/elevated presentation variants.

import { type JSX, type Component, Show } from "solid-js";

export const Card: Component<{
  title?: string;
  supporting?: string;
  trailing?: JSX.Element;
  variant?: "filled" | "elevated" | "outlined";
  class?: string;
  style?: JSX.CSSProperties;
  "data-testid"?: string;
  children: JSX.Element;
}> = (props) => (
  <section
    class={`md-card ${props.variant === "elevated" ? "md-card--elevated" : props.variant === "outlined" ? "md-card--outlined" : ""} ${props.class ?? ""}`}
    style={props.style}
    data-testid={props["data-testid"]}
  >
      <Show when={props.title || props.trailing || props.supporting}>
        <header class="md-card__header">
          <div style={{ display: "flex", "flex-direction": "column", gap: "4px", "min-width": 0, flex: 1 }}>
            <Show when={props.title}>
              <h2 class="md-card__title">{props.title}</h2>
            </Show>
            <Show when={props.supporting}>
              <p class="md-card__supporting">{props.supporting}</p>
            </Show>
          </div>
          <Show when={props.trailing}>{props.trailing}</Show>
        </header>
      </Show>
      {props.children}
    </section>
  );

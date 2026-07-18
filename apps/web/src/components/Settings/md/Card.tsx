import { type JSX, type Component, Show } from "solid-js";

// ─── Card ───────────────────────────────────────────────────────────
// variant: filled (default) | elevated | outlined. Title + supporting
// + a trailing action all optional. Children render under the header.
export const Card: Component<{
  title?: string;
  supporting?: string;
  trailing?: JSX.Element;
  variant?: "filled" | "elevated" | "outlined";
  class?: string;
  children: JSX.Element;
}> = (props) => {
  const variantClass = () =>
    props.variant === "elevated" ? "md-card--elevated"
      : props.variant === "outlined" ? "md-card--outlined"
        : "";
  return (
    <section class={`md-card ${variantClass()} ${props.class ?? ""}`}>
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
};

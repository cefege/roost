import { type JSX, type Component, splitProps, Show } from "solid-js";
import { Dynamic } from "solid-js/web";
import "@material/web/button/filled-button.js";
import "@material/web/button/filled-tonal-button.js";
import "@material/web/button/text-button.js";
import { Icon } from "./Icon.tsx";

// ─── Buttons → real md-button (ripple + state-layer + focus ring + motion) ───
type ButtonProps = JSX.ButtonHTMLAttributes<HTMLButtonElement> & {
  variant?: "filled" | "tonal" | "text";
  icon?: string;
};
export const Button: Component<ButtonProps> = (props) => {
  // Pull data-testid out of the spread: on a custom element Dynamic sets spread
  // props as PROPERTIES, so data-testid never lands as a queryable attribute.
  // attr: forces the attribute (see also the standalone primitives above).
  const [own, rest] = splitProps(props as ButtonProps & { "data-testid"?: string },
    ["variant", "icon", "children", "class", "type", "data-testid"]);
  const tag = () =>
    own.variant === "filled" ? "md-filled-button"
      : own.variant === "text" ? "md-text-button"
        : "md-filled-tonal-button";
  return (
    // md-button variants are custom elements; Dynamic spreads onClick/disabled
    // through and the leading icon goes in slot="icon".
    <Dynamic component={tag()} class={own.class} type={own.type ?? "button"} attr:data-testid={own["data-testid"]} {...rest}>
      <Show when={own.icon}>
        <span slot="icon" style={{ display: "inline-flex" }}>
          <Icon name={own.icon!} size="sm" />
        </span>
      </Show>
      {own.children}
    </Dynamic>
  );
};

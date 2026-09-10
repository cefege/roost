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
type ButtonAttributes = ButtonProps & {
  "data-testid"?: string;
  "data-selected"?: string;
  "data-active"?: string;
  "aria-selected"?: boolean | string;
  "aria-current"?: string;
  "aria-label"?: string;
  role?: string;
};
export const Button: Component<ButtonProps> = (props) => {
  // Custom Material elements do not reliably reflect spread properties as
  // host attributes; explicit attr bindings keep selectors and assistive
  // technology on the element users actually focus.
  const [own, rest] = splitProps(props as ButtonAttributes, [
    "variant", "icon", "children", "class", "type", "role", "data-testid",
    "data-selected", "data-active", "aria-selected", "aria-current", "aria-label",
  ]);
  const tag = () =>
    own.variant === "filled" ? "md-filled-button"
      : own.variant === "text" ? "md-text-button"
        : "md-filled-tonal-button";
  return (
    // md-button variants are custom elements; Dynamic spreads onClick/disabled
    // through and the leading icon goes in slot="icon".
    <Dynamic
      component={tag()}
      attr:class={own.class}
      attr:type={own.type ?? "button"}
      attr:role={own.role}
      attr:data-testid={own["data-testid"]}
      attr:data-selected={own["data-selected"]}
      attr:data-active={own["data-active"]}
      attr:aria-selected={own["aria-selected"]}
      attr:aria-current={own["aria-current"]}
      attr:aria-label={own["aria-label"]}
      {...rest}
    >
      <Show when={own.icon}>
        <span slot="icon" style={{ display: "inline-flex" }}>
          <Icon name={own.icon!} size="sm" />
        </span>
      </Show>
      {own.children}
    </Dynamic>
  );
};

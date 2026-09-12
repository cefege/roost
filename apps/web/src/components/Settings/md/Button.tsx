// Native settings button primitive.
// Callers receive shared variants, sizing, optional leading icons, and native button props.
// Shared control CSS owns the visual treatment.

import { type Component, type JSX, Show, splitProps } from "solid-js";
import { Icon } from "./Icon.tsx";

export type ButtonVariant =
  | "default"
  | "secondary"
  | "outline"
  | "ghost"
  | "destructive"
  | "link";

export type ButtonSize =
  | "xs"
  | "sm"
  | "default"
  | "lg"
  | "icon-xs"
  | "icon-sm"
  | "icon"
  | "icon-lg";

export type ButtonProps = JSX.ButtonHTMLAttributes<HTMLButtonElement> & {
  variant?: ButtonVariant;
  size?: ButtonSize;
  icon?: string;
};

export const Button: Component<ButtonProps> = (props) => {
  const [own, rest] = splitProps(props, [
    "variant",
    "size",
    "icon",
    "children",
    "class",
    "type",
  ]);

  return (
    <button
      {...rest}
      type={own.type ?? "button"}
      class={`roost-button roost-button--${own.variant ?? "default"} roost-button--${own.size ?? "default"}${own.class ? ` ${own.class}` : ""}`}
    >
      <Show when={own.icon}>
        <Icon name={own.icon!} size="sm" />
      </Show>
      {own.children}
    </button>
  );
};

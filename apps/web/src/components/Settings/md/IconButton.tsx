// Native icon button built from the shared Button primitive.
// It owns accessible icon-only semantics and menu-trigger ARIA mappings.
// Shared control CSS owns its visual treatment.

import { type Component, type JSX, splitProps } from "solid-js";
import { Button, type ButtonProps, type ButtonSize } from "./Button.tsx";
import { Icon } from "./Icon.tsx";

export type IconButtonSize = Extract<
  ButtonSize,
  "icon-xs" | "icon-sm" | "icon" | "icon-lg"
>;

export type IconButtonProps = Omit<ButtonProps, "children" | "icon" | "size"> & {
  icon: string;
  label: string;
  size?: IconButtonSize;
  menuPopup?: JSX.AriaAttributes["aria-haspopup"];
  controlsId?: string;
  expanded?: boolean;
};

export const IconButton: Component<IconButtonProps> = (props) => {
  const [own, rest] = splitProps(props, [
    "icon",
    "label",
    "menuPopup",
    "controlsId",
    "expanded",
    "class",
    "variant",
    "size",
  ]);

  return (
    <Button
      {...rest}
      class={`roost-icon-button${own.class ? ` ${own.class}` : ""}`}
      variant={own.variant ?? "ghost"}
      size={own.size ?? "icon"}
      aria-label={own.label}
      aria-haspopup={own.menuPopup}
      aria-controls={own.controlsId}
      aria-expanded={own.expanded}
    >
      <Icon name={own.icon} />
    </Button>
  );
};

import { type JSX, type Component, splitProps } from "solid-js";
import { Dynamic } from "solid-js/web";
import "@material/web/iconbutton/icon-button.js";
import { Icon } from "./Icon.tsx";

// ─── Icon button → real md-icon-button (ripple + 48px touch target free) ─────
// The 48px touch target is an internal overflow pseudo, so the VISUAL size can
// stay dense (set via the `style` width/height + --md-icon-button-icon-size)
// without shrinking the tap area — fixes the sub-48px ✕/icon buttons the audit
// flagged without bloating dense rows.
export const IconButton: Component<
  JSX.ButtonHTMLAttributes<HTMLButtonElement> & { icon: string; label: string }
> = (props) => {
  const [own, rest] = splitProps(props, ["icon", "label", "children", "type"]);
  return (
    <Dynamic component="md-icon-button" aria-label={own.label} type={own.type ?? "button"} {...rest}>
      <Icon name={own.icon} />
    </Dynamic>
  );
};

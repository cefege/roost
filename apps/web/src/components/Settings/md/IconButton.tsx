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
  JSX.ButtonHTMLAttributes<HTMLButtonElement> & { icon: string; label: string; "data-testid"?: string }
> = (props) => {
  const [own, rest] = splitProps(props, ["icon", "label", "children", "type", "data-testid"]);
  return (
    // Solid's runtime spread sets unknown keys as PROPERTIES on a hyphenated
    // custom element, so a spread `data-testid` lands as el["data-testid"] and
    // is invisible to querySelector. Force attribute mode with attr: so test
    // hooks resolve. (Static JSX on md-icon-button attributes it correctly; the
    // spread path does not.)
    <Dynamic
      component="md-icon-button"
      aria-label={own.label}
      type={own.type ?? "button"}
      attr:data-testid={own["data-testid"]}
      {...rest}
    >
      <Icon name={own.icon} />
    </Dynamic>
  );
};

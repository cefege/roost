// Wraps Material Web's icon button for shared ROOST icon controls.
// Callers receive the standard touch target, icon, label, and native events.
// Explicit attribute forwarding keeps accessibility state on the custom element.

import { createEffect, type JSX, type Component, splitProps } from "solid-js";
import { Dynamic } from "solid-js/web";
import "@material/web/iconbutton/icon-button.js";
import { Icon } from "./Icon.tsx";

// ─── Icon button → real md-icon-button (ripple + 48px touch target free) ─────
// The 48px touch target is an internal overflow pseudo, so the VISUAL size can
// stay dense (set via the `style` width/height + --md-icon-button-icon-size)
// without shrinking the tap area — fixes the sub-48px ✕/icon buttons the audit
// flagged without bloating dense rows.
export const IconButton: Component<
  JSX.ButtonHTMLAttributes<HTMLButtonElement> & {
    icon: string;
    label: string;
    "data-testid"?: string;
    menuPopup?: string;
    controlsId?: string;
    expanded?: boolean;
  }
> = (props) => {
  const [own, rest] = splitProps(props, [
    "icon",
    "label",
    "children",
    "type",
    "data-testid",
    "menuPopup",
    "controlsId",
    "expanded",
    "ref",
  ]);
  let buttonElement: HTMLElement | undefined;
  const syncHostAttributes = () => {
    const label = own.label;
    const testId = own["data-testid"];
    const hasPopup = own.menuPopup;
    const controls = own.controlsId;
    const expanded = own.expanded;
    if (!buttonElement) return;
    setOptionalAttribute(buttonElement, "aria-label", label);
    setOptionalAttribute(buttonElement, "data-testid", testId);
    setOptionalAttribute(buttonElement, "aria-haspopup", hasPopup);
    setOptionalAttribute(buttonElement, "aria-controls", controls);
    setOptionalAttribute(buttonElement, "aria-expanded", expanded);
  };
  createEffect(syncHostAttributes);
  const captureButtonElement = (element: HTMLElement) => {
    buttonElement = element;
    syncHostAttributes();
    if (typeof own.ref === "function") own.ref(element as HTMLButtonElement);
  };

  return (
    <Dynamic
      component="md-icon-button"
      ref={captureButtonElement}
      type={own.type ?? "button"}
      {...rest}
    >
      <Icon name={own.icon} />
    </Dynamic>
  );
};

function setOptionalAttribute(
  element: HTMLElement,
  name: string,
  value: unknown,
): void {
  if (value === undefined || value === null) {
    element.removeAttribute(name);
    return;
  }
  element.setAttribute(name, String(value));
}

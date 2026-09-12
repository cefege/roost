// Shared sheet variants built on the sole Dialog owner.
// It supplies side-specific content classes while Dialog retains modal semantics.
// Consumers keep one content subtree and provide the visible accessible headline.

import { type Component, type JSX } from "solid-js";
import { Dialog } from "./Dialog.tsx";

export const Sheet: Component<{
  open: boolean;
  onClose: () => void;
  headline: string;
  side?: "right" | "bottom" | "center";
  class?: string;
  children: JSX.Element;
  onOpenAutoFocus?: (event: Event) => void;
  showCloseButton?: boolean;
}> = (props) => (
  <Dialog
    open={props.open}
    onClose={props.onClose}
    headline={props.headline}
    class={`roost-sheet--${props.side ?? "right"}${props.class ? ` ${props.class}` : ""}`}
    showCloseButton={props.showCloseButton ?? true}
    onOpenAutoFocus={props.onOpenAutoFocus}
  >
    {props.children}
  </Dialog>
);

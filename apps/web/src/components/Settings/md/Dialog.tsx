// Shared accessible modal primitive.
// It owns Kobalte's portal, modal dismissal, and scroll containment.
// Consumers supply the dialog content and optional action band; Sheet composes it.

import * as KobalteDialog from "@kobalte/core/dialog";
import { type Component, type JSX, Show } from "solid-js";
import { Icon } from "./Icon.tsx";
import "./overlays.css";

export const Dialog: Component<{
  open: boolean;
  onClose: () => void;
  headline?: string;
  children: JSX.Element;
  actions?: JSX.Element;
  description?: JSX.Element;
  class?: string;
  testId?: string;
  onOpenAutoFocus?: (event: Event) => void;
  onCloseAutoFocus?: (event: Event) => void;
  showCloseButton?: boolean;
}> = (props) => {
  let openerElement: HTMLElement | undefined;

  return (
    <KobalteDialog.Dialog
      open={props.open}
      onOpenChange={(open) => { if (!open) props.onClose(); }}
    >
      <KobalteDialog.Dialog.Portal>
        <KobalteDialog.Dialog.Overlay class="roost-dialog__overlay" />
        <KobalteDialog.Dialog.Content
          class={`roost-dialog${props.class ? ` ${props.class}` : ""}`}
          data-testid={props.testId}
          onOpenAutoFocus={(event) => {
            if (document.activeElement instanceof HTMLElement) {
              openerElement = document.activeElement;
            }
            props.onOpenAutoFocus?.(event);
          }}
          onCloseAutoFocus={(event) => {
            props.onCloseAutoFocus?.(event);
            if (event.defaultPrevented || !openerElement?.isConnected) return;
            event.preventDefault();
            openerElement.focus({ preventScroll: true });
          }}
        >
          <Show when={props.headline || props.description || (props.showCloseButton ?? !props.actions)}>
            <div class="roost-dialog__header">
              <div class="roost-dialog__heading">
                <Show when={props.headline}>
                  <KobalteDialog.Dialog.Title class="roost-dialog__title">
                    {props.headline}
                  </KobalteDialog.Dialog.Title>
                </Show>
                <Show when={props.description}>
                  <KobalteDialog.Dialog.Description class="roost-dialog__description">
                    {props.description}
                  </KobalteDialog.Dialog.Description>
                </Show>
              </div>
              <Show when={props.showCloseButton ?? !props.actions}>
                <KobalteDialog.Dialog.CloseButton class="roost-dialog__close" aria-label="Close">
                  <Icon name="close" />
                </KobalteDialog.Dialog.CloseButton>
              </Show>
            </div>
          </Show>
          <div class="roost-dialog__body">{props.children}</div>
          <Show when={props.actions}>
            <div class="roost-dialog__actions">{props.actions}</div>
          </Show>
        </KobalteDialog.Dialog.Content>
      </KobalteDialog.Dialog.Portal>
    </KobalteDialog.Dialog>
  );
};

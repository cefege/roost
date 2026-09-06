// Owns the compact workspace sheet's anchored overflow menu.
// MobileDeckBar supplies tab-selection actions and browser-local layout transfer;
// shared context-menu primitives provide the focusable menu controls and chrome.

import { Show, createSignal } from "solid-js";
import { Portal } from "solid-js/web";
import { IconButton } from "./Settings/md/IconButton.tsx";
import {
  anchoredMenuPosition,
  anchoredMenuSurfaceStyle,
  CtxMenuItem,
  CtxMenuSeparator,
  focusMenuEdge,
  handleMenuKeyboardNavigation,
  trackFloatingMenuDismiss,
} from "./contextMenuPrimitives.tsx";
import type { MenuFocusEdge } from "./contextMenuPrimitives.tsx";

interface WorkspaceTabsMenuProps {
  selectionMode: boolean;
  onCloseAll: () => void;
  onSelectTabs: () => void;
  onSelectAll: () => void;
  onCloseSelected: () => void;
  onCopyLayout: () => void;
  onDownloadLayout: () => void;
  onImportLayout: () => void;
}

export function WorkspaceTabsMenu(props: WorkspaceTabsMenuProps) {
  const [open, setOpen] = createSignal<{ right: number; y: number } | null>(null);
  let buttonElement: HTMLButtonElement | undefined;
  let menuElement: HTMLDivElement | undefined;
  let cancelPendingFocus: (() => void) | null = null;

  const openMenu = (edge: MenuFocusEdge = "first") => {
    if (!buttonElement) return;
    cancelPendingFocus?.();
    setOpen(anchoredMenuPosition(buttonElement));
    cancelPendingFocus = focusMenuEdge(() => menuElement, edge);
  };
  const closeMenu = (restoreTriggerFocus = false) => {
    cancelPendingFocus?.();
    cancelPendingFocus = null;
    setOpen(null);
    if (restoreTriggerFocus) queueMicrotask(() => buttonElement?.focus());
  };
  const toggle = () => {
    if (open()) {
      closeMenu();
      return;
    }
    openMenu();
  };
  const choose = (action: () => void) => {
    closeMenu(true);
    action();
  };
  const onTriggerKeyDown = (event: KeyboardEvent) => {
    if (event.key !== "ArrowDown" && event.key !== "ArrowUp") return;
    event.preventDefault();
    event.stopPropagation();
    openMenu(event.key === "ArrowDown" ? "first" : "last");
  };
  const onMenuKeyDown = (event: KeyboardEvent) => {
    handleMenuKeyboardNavigation(
      event,
      menuElement,
      () => closeMenu(true),
      () => closeMenu(),
    );
  };
  const surfaceStyle = (position: { right: number; y: number }) =>
    anchoredMenuSurfaceStyle(position, {
      minWidth: "calc(var(--md-space-9) * 6)",
      zIndex: 70,
    });

  trackFloatingMenuDismiss({
    within: [() => buttonElement, () => menuElement],
    onClose: () => closeMenu(),
  });

  return (
    <>
      <IconButton
        ref={buttonElement}
        id="workspace-tabs-menu-trigger"
        icon="more_vert"
        label="More options"
        data-testid="workspace-tabs-menu"
        onClick={toggle}
        menuPopup="menu"
        controlsId="workspace-tabs-menu-popup"
        expanded={open() !== null}
        onKeyDown={onTriggerKeyDown}
      />
      <Show when={open()}>
        {(position) => (
          <Portal>
            <div
              ref={menuElement}
              role="menu"
              aria-label="Workspace terminal actions"
              id="workspace-tabs-menu-popup"
              aria-labelledby="workspace-tabs-menu-trigger"
              data-testid="workspace-tabs-menu-popup"
              class="df-menu-enter"
              style={surfaceStyle(position())}
              onKeyDown={onMenuKeyDown}
            >
              <Show when={!props.selectionMode}>
                <CtxMenuItem testid="workspace-tabs-close-all" danger onClick={() => choose(props.onCloseAll)}>
                  Close all tabs
                </CtxMenuItem>
                <CtxMenuItem testid="workspace-tabs-select" onClick={() => choose(props.onSelectTabs)}>
                  Select tabs
                </CtxMenuItem>
                <CtxMenuSeparator />
                <CtxMenuItem testid="layout-copy" onClick={() => choose(props.onCopyLayout)}>
                  Copy layout
                </CtxMenuItem>
                <CtxMenuItem testid="layout-download" onClick={() => choose(props.onDownloadLayout)}>
                  Download layout
                </CtxMenuItem>
                <CtxMenuItem testid="layout-import" onClick={() => choose(props.onImportLayout)}>
                  Import layout…
                </CtxMenuItem>
              </Show>
              <Show when={props.selectionMode}>
                <CtxMenuItem testid="workspace-tabs-select-all" onClick={() => choose(props.onSelectAll)}>
                  Select all
                </CtxMenuItem>
                <CtxMenuItem testid="workspace-tabs-close-selected" danger onClick={() => choose(props.onCloseSelected)}>
                  Close selected tabs
                </CtxMenuItem>
              </Show>
            </div>
          </Portal>
        )}
      </Show>
    </>
  );
}

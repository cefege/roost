// DesignOverlayStates renders the modal and menu reference for /design.
// It delegates focus, dismissal, portal, and keyboard behavior to shipped owners.
// DesignGallery supplies the surrounding catalog surface and theme tokens.

import { type Component, createSignal, Show } from "solid-js";
import { Portal } from "solid-js/web";
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
import { Button, IconButton, Sheet, Dialog } from "./Settings/md/primitives.tsx";

export const DesignOverlayStates: Component = () => {
  const [dialogOpen, setDialogOpen] = createSignal(false);
  const [sheetOpen, setSheetOpen] = createSignal(false);
  const [centeredSheetOpen, setCenteredSheetOpen] = createSignal(false);
  const [menuOpen, setMenuOpen] = createSignal<{ right: number; y: number } | null>(null);
  let menuTrigger: HTMLButtonElement | undefined;
  let menuElement: HTMLDivElement | undefined;
  let cancelPendingFocus: (() => void) | null = null;

  const closeMenu = (restoreTriggerFocus = false) => {
    cancelPendingFocus?.();
    cancelPendingFocus = null;
    setMenuOpen(null);
    if (restoreTriggerFocus) queueMicrotask(() => menuTrigger?.focus());
  };
  const openMenu = (edge: MenuFocusEdge = "first") => {
    if (!menuTrigger) return;
    cancelPendingFocus?.();
    setMenuOpen(anchoredMenuPosition(menuTrigger));
    cancelPendingFocus = focusMenuEdge(() => menuElement, edge);
  };
  const toggleMenu = () => {
    if (menuOpen()) {
      closeMenu();
      return;
    }
    openMenu();
  };
  const onTriggerKeyDown = (event: KeyboardEvent) => {
    if (event.key !== "ArrowDown" && event.key !== "ArrowUp") return;
    event.preventDefault();
    event.stopPropagation();
    openMenu(event.key === "ArrowDown" ? "first" : "last");
  };

  trackFloatingMenuDismiss({
    within: [() => menuTrigger, () => menuElement],
    onClose: () => closeMenu(),
  });

  return (
    <div style={{ display: "flex", "flex-wrap": "wrap", gap: "var(--md-space-3)", "align-items": "center" }}>
      <Button variant="default" icon="open_in_new" onClick={() => setDialogOpen(true)}>
        Open dialog
      </Button>
      <Button variant="secondary" icon="open_in_full" onClick={() => setSheetOpen(true)}>
        Open sheet
      </Button>
      <Button variant="secondary" icon="open_in_full" onClick={() => setCenteredSheetOpen(true)}>
        Open centered sheet
      </Button>
      <IconButton
        ref={menuTrigger}
        icon="more_vert"
        label="Open context menu"
        menuPopup="menu"
        controlsId="design-context-menu"
        expanded={menuOpen() !== null}
        onClick={toggleMenu}
        onKeyDown={onTriggerKeyDown}
      />

      <Dialog
        open={dialogOpen()}
        onClose={() => setDialogOpen(false)}
        headline="Shared dialog"
        description="The Dialog primitive owns its portal, focus containment, and dismissal."
        showCloseButton={false}
        actions={<Button variant="default" onClick={() => setDialogOpen(false)}>Done</Button>}
      >
        <p style={{ margin: 0, color: "var(--text-mid)", "font-size": "var(--md-body-m-size)", "line-height": "var(--md-body-m-line)" }}>
          Dialog body content remains ordinary application markup.
        </p>
      </Dialog>

      <Sheet open={sheetOpen()} onClose={() => setSheetOpen(false)} headline="Shared sheet" side="right">
        <div style={{ display: "grid", gap: "var(--md-space-4)", "min-inline-size": "min(calc(var(--md-space-9) * 5), 80vw)" }}>
          <p style={{ margin: 0, color: "var(--text-mid)", "font-size": "var(--md-body-m-size)", "line-height": "var(--md-body-m-line)" }}>
            Sheet uses the same Dialog accessibility and dismissal owner with side-specific presentation.
          </p>
        </div>
      </Sheet>

      <Sheet open={centeredSheetOpen()} onClose={() => setCenteredSheetOpen(false)} headline="Centered sheet" side="center">
        <div style={{ display: "grid", gap: "var(--md-space-4)", "min-inline-size": "min(calc(var(--md-space-9) * 5), 80vw)" }}>
          <p style={{ margin: 0, color: "var(--text-mid)", "font-size": "var(--md-body-m-size)", "line-height": "var(--md-body-m-line)" }}>
            Centered sheets provide the shared modal presentation for focused desktop work.
          </p>
        </div>
      </Sheet>

      <Show when={menuOpen()}>
        {(position) => (
          <Portal>
            <div
              ref={menuElement}
              id="design-context-menu"
              role="menu"
              aria-label="Context menu specimen"
              class="df-menu-enter"
              style={anchoredMenuSurfaceStyle(position(), {
                minWidth: "calc(var(--md-space-9) * 5)",
                zIndex: 70,
              })}
              onKeyDown={(event) => handleMenuKeyboardNavigation(
                event,
                menuElement,
                () => closeMenu(true),
                () => closeMenu(),
              )}
            >
              <CtxMenuItem testid="design-context-menu-primary" onClick={() => closeMenu(true)}>
                Primary action
              </CtxMenuItem>
              <CtxMenuItem testid="design-context-menu-disabled" disabled onClick={() => {}}>
                Disabled action
              </CtxMenuItem>
              <CtxMenuSeparator />
              <CtxMenuItem testid="design-context-menu-danger" danger onClick={() => closeMenu(true)}>
                Destructive action
              </CtxMenuItem>
            </div>
          </Portal>
        )}
      </Show>
    </div>
  );
};

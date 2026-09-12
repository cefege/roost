// Browse-page breadcrumb trail: the visible (already-collapsed) crumb strip
// with its overflow menu, plus the hidden measurement mirror that BrowsePage's
// width-aware collapse reads. Split out of BrowsePage.tsx; the page owns the
// collapse math, both element refs, and the menu signals — this file only
// paints them.
//
// Callers: BrowsePage.tsx (WorkerBrowsePage).

import { For, Show } from "solid-js";
import type { Crumb, CrumbView } from "../lib/folderPalette.ts";
import { Button } from "./Settings/md/Button.tsx";
import { IconButton } from "./Settings/md/IconButton.tsx";
import {
  ctxMenuSurfaceStyle,
  CtxMenuItem,
  focusMenuEdge,
  handleMenuKeyboardNavigation,
  trackFloatingMenuDismiss,
} from "./contextMenuPrimitives.tsx";
import type { MenuFocusEdge } from "./contextMenuPrimitives.tsx";

export function BrowseBreadcrumbs(props: {
  /** Collapsed views the strip paints (collapseCrumbsTo output). */
  crumbViews: CrumbView[];
  /** Full, uncollapsed trail the hidden mirror measures. */
  crumbs: Crumb[];
  menuOpen: boolean;
  menuPos: { top: number; left: number };
  setMenuOpen: (open: boolean) => void;
  setMenuPos: (pos: { top: number; left: number }) => void;
  onNavigate: (path: string) => void;
  setStripRef: (el: HTMLDivElement) => void;
  setMirrorRef: (el: HTMLDivElement) => void;
}) {
  let crumbOverflowButton: HTMLButtonElement | undefined;
  let crumbMenuElement: HTMLDivElement | undefined;
  let cancelPendingFocus: (() => void) | null = null;

  function closeCrumbMenu(restoreTriggerFocus = false): void {
    if (!props.menuOpen) return;
    cancelPendingFocus?.();
    cancelPendingFocus = null;
    props.setMenuOpen(false);
    if (restoreTriggerFocus) queueMicrotask(() => crumbOverflowButton?.focus());
  }
  function openCrumbMenu(edge: MenuFocusEdge = "first"): void {
    if (!crumbOverflowButton) return;
    cancelPendingFocus?.();
    const bounds = crumbOverflowButton.getBoundingClientRect();
    props.setMenuPos({ top: bounds.bottom + 4, left: bounds.left });
    props.setMenuOpen(true);
    cancelPendingFocus = focusMenuEdge(() => crumbMenuElement, edge);
  }
  function toggleCrumbMenu(): void {
    if (props.menuOpen) closeCrumbMenu();
    else openCrumbMenu();
  }
  function chooseCrumb(path: string): void {
    closeCrumbMenu();
    props.onNavigate(path);
  }
  function onOverflowTriggerKeyDown(event: KeyboardEvent): void {
    if (event.key === "ArrowDown" || event.key === "ArrowUp") {
      event.preventDefault();
      event.stopPropagation();
      openCrumbMenu(event.key === "ArrowDown" ? "first" : "last");
    } else if (event.key === "Escape" && props.menuOpen) {
      event.preventDefault();
      event.stopPropagation();
      closeCrumbMenu();
    }
  }
  function onCrumbMenuKeyDown(event: KeyboardEvent): void {
    handleMenuKeyboardNavigation(
      event,
      crumbMenuElement,
      () => closeCrumbMenu(true),
      () => closeCrumbMenu(),
    );
  }

  trackFloatingMenuDismiss({
    within: [() => crumbOverflowButton, () => crumbMenuElement],
    onClose: () => closeCrumbMenu(),
    onEscape: () => closeCrumbMenu(true),
  });

  return (
    <>
      <div class="df-browse-crumbs" ref={props.setStripRef} data-testid="browse-crumbs">
        <For each={props.crumbViews}>
          {(view, index) => (
            <>
              <Show when={index() > 0}>
                <span class="df-browse-crumb-sep" aria-hidden="true">▸</span>
              </Show>
              <Show
                when={view.kind === "crumb"}
                fallback={
                  <IconButton
                    ref={crumbOverflowButton}
                    id="browse-crumb-overflow"
                    class="df-browse-crumb-overflow"
                    data-testid="browse-crumb-overflow"
                    icon="more_horiz"
                    label="Show hidden folders"
                    title="Show hidden folders"
                    menuPopup="menu"
                    controlsId="browse-crumb-menu"
                    expanded={props.menuOpen}
                    onClick={toggleCrumbMenu}
                    onKeyDown={onOverflowTriggerKeyDown}
                  />
                }
              >
                <Button
                  class="df-browse-crumb"
                  variant={index() === props.crumbViews.length - 1 ? "secondary" : "ghost"}
                  size="sm"
                  data-testid="browse-crumb"
                  data-current={index() === props.crumbViews.length - 1 ? "true" : undefined}
                  aria-current={index() === props.crumbViews.length - 1 ? "page" : undefined}
                  onClick={() => chooseCrumb((view as Extract<CrumbView, { kind: "crumb" }>).path)}
                  title={(view as Extract<CrumbView, { kind: "crumb" }>).path}
                >
                  {(view as Extract<CrumbView, { kind: "crumb" }>).label}
                </Button>
              </Show>
            </>
          )}
        </For>
      </div>
      <Show when={props.menuOpen}>
        <div
          ref={crumbMenuElement}
          id="browse-crumb-menu"
          class="df-menu-enter df-browse-crumb-menu"
          data-testid="browse-crumb-menu"
          role="menu"
          aria-labelledby="browse-crumb-overflow"
          style={ctxMenuSurfaceStyle(props.menuPos.left, props.menuPos.top)}
          onKeyDown={onCrumbMenuKeyDown}
        >
          <For each={props.crumbViews}>
            {(view) => (
              <Show when={view.kind === "ellipsis"}>
                <For each={(view as Extract<CrumbView, { kind: "ellipsis" }>).hidden}>
                  {(hidden) => (
                    <CtxMenuItem
                      class="df-browse-crumb-menu-item"
                      testid="browse-crumb-menu-item"
                      title={hidden.path}
                      onClick={() => chooseCrumb(hidden.path)}
                    >
                      <span>{hidden.label}</span>
                    </CtxMenuItem>
                  )}
                </For>
              </Show>
            )}
          </For>
        </div>
      </Show>
      <div class="df-browse-crumbs-measure" ref={props.setMirrorRef} aria-hidden="true">
        <For each={props.crumbs}>
          {(crumb, index) => (
            <>
              <Show when={index() > 0}>
                <span class="df-browse-crumb-sep" data-mirror-sep aria-hidden="true">▸</span>
              </Show>
              <button type="button" class="roost-button roost-button--ghost roost-button--sm df-browse-crumb"
                data-mirror-crumb tabIndex={-1}>{crumb.label}</button>
            </>
          )}
        </For>
        <span class="df-browse-crumb-sep" aria-hidden="true">▸</span>
        <button type="button"
          class="roost-button roost-button--ghost roost-button--icon roost-icon-button df-browse-crumb-overflow"
          data-mirror-overflow tabIndex={-1}>…</button>
      </div>
    </>
  );
}

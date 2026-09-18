// Folder-picker path band: history/parent/home navigation beside the collapsed
// breadcrumb trail and its overflow menu, plus the hidden measurement mirror the
// width-aware collapse reads. The same band carries the filter field instead of
// the trail while filtering, so filtering costs the surface no height. The page
// owns the collapse math, both element refs, and every signal read here.
//
// Callers: WorkerBrowsePage.tsx.

import { For, Show } from "solid-js";
import type { Crumb, CrumbView } from "../lib/folderPalette.ts";
import { Icon } from "./Settings/md/Icon.tsx";
import { Button } from "./Settings/md/Button.tsx";
import { Surface } from "./Settings/md/Surface.tsx";
import { IconButton } from "./Settings/md/IconButton.tsx";
import { TextField, type TextFieldElement } from "./Settings/md/TextField.tsx";
import {
  ctxMenuSurfaceStyle,
  CtxMenuItem,
  focusMenuEdge,
  handleMenuKeyboardNavigation,
  trackFloatingMenuDismiss,
} from "./contextMenuPrimitives.tsx";
import type { MenuFocusEdge } from "./contextMenuPrimitives.tsx";

export function BrowsePathBar(props: {
  /** Collapsed views the strip paints (collapseCrumbsTo output). */
  crumbViews: CrumbView[];
  /** Full, uncollapsed trail the hidden mirror measures. */
  crumbs: Crumb[];
  menuOpen: boolean;
  menuPos: { top: number; left: number };
  backEnabled: boolean;
  forwardEnabled: boolean;
  upEnabled: boolean;
  filterOpen: boolean;
  filter: string;
  setMenuOpen: (open: boolean) => void;
  setMenuPos: (pos: { top: number; left: number }) => void;
  onNavigate: (path: string) => void;
  onBack: () => void;
  onForward: () => void;
  onUp: () => void;
  onHome: () => void;
  onFilter: (value: string) => void;
  onCloseFilter: () => void;
  setStripRef: (element: HTMLElement) => void;
  setMirrorRef: (element: HTMLDivElement) => void;
  setFilterRef: (element: TextFieldElement) => void;
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
      <Surface class="df-browse-path" level={1} radius="none">
        <Show
          when={props.filterOpen}
          fallback={
            <>
              <IconButton size="icon-sm" data-testid="browse-back" icon="arrow_back"
                label="Back" title="Back" disabled={!props.backEnabled} onClick={props.onBack} />
              <IconButton size="icon-sm" data-testid="browse-forward" icon="arrow_forward"
                label="Forward" title="Forward" disabled={!props.forwardEnabled} onClick={props.onForward} />
              <IconButton size="icon-sm" data-testid="browse-up" icon="arrow_upward"
                label="Parent folder" title="Parent folder" disabled={!props.upEnabled} onClick={props.onUp} />
              <IconButton size="icon-sm" data-testid="browse-home" icon="home"
                label="Home folder" title="Home folder" onClick={props.onHome} />
              <div class="df-browse-crumbs" ref={props.setStripRef} data-testid="browse-crumbs">
                <For each={props.crumbViews}>
                  {(view, index) => (
                    <>
                      <Show when={index() > 0}>
                        <Icon name="chevron_right" size="sm" class="df-browse-crumb-sep" />
                      </Show>
                      <Show
                        when={view.kind === "crumb"}
                        fallback={
                          <IconButton
                            ref={crumbOverflowButton}
                            id="browse-crumb-overflow"
                            class="df-browse-crumb-overflow"
                            data-testid="browse-crumb-overflow"
                            size="icon-sm"
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
              <div class="df-browse-crumbs-measure" ref={props.setMirrorRef} aria-hidden="true">
                <For each={props.crumbs}>
                  {(crumb, index) => (
                    <>
                      <Show when={index() > 0}>
                        <Icon name="chevron_right" size="sm"
                          class="df-browse-crumb-sep df-browse-crumb-sep-mirror" />
                      </Show>
                      <Button class="df-browse-crumb" variant="ghost" size="sm"
                        data-mirror-crumb tabIndex={-1}>{crumb.label}</Button>
                    </>
                  )}
                </For>
                <Icon name="chevron_right" size="sm" class="df-browse-crumb-sep" />
                <IconButton class="df-browse-crumb-overflow" size="icon-sm" icon="more_horiz"
                  label="Show hidden folders" data-mirror-overflow tabIndex={-1} />
              </div>
            </>
          }
        >
          <TextField
            class="df-browse-filter"
            value={props.filter}
            onInput={props.onFilter}
            ref={props.setFilterRef}
            placeholder="Filter this folder"
            ariaLabel="Filter this folder"
            testId="browse-filter"
            onKeyDown={(event) => { if (event.key === "Escape") props.onCloseFilter(); }}
          />
          <IconButton size="icon-sm" data-testid="browse-filter-close" icon="close"
            label="Close filter" title="Close filter" onClick={props.onCloseFilter} />
        </Show>
      </Surface>
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
    </>
  );
}

// Browse-page toolbar: cancel (compact only), back/forward, grid⇄list, the
// show-files toggle, New folder, and the server switcher. Split out of
// BrowsePage.tsx so the page keeps state ownership — every value below arrives
// already computed and every button reports back through a callback.
//
// Callers: BrowsePage.tsx (WorkerBrowsePage).

import { For, Show, createSignal } from "solid-js";
import type { Worker } from "@roost/shared/wire";
import { Button } from "./Settings/md/Button.tsx";
import { Icon } from "./Settings/md/Icon.tsx";
import { IconButton } from "./Settings/md/IconButton.tsx";
import { StatusDot } from "./Settings/md/StatusDot.tsx";
import {
  anchoredMenuPosition,
  anchoredMenuSurfaceStyle,
  CtxMenuItem,
  focusMenuEdge,
  handleMenuKeyboardNavigation,
  trackFloatingMenuDismiss,
} from "./contextMenuPrimitives.tsx";
import type { MenuFocusEdge } from "./contextMenuPrimitives.tsx";

export function BrowseToolbar(props: {
  compact: boolean;
  viewMode: "grid" | "list";
  showFiles: boolean;
  backEnabled: boolean;
  forwardEnabled: boolean;
  serverFp: string;
  serverLabel: string;
  serverOnline: boolean;
  onlineWorkers: Worker[];
  serverMenuOpen: boolean;
  setServerMenuOpen: (open: boolean) => void;
  onCancel: () => void;
  onBack: () => void;
  onForward: () => void;
  onViewMode: (mode: "grid" | "list") => void;
  onToggleShowFiles: () => void;
  onNewFolder: () => void;
  onSelectServer: (fp: string) => void;
}) {
  const [serverMenuPosition, setServerMenuPosition] = createSignal<{ right: number; y: number } | null>(null);
  let serverMenuButton: HTMLButtonElement | undefined;
  let serverMenuElement: HTMLDivElement | undefined;
  let cancelPendingFocus: (() => void) | null = null;

  function closeServerMenu(restoreTriggerFocus = false): void {
    if (!props.serverMenuOpen) return;
    cancelPendingFocus?.();
    cancelPendingFocus = null;
    props.setServerMenuOpen(false);
    if (restoreTriggerFocus) queueMicrotask(() => serverMenuButton?.focus());
  }
  function openServerMenu(edge: MenuFocusEdge = "first"): void {
    if (!serverMenuButton) return;
    cancelPendingFocus?.();
    setServerMenuPosition(anchoredMenuPosition(serverMenuButton));
    props.setServerMenuOpen(true);
    cancelPendingFocus = focusMenuEdge(() => serverMenuElement, edge);
  }
  function toggleServerMenu(): void {
    if (props.serverMenuOpen) closeServerMenu();
    else openServerMenu();
  }
  function chooseServer(fp: string): void {
    closeServerMenu();
    props.onSelectServer(fp);
  }
  function onServerTriggerKeyDown(event: KeyboardEvent): void {
    if (event.key === "ArrowDown" || event.key === "ArrowUp") {
      event.preventDefault();
      event.stopPropagation();
      openServerMenu(event.key === "ArrowDown" ? "first" : "last");
    } else if (event.key === "Escape" && props.serverMenuOpen) {
      event.preventDefault();
      event.stopPropagation();
      closeServerMenu();
    }
  }
  function onServerMenuKeyDown(event: KeyboardEvent): void {
    handleMenuKeyboardNavigation(
      event,
      serverMenuElement,
      () => closeServerMenu(true),
      () => closeServerMenu(),
    );
  }

  trackFloatingMenuDismiss({
    within: [() => serverMenuButton, () => serverMenuElement],
    onClose: () => closeServerMenu(),
    onEscape: () => closeServerMenu(true),
  });

  return (
    <div class="df-browse-toolbar">
      <Show when={props.compact}>
        <IconButton class="df-browse-toolbar-icon" data-testid="browse-close" icon="close"
          label="Cancel" title="Cancel" onClick={props.onCancel} />
      </Show>
      <IconButton class="df-browse-toolbar-icon" data-testid="browse-back" icon="arrow_back"
        label="Back" title="Back" disabled={!props.backEnabled} onClick={props.onBack} />
      <IconButton class="df-browse-toolbar-icon" data-testid="browse-forward" icon="arrow_forward"
        label="Forward" title="Forward" disabled={!props.forwardEnabled} onClick={props.onForward} />

      <div class="df-browse-toggle" role="group" aria-label="View mode">
        <IconButton class="df-browse-toggle-btn" data-testid="browse-view-grid" icon="grid_view"
          label="Grid view" title="Grid view" data-active={props.viewMode === "grid" ? "true" : undefined}
          aria-pressed={props.viewMode === "grid"} onClick={() => props.onViewMode("grid")} />
        <IconButton class="df-browse-toggle-btn" data-testid="browse-view-list" icon="view_list"
          label="List view" title="List view" data-active={props.viewMode === "list" ? "true" : undefined}
          aria-pressed={props.viewMode === "list"} onClick={() => props.onViewMode("list")} />
      </div>

      <div class="df-browse-toolbar-actions">
        <IconButton class="df-browse-toggle-btn" data-testid="browse-show-files" icon="description"
          label="Show files in this folder" title="Show files in this folder"
          data-active={props.showFiles ? "true" : undefined} aria-pressed={props.showFiles}
          onClick={props.onToggleShowFiles} />
        <Button class="df-browse-new" variant="secondary" icon="create_new_folder"
          data-testid="browse-new" onClick={props.onNewFolder}>
          <span class="df-browse-new-label">New folder</span>
        </Button>

        <Show when={props.onlineWorkers.length > 1}>
          <Button
            ref={serverMenuButton}
            id="browse-server-trigger"
            class="df-browse-server"
            variant="secondary"
            size="sm"
            data-testid="browse-server"
            title={props.serverLabel}
            aria-haspopup="menu"
            aria-controls="browse-server-menu"
            aria-expanded={props.serverMenuOpen}
            onClick={toggleServerMenu}
            onKeyDown={onServerTriggerKeyDown}
          >
            <StatusDot status={props.serverOnline ? "ok" : "idle"} />
            <span class="df-browse-server-label">{props.serverLabel}</span>
            <Icon name="expand_more" class="df-browse-server-chevron" size="sm" />
          </Button>
          <Show when={serverMenuPosition()}>
            {(position) => (
              <Show when={props.serverMenuOpen}>
                <div
                  ref={serverMenuElement}
                  id="browse-server-menu"
                  class="df-menu-enter df-browse-server-menu"
                  data-testid="browse-server-menu"
                  role="menu"
                  aria-labelledby="browse-server-trigger"
                  style={anchoredMenuSurfaceStyle(position(), {
                    minWidth: "calc(var(--control-touch-target) * 4)",
                  })}
                  onKeyDown={onServerMenuKeyDown}
                >
                  <For each={props.onlineWorkers}>
                    {(worker) => {
                      const selected = String(worker.fp) === props.serverFp;
                      return (
                        <CtxMenuItem
                          class="df-browse-server-option"
                          testid="browse-server-option"
                          selected={selected}
                          title={worker.label}
                          onClick={() => chooseServer(String(worker.fp))}
                        >
                          <StatusDot status="ok" />
                          <span class="df-browse-server-option-label">{worker.label}</span>
                          <Show when={selected}>
                            <Icon name="check" class="df-browse-server-option-check" size="sm" />
                          </Show>
                        </CtxMenuItem>
                      );
                    }}
                  </For>
                </div>
              </Show>
            )}
          </Show>
        </Show>
      </div>
    </div>
  );
}

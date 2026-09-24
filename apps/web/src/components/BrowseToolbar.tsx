// Folder-picker header band: close, the folder being browsed over its machine,
// and the three actions that act on this folder — filter, show files, New
// folder — plus the machine switcher when more than one machine is online.
// One row at every width; the path band below owns navigation. The page keeps
// state ownership: every value arrives computed, every control reports back.
//
// Callers: WorkerBrowsePage.tsx.

import { For, Show, createSignal } from "solid-js";
import type { Worker } from "@roost/protocol/wire";
import { Button } from "./Settings/md/Button.tsx";
import { Icon } from "./Settings/md/Icon.tsx";
import { IconButton } from "./Settings/md/IconButton.tsx";
import { StatusDot } from "./Settings/md/StatusDot.tsx";
import { Surface } from "./Settings/md/Surface.tsx";
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
  /** Basename of the folder being browsed — the picker's own title. */
  folderName: string;
  /** The machine is in scope: mkdir and launch are reachable. */
  ready: boolean;
  showFiles: boolean;
  filterOpen: boolean;
  serverFp: string;
  serverLabel: string;
  serverOnline: boolean;
  onlineWorkers: Worker[];
  serverMenuOpen: boolean;
  setServerMenuOpen: (open: boolean) => void;
  onClose: () => void;
  onToggleFilter: () => void;
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
    <Surface class="df-browse-header" level={2} radius="none">
      <IconButton size="icon-sm" data-testid="browse-close" icon="close"
        label="Close" title="Close" onClick={props.onClose} />
      <div class="df-browse-header-title">
        <span class="df-browse-header-folder md-title-s" data-testid="browse-folder-name">{props.folderName}</span>
        <span class="df-browse-header-machine md-label-s" data-testid="browse-machine">
          <StatusDot status={props.serverOnline ? "ok" : "idle"} />
          {props.serverLabel}
        </span>
      </div>

      <Show when={props.onlineWorkers.length > 1}>
        {/* Icon-only: the header line above already names the machine, and a
            trigger that repeats it makes a screen reader say it twice. */}
        <IconButton
          ref={serverMenuButton}
          id="browse-server-trigger"
          class="df-browse-server"
          size="icon-sm"
          icon="unfold_more"
          label="Switch machine"
          data-testid="browse-server"
          title={props.serverLabel}
          menuPopup="menu"
          controlsId="browse-server-menu"
          expanded={props.serverMenuOpen}
          onClick={toggleServerMenu}
          onKeyDown={onServerTriggerKeyDown}
        />
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
                          <Icon name="check" size="sm" />
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

      <IconButton class="df-browse-toggle" size="icon-sm" data-testid="browse-filter-toggle"
        icon="search" label="Filter this folder" title="Filter this folder"
        data-active={props.filterOpen ? "true" : undefined} aria-pressed={props.filterOpen}
        onClick={props.onToggleFilter} />
      <IconButton class="df-browse-toggle" size="icon-sm" data-testid="browse-show-files"
        icon="description" label="Show files in this folder" title="Show files in this folder"
        data-active={props.showFiles ? "true" : undefined} aria-pressed={props.showFiles}
        onClick={props.onToggleShowFiles} />
      <Button class="df-browse-new" variant="secondary" size="sm" icon="create_new_folder"
        data-testid="browse-new" title="New folder" disabled={!props.ready}
        onClick={props.onNewFolder}>
        New folder
      </Button>
    </Surface>
  );
}

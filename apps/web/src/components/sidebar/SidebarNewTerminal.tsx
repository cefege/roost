// Sidebar-wide pinned action bar: open a new folder plus its machine picker.
// SidebarRoot mounts it in the sidebar grid's bottom row, outside both scrollers.
// Depends on the shared new-terminal target resolver and anchored-menu primitives.

import { useLocation, useNavigate } from "@solidjs/router";
import { createEffect, createMemo, createSignal, For, Show } from "solid-js";
import { diag } from "@roost/shared/diag";
import { browseHref } from "../../routes.ts";
import { defaultNewTerminalWorkerFp, onlineWorkersByLabel } from "../../lib/newTerminalTarget.ts";
import { Button } from "../Settings/md/Button.tsx";
import { Icon } from "../Settings/md/Icon.tsx";
import { StatusDot } from "../Settings/md/StatusDot.tsx";
import {
  anchoredMenuPosition,
  anchoredMenuSurfaceStyle,
  CtxMenuItem,
  focusMenuEdge,
  handleMenuKeyboardNavigation,
  trackFloatingMenuDismiss,
} from "../contextMenuPrimitives.tsx";
import type { MenuFocusEdge } from "../contextMenuPrimitives.tsx";

export function SidebarNewTerminal() {
  const navigate = useNavigate();
  const location = useLocation();
  const [selectedWorkerFp, setSelectedWorkerFp] = createSignal<string | null>(null);
  const [machineMenuOpen, setMachineMenuOpen] = createSignal(false);
  const [machineMenuPosition, setMachineMenuPosition] = createSignal<{ right: number; bottom: number } | null>(null);
  let machineMenuButton: HTMLButtonElement | undefined;
  let machineMenuElement: HTMLDivElement | undefined;
  let menuTargetFp: string | null = null;
  let cancelPendingFocus: (() => void) | null = null;

  const onlineWorkers = createMemo(() => onlineWorkersByLabel());
  const defaultTargetFp = createMemo(() => defaultNewTerminalWorkerFp(location.pathname));
  const effectiveTargetFp = createMemo(() => {
    const selectedFp = selectedWorkerFp();
    if (selectedFp && onlineWorkers().some((worker) => worker.fp === selectedFp)) return selectedFp;
    return defaultTargetFp();
  });
  const effectiveWorkerLabel = createMemo(() => {
    const fp = effectiveTargetFp();
    if (!fp) return "";
    return onlineWorkers().find((worker) => worker.fp === fp)?.label ?? fp.slice(0, 8);
  });

  function closeMachineMenu(restoreTriggerFocus = false): void {
    if (!machineMenuOpen()) return;
    cancelPendingFocus?.();
    cancelPendingFocus = null;
    menuTargetFp = null;
    setMachineMenuOpen(false);
    if (restoreTriggerFocus) queueMicrotask(() => machineMenuButton?.focus());
  }
  function machineMenuSurfaceStyle(position: { right: number; bottom: number }) {
    const style = anchoredMenuSurfaceStyle({ right: position.right, y: position.bottom }, {
      minWidth: "calc(var(--control-touch-target) * 4)",
    });
    delete style.top;
    style.bottom = `${position.bottom}px`;
    return style;
  }
  function openMachineMenu(edge: MenuFocusEdge = "first"): void {
    if (!machineMenuButton) return;
    const targetFp = effectiveTargetFp();
    if (!targetFp) return;
    cancelPendingFocus?.();
    menuTargetFp = targetFp;
    const triggerPosition = anchoredMenuPosition(machineMenuButton);
    const triggerBounds = machineMenuButton.getBoundingClientRect();
    setMachineMenuPosition({
      right: triggerPosition.right,
      bottom: window.innerHeight - triggerBounds.top + (triggerPosition.y - triggerBounds.bottom),
    });
    setMachineMenuOpen(true);
    cancelPendingFocus = focusMenuEdge(() => machineMenuElement, edge);
  }
  function toggleMachineMenu(): void {
    if (machineMenuOpen()) closeMachineMenu();
    else openMachineMenu();
  }
  function chooseMachine(fp: string): void {
    setSelectedWorkerFp(fp);
    diag("sidebar.new_terminal_target_changed", { from: effectiveTargetFp(), to: fp });
    closeMachineMenu(true);
  }
  function onMachineTriggerKeyDown(event: KeyboardEvent): void {
    if (event.key === "ArrowDown" || event.key === "ArrowUp") {
      event.preventDefault();
      event.stopPropagation();
      openMachineMenu(event.key === "ArrowDown" ? "first" : "last");
    } else if (event.key === "Escape" && machineMenuOpen()) {
      event.preventDefault();
      event.stopPropagation();
      closeMachineMenu();
    }
  }
  function onMachineMenuKeyDown(event: KeyboardEvent): void {
    handleMenuKeyboardNavigation(
      event,
      machineMenuElement,
      () => closeMachineMenu(true),
      () => closeMachineMenu(),
    );
  }

  createEffect(() => {
    const workers = onlineWorkers();
    const selectedFp = selectedWorkerFp();
    if (selectedFp && !workers.some((worker) => worker.fp === selectedFp)) setSelectedWorkerFp(null);
    if (
      workers.length < 2
      || (machineMenuOpen() && !workers.some((worker) => worker.fp === menuTargetFp))
    ) closeMachineMenu();
  });
  trackFloatingMenuDismiss({
    within: [() => machineMenuButton, () => machineMenuElement],
    onClose: () => closeMachineMenu(true),
    onEscape: () => closeMachineMenu(true),
  });

  return (
    <footer class="workbench-sidebar-actionbar" data-testid="sidebar-new-terminal">
      <Button
        class="workbench-sidebar-actionbar__new"
        variant="default"
        size="sm"
        icon="add"
        data-testid="sidebar-new-terminal-button"
        disabled={!effectiveTargetFp()}
        onClick={() => {
          const fp = effectiveTargetFp();
          if (fp) navigate(browseHref(fp));
        }}
      >
        New folder
      </Button>
      <Show when={onlineWorkers().length > 1}>
        <Button
          ref={machineMenuButton}
          id="sidebar-new-terminal-machine"
          class="workbench-sidebar-actionbar__machine"
          variant="ghost"
          size="sm"
          data-testid="sidebar-new-terminal-machine"
          title={effectiveWorkerLabel()}
          aria-haspopup="menu"
          aria-controls="sidebar-new-terminal-machine-menu"
          aria-expanded={machineMenuOpen()}
          onClick={toggleMachineMenu}
          onKeyDown={onMachineTriggerKeyDown}
        >
          <StatusDot status="ok" />
          <span class="workbench-sidebar-actionbar__machine-label">{effectiveWorkerLabel()}</span>
          <Icon name="expand_more" class="workbench-sidebar-actionbar__machine-chevron" size="sm" />
        </Button>
      </Show>
      <Show when={machineMenuPosition()}>
        {(position) => (
          <Show when={machineMenuOpen()}>
            <div
              ref={machineMenuElement}
              id="sidebar-new-terminal-machine-menu"
              class="df-menu-enter workbench-sidebar-actionbar__machine-menu"
              data-testid="sidebar-new-terminal-machine-menu"
              role="menu"
              aria-labelledby="sidebar-new-terminal-machine"
              style={machineMenuSurfaceStyle(position())}
              onKeyDown={onMachineMenuKeyDown}
            >
              <For each={onlineWorkers()}>
                {(worker) => (
                  <CtxMenuItem
                    class="workbench-sidebar-actionbar__machine-option"
                    testid="sidebar-new-terminal-machine-option"
                    selected={worker.fp === effectiveTargetFp()}
                    title={worker.label}
                    onClick={() => chooseMachine(worker.fp)}
                  >
                    <StatusDot status="ok" />
                    <span class="workbench-sidebar-actionbar__machine-option-label">{worker.label}</span>
                    <Show when={worker.fp === effectiveTargetFp()}>
                      <Icon name="check" class="workbench-sidebar-actionbar__machine-option-check" size="sm" />
                    </Show>
                  </CtxMenuItem>
                )}
              </For>
            </div>
          </Show>
        )}
      </Show>
    </footer>
  );
}

// Binds deck-wide keyboard shortcuts to the same operations used by pointer UI.
// It resolves pane movement from painted geometry and releases stale composer
// focus when the keyboard-owning tab changes. Terminal input keeps every chord
// not reserved by the browser-platform shortcut map.

import {
  createEffect,
  on,
  onCleanup,
  onMount,
  type Accessor,
} from "solid-js";
import type { Layout, PaneDir, PaneView } from "../store/paneLayout.ts";
import {
  activeComposeSessionId,
  releaseActiveComposeFocus,
} from "./TerminalComposeButton.tsx";
import {
  browserPlatform,
  matchesPlatformShortcut,
  type PlatformShortcutId,
} from "../lib/browserPlatform.ts";
import {
  clearSpotlight,
  spotlightSessionId,
} from "../store/spotlight.ts";
import type { ArrangeKind } from "../store/paneLayoutPresets.ts";
import type { TerminalDeckProps } from "./terminal-deck-model.ts";

const ARROW_PANE_DIR: Readonly<Record<string, PaneMoveDirection | undefined>> = {
  ArrowLeft: "left",
  ArrowRight: "right",
  ArrowUp: "up",
  ArrowDown: "down",
};
const ARRANGE_SHORTCUTS: ReadonlyArray<readonly [PlatformShortcutId, ArrangeKind]> = [
  ["arrangeBalance", "balance"],
  ["arrangeColumns", "even"],
  ["arrangeRows", "rows"],
  ["arrangeGrid", "tiled"],
  ["arrangeMain", "main-vertical"],
];
type PaneMoveDirection = "left" | "right" | "up" | "down";

interface ShortcutDeckModel {
  folderKey: Accessor<string | null>;
  layout: Accessor<Layout | null>;
  view: Accessor<{ panes: PaneView[] }>;
}

interface ShortcutDeckActions {
  arrange(kind: ArrangeKind): void;
  focusPane(paneId: string): void;
  newTab(paneId: string): Promise<void>;
  select(id: string): void;
  split(dir: PaneDir): Promise<void>;
  spotlight(): void;
}

export function bindTerminalDeckShortcuts(
  props: TerminalDeckProps,
  model: ShortcutDeckModel,
  actions: ShortcutDeckActions,
): void {
  const focusedPaneView = (): PaneView | null => {
    const panes = model.view().panes;
    const focusedId = model.layout()?.focusedPaneId;
    return panes.find((pane) => pane.paneId === focusedId)
      ?? panes.find((pane) => pane.focused)
      ?? null;
  };
  createEffect(on(
    () => focusedPaneView()?.selectedTab ?? null,
    (selectedId) => {
      const ownerId = activeComposeSessionId();
      if (ownerId && ownerId !== selectedId) releaseActiveComposeFocus();
    },
    { defer: true },
  ));
  const activateTabAt = (digit: number): void => {
    const pane = focusedPaneView();
    if (!pane) return;
    const id = digit === 9
      ? pane.tabIds[pane.tabIds.length - 1]
      : pane.tabIds[digit - 1];
    if (id) actions.select(id);
  };
  const focusAdjacentPane = (direction: PaneMoveDirection): void => {
    const source = focusedPaneView();
    if (!source) return;
    const centerX = source.rect.x + source.rect.w / 2;
    const centerY = source.rect.y + source.rect.h / 2;
    let bestId: string | null = null;
    let bestDistance = Infinity;
    for (const pane of model.view().panes) {
      if (pane.paneId === source.paneId) continue;
      const deltaX = pane.rect.x + pane.rect.w / 2 - centerX;
      const deltaY = pane.rect.y + pane.rect.h / 2 - centerY;
      const ahead = direction === "left" ? deltaX < 0
        : direction === "right" ? deltaX > 0
          : direction === "up" ? deltaY < 0
            : deltaY > 0;
      if (!ahead) continue;
      const distance = deltaX * deltaX + deltaY * deltaY;
      if (distance < bestDistance) {
        bestDistance = distance;
        bestId = pane.paneId;
      }
    }
    if (bestId) {
      releaseActiveComposeFocus();
      actions.focusPane(bestId);
    }
  };

  onMount(() => {
    const onKey = (event: KeyboardEvent): void => {
      if (event.defaultPrevented) return;
      const consume = (): void => {
        event.preventDefault();
        event.stopPropagation();
      };
      if (event.key === "Escape" && spotlightSessionId()) {
        consume();
        clearSpotlight();
        return;
      }
      if (!props.surfaceVisible || !model.folderKey()) return;
      const platform = browserPlatform();
      const direction = ARROW_PANE_DIR[event.key];
      if (direction && matchesPlatformShortcut(event, "paneFocus", platform)) {
        consume();
        focusAdjacentPane(direction);
        return;
      }
      if (matchesPlatformShortcut(event, "newTerminal", platform)) {
        consume();
        void actions.newTab(model.layout()?.focusedPaneId ?? "");
        return;
      }
      if (matchesPlatformShortcut(event, "terminalTab", platform)) {
        consume();
        activateTabAt(Number(event.key));
        return;
      }
      if (matchesPlatformShortcut(event, "splitRight", platform)) {
        consume();
        void actions.split("row");
        return;
      }
      if (matchesPlatformShortcut(event, "splitDown", platform)) {
        consume();
        void actions.split("col");
        return;
      }
      if (matchesPlatformShortcut(event, "spotlight", platform)) {
        consume();
        actions.spotlight();
        return;
      }
      for (const [shortcut, preset] of ARRANGE_SHORTCUTS) {
        if (!matchesPlatformShortcut(event, shortcut, platform)) continue;
        consume();
        actions.arrange(preset);
        return;
      }
    };
    document.addEventListener("keydown", onKey, true);
    onCleanup(() => document.removeEventListener("keydown", onKey, true));
  });
}

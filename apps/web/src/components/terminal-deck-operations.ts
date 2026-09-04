// Owns terminal deck commands: selection, spawning, splitting, arranging, and
// drag-to-tile commits. Keyboard and pane-pointer entry points call these same
// operations so every interaction preserves one layout and navigation path.
// The deck model supplies reactive geometry without exposing its implementation.

import type { Accessor, Setter } from "solid-js";
import type { Session } from "@roost/shared/wire";
import { diag } from "@roost/shared/diag";
import { rootStore } from "../store/root.ts";
import { releaseActiveComposeFocus } from "./TerminalComposeButton.tsx";
import {
  spawnShellDetailed,
  spawnInWorkspaceDetailed,
  waitForSession,
  maybeAutoLaunchAgent,
  type SpawnInitialViewport,
  type SpawnShellResult,
} from "../lib/spawnSession.ts";
import {
  beginOptimisticSpawn,
  clearAborted,
  endOptimisticSpawn,
  failOptimisticSpawn,
  waitForMountedSpawnMeasurement,
  wasAborted,
} from "../store/optimisticSpawn.ts";
import {
  captureDashboardResourceToken,
  isCurrentDashboardResourceToken,
} from "../store/dashboard-selection.ts";
import { coordClient } from "../connect.ts";
import {
  findLeafOfTab,
  focusPane,
  moveTab,
  reorderTab,
  selectTab,
  setRatio,
  splitLeaf,
  type Layout,
  type PaneDir,
  type PaneView,
  type Rect,
} from "../store/paneLayout.ts";
import {
  clearSpotlight,
  setSpotlightSessionId,
  spotlightSessionId,
} from "../store/spotlight.ts";
import {
  tileTargetFor,
  zoneRect,
  zoneToSplit,
  type DropZone,
  type TileTarget,
} from "../lib/dropZones.ts";
import {
  closeSessionOp,
  focusPaneOp,
  type DeckOpsCtx,
} from "../lib/deckOps.ts";
import { pulseArrange } from "../lib/resizeDrag.ts";
import { arrangeLayout, type ArrangeKind } from "../store/paneLayoutPresets.ts";
import { isCompact } from "../lib/windowSizeClass.ts";
import {
  TERMINAL_STRIP_HEIGHT,
} from "./terminal-deck-geometry.ts";
import type { TerminalDeckProps } from "./terminal-deck-model.ts";
import { bindTerminalDeckShortcuts } from "./terminal-deck-shortcuts.ts";
import { folderPathOf } from "../lib/folderKey.ts";


interface DeckOperationModel {
  activeSession: Accessor<Session | null>;
  apply: (transform: (current: Layout) => Layout) => void;
  folderKey: Accessor<string | null>;
  layout: Accessor<Layout | null>;
  liveIds: Accessor<string[]>;
  navigate: (href: string) => void;
  opsCtx: DeckOpsCtx;
  selectSession: (id: string) => void;
  setDragRatios: Setter<Record<string, number>>;
  setDropOverlay: Setter<{ rect: Rect; zone: DropZone } | null>;
  size: Accessor<{ w: number; h: number }>;
  spotlightPane: Accessor<PaneView | null>;
  view: Accessor<{ panes: PaneView[] }>;
}

export interface TerminalDeckOperations {
  arrange(kind: ArrangeKind): void;
  close(session: Session): void;
  dividerCommit(splitId: string, ratio: number): void;
  dividerDrag(splitId: string, ratio: number): void;
  focusPane(paneId: string): void;
  newTab(paneId: string): Promise<void>;
  onDeckFocusIn(event: FocusEvent): void;
  onDeckPointerDown(event: PointerEvent): void;
  onTabDragEnd(): void;
  onTabDragMove(originPaneId: string, clientX: number, clientY: number): void;
  onTabTileDrop(tabId: string, originPaneId: string, clientX: number, clientY: number): boolean;
  reorder(paneId: string, ids: string[]): void;
  select(id: string): void;
  split(dir: PaneDir): Promise<void>;
  spotlight(): void;
}

export function createTerminalDeckOperations(
  props: TerminalDeckProps,
  model: DeckOperationModel,
  getDeckElement: () => HTMLDivElement | undefined,
): TerminalDeckOperations {
  function focusDeckPane(paneId: string): void {
    focusPaneOp(model.opsCtx, paneId);
  }
  function closeSession(session: Session): void {
    closeSessionOp(model.opsCtx, session);
  }
  function anchorFor(paneId: string): Session | null {
    const pane = model.view().panes.find((candidate) => candidate.paneId === paneId);
    return (pane && rootStore.sessions[pane.selectedTab]) || model.activeSession();
  }
  async function spawnSibling(
    anchor: Session,
    sessionId?: string,
    initialViewport?: SpawnInitialViewport,
  ): Promise<SpawnShellResult> {
    const folder = folderPathOf(anchor);
    return anchor.workspace_id
      ? await spawnInWorkspaceDetailed(
        anchor.worker_fp,
        anchor.workspace_id,
        folder,
        sessionId,
        initialViewport,
      )
      : await spawnShellDetailed(
        anchor.worker_fp,
        folder,
        sessionId,
        initialViewport,
      );
  }
  async function newTab(paneId: string): Promise<void> {
    const anchor = anchorFor(paneId);
    if (!anchor) return;
    const dashboardToken = captureDashboardResourceToken();
    releaseActiveComposeFocus();
    model.apply((layout) => focusPane(layout, paneId));
    const sessionId = beginOptimisticSpawn(anchor);
    model.navigate(`/s/${sessionId}`);
    const measured = await waitForMountedSpawnMeasurement(sessionId, 100);
    if (!isCurrentDashboardResourceToken(dashboardToken)) return;
    if (wasAborted(sessionId)) {
      clearAborted(sessionId);
      return;
    }
    const startedAt = Date.now();
    try {
      await spawnSibling(anchor, sessionId, measured ?? undefined);
      if (!isCurrentDashboardResourceToken(dashboardToken)) {
        endOptimisticSpawn(sessionId);
        return;
      }
      diag("spawn.optimistic", {
        session_id: sessionId,
        rtt_ms: Date.now() - startedAt,
      });
      if (wasAborted(sessionId)) {
        clearAborted(sessionId);
        void coordClient.sessionsKill({ sessionId });
        return;
      }
      endOptimisticSpawn(sessionId);
      maybeAutoLaunchAgent(sessionId);
    } catch (error) {
      if (!isCurrentDashboardResourceToken(dashboardToken)) return;
      if ((rootStore.sessions[sessionId]?.channel ?? 0) > 0) {
        endOptimisticSpawn(sessionId);
        maybeAutoLaunchAgent(sessionId);
        return;
      }
      failOptimisticSpawn(sessionId, error);
    }
  }
  async function split(dir: PaneDir): Promise<void> {
    const layout = model.layout();
    if (!layout) return;
    const paneId = layout.focusedPaneId;
    const anchor = anchorFor(paneId);
    if (!anchor) return;
    const dashboardToken = captureDashboardResourceToken();
    releaseActiveComposeFocus();
    const { sessionId } = await spawnSibling(anchor);
    if (!isCurrentDashboardResourceToken(dashboardToken)) return;
    await waitForSession(sessionId);
    if (!isCurrentDashboardResourceToken(dashboardToken)) return;
    maybeAutoLaunchAgent(sessionId);
    model.apply((current) => splitLeaf(current, paneId, dir, sessionId, false));
    model.navigate(`/s/${sessionId}`);
  }
  function spotlight(): void {
    if (spotlightSessionId()) {
      clearSpotlight();
      return;
    }
    const pane = model.view().panes.find(
      (candidate) => candidate.paneId === model.layout()?.focusedPaneId,
    );
    if (pane?.selectedTab) setSpotlightSessionId(pane.selectedTab);
  }
  function arrange(kind: ArrangeKind): void {
    const currentFolder = model.folderKey();
    const layout = model.layout();
    if (!currentFolder || !layout) return;
    clearSpotlight();
    let next = arrangeLayout(kind, layout, model.liveIds());
    const active = props.activeSessionId;
    if (kind !== "balance" && active && findLeafOfTab(next.root, active)) {
      next = selectTab(next, active);
    }
    model.apply(() => next);
    pulseArrange();
  }
  function dividerDrag(splitId: string, ratio: number): void {
    model.setDragRatios((previous) => ({ ...previous, [splitId]: ratio }));
  }
  function dividerCommit(splitId: string, ratio: number): void {
    model.apply((layout) => ({ ...layout, root: setRatio(layout.root, splitId, ratio) }));
    model.setDragRatios((previous) => {
      const next = { ...previous };
      delete next[splitId];
      return next;
    });
  }
  function deckLocal(clientX: number, clientY: number): { x: number; y: number } {
    const rect = getDeckElement()?.getBoundingClientRect();
    return { x: clientX - (rect?.left ?? 0), y: clientY - (rect?.top ?? 0) };
  }
  function tileTarget(
    originPaneId: string,
    clientX: number,
    clientY: number,
  ): TileTarget {
    const point = deckLocal(clientX, clientY);
    return tileTargetFor(
      model.view().panes,
      originPaneId,
      point.x,
      point.y,
      TERMINAL_STRIP_HEIGHT,
    );
  }
  function onTabDragMove(originPaneId: string, clientX: number, clientY: number): void {
    const target = tileTarget(originPaneId, clientX, clientY);
    model.setDropOverlay(target
      ? { rect: zoneRect(target.rect, target.zone), zone: target.zone }
      : null);
  }
  function onTabTileDrop(
    tabId: string,
    originPaneId: string,
    clientX: number,
    clientY: number,
  ): boolean {
    model.setDropOverlay(null);
    const target = tileTarget(originPaneId, clientX, clientY);
    if (!target || target.zone === "reorder") return false;
    const splitTarget = zoneToSplit(target.zone);
    if (splitTarget) {
      model.apply((layout) => splitLeaf(
        layout,
        target.paneId,
        splitTarget.dir,
        tabId,
        splitTarget.insertFirst,
      ));
    } else {
      model.apply((layout) => moveTab(layout, tabId, target.paneId));
    }
    model.navigate(`/s/${tabId}`);
    return true;
  }
  function onDeckPointerDown(event: PointerEvent): void {
    const target = event.target as HTMLElement | null;
    if (target?.closest("[data-pane-strip]")) return;
    const paneElement = target?.closest<HTMLElement>("[data-pane-id]");
    const paneId = paneElement?.getAttribute("data-pane-id");
    if (paneId) focusDeckPane(paneId);
    if (event.button === 1 && paneId && !isCompact() && !target?.closest("a")) {
      event.preventDefault();
      spotlight();
    }
  }
  function onDeckFocusIn(event: FocusEvent): void {
    const target = event.target;
    if (!(target instanceof Element)
      || !target.closest('[data-testid="mobile-chat-input"]')) return;
    const pane = target.closest<HTMLElement>("[data-pane-id]");
    const paneId = pane?.getAttribute("data-pane-id");
    if (paneId && paneId !== model.layout()?.focusedPaneId) focusDeckPane(paneId);
  }


  bindTerminalDeckShortcuts(props, model, {
    arrange,
    focusPane: focusDeckPane,
    newTab,
    select: model.selectSession,
    split,
    spotlight,
  });

  return {
    arrange,
    close: closeSession,
    dividerCommit,
    dividerDrag,
    focusPane: focusDeckPane,
    newTab,
    onDeckFocusIn,
    onDeckPointerDown,
    onTabDragEnd: () => model.setDropOverlay(null),
    onTabDragMove,
    onTabTileDrop,
    reorder: (paneId, ids) => model.apply((layout) => reorderTab(layout, paneId, ids)),
    select: model.selectSession,
    split,
    spotlight,
  };
}

// Owns the terminal deck's reactive folder, layout, geometry, and warm-set model.
// TerminalDeck renders these stable memos while operation and gesture modules
// mutate them through the returned layout boundary. Keeping geometry here makes
// parked terminal sizing and keyed renderer retention one explicit invariant.

import {
  createEffect,
  createMemo,
  createSignal,
  on,
  onCleanup,
  onMount,
  type Accessor,
  type Setter,
} from "solid-js";
import { useNavigate } from "@solidjs/router";
import type { Session } from "@roost/shared/wire";
import { rootStore } from "../store/root.ts";
import {
  activeComposeSessionId,
  releaseActiveComposeFocus,
} from "./TerminalComposeButton.tsx";
import { isCompact } from "../lib/windowSizeClass.ts";
import { folderKeyOf, folderPathOf } from "../lib/folderKey.ts";
import { isPendingClose } from "../lib/pendingClose.ts";
import { shortCwd } from "../lib/sidebarFormat.ts";
import { commitLayout, seedIfAbsent, resolveLayout } from "../store/paneLayoutStore.ts";
import {
  allLeaves,
  findLeafOfTab,
  flatTabs,
  layoutView,
  selectTab,
  setRatio,
  type Layout,
  type PaneView,
  type Rect,
} from "../store/paneLayout.ts";
import {
  clearSpotlight,
  setVisiblePaneCount,
  spotlightSessionId,
} from "../store/spotlight.ts";
import type { DropZone } from "../lib/dropZones.ts";
import { selectTabOp, type DeckOpsCtx } from "../lib/deckOps.ts";
import { nextWarmSessionIds } from "../lib/deckWarmSet.ts";
import {
  barNeighborId as barNeighborIdOf,
  type Swipe,
} from "../lib/deckSwipe.ts";
import {
  MOBILE_TERMINAL_STRIP_HEIGHT,
  TERMINAL_STRIP_HEIGHT,
  sameTerminalPaneView,
  sameTerminalParkSizes,
  terminalSessionStyle,
  type TerminalSessionSlot,
} from "./terminal-deck-geometry.ts";

export interface TerminalDeckProps {
  activeSessionId: string | null;
  surfaceVisible: boolean;
}


export function createTerminalDeckModel(
  props: TerminalDeckProps,
  swipe: Accessor<Swipe | null>,
  setSwipe: Setter<Swipe | null>,
  getDeckElement: () => HTMLDivElement | undefined,
) {
  const navigate = useNavigate();
  const openSessions = createMemo(() =>
    Object.values(rootStore.sessions).filter((session) => session.status === "open"));
  const [warmSessionIds, setWarmSessionIds] = createSignal<ReadonlySet<string>>(new Set());
  const [retainedSessionId, setRetainedSessionId] = createSignal<string | null>(
    props.activeSessionId,
  );
  createEffect(() => {
    if (props.activeSessionId) setRetainedSessionId(props.activeSessionId);
  });
  const deckSessionId = createMemo(() =>
    props.activeSessionId ?? (!props.surfaceVisible ? retainedSessionId() : null));
  const activeSession = createMemo(() => {
    const sessionId = deckSessionId();
    return sessionId ? rootStore.sessions[sessionId] ?? null : null;
  });
  const newTermFolder = createMemo(() => {
    const session = activeSession();
    return session ? shortCwd(folderPathOf(session), session.worker_fp) : "";
  });
  const folderKey = createMemo(() => {
    const session = activeSession();
    return session ? folderKeyOf(session) : null;
  });
  const liveIds = createMemo(() => {
    const currentFolder = folderKey();
    if (!currentFolder) return [];
    return openSessions()
      .filter((session) => folderKeyOf(session) === currentFolder && !isPendingClose(session.id))
      .sort((left, right) => left.created_at - right.created_at)
      .map((session) => session.id);
  });

  const [size, setSize] = createSignal({ w: 0, h: 0 });
  onMount(() => {
    const observer = new ResizeObserver(() => {
      const deckElement = getDeckElement();
      if (deckElement) setSize({ w: deckElement.clientWidth, h: deckElement.clientHeight });
    });
    const deckElement = getDeckElement();
    if (deckElement) {
      observer.observe(deckElement);
      setSize({ w: deckElement.clientWidth, h: deckElement.clientHeight });
    }
    onCleanup(() => observer.disconnect());
  });

  createEffect(() => {
    const currentFolder = folderKey();
    if (currentFolder) seedIfAbsent(currentFolder, liveIds());
  });
  createEffect(on(
    () => [deckSessionId(), folderKey(), liveIds().join("\u0000")] as const,
    ([active, currentFolder]) => {
      if (!active || !currentFolder) return;
      const current = resolveLayout(currentFolder, liveIds());
      const leaf = findLeafOfTab(current.root, active);
      if (!leaf) return;
      if (current.focusedPaneId === leaf.paneId && leaf.selectedTab === active) return;
      commitLayout(currentFolder, selectTab(current, active));
    },
  ));
  createEffect(on(deckSessionId, (active) => {
    if (!active) return;
    const currentSwipe = swipe();
    if (currentSwipe?.phase === "track" && active !== currentSwipe.currentId) setSwipe(null);
  }));

  const [dragRatios, setDragRatios] = createSignal<Record<string, number>>({});
  const [dropOverlay, setDropOverlay] = createSignal<{ rect: Rect; zone: DropZone } | null>(null);
  const layout = createMemo<Layout | null>(() => {
    const currentFolder = folderKey();
    return currentFolder ? resolveLayout(currentFolder, liveIds()) : null;
  });
  const view = createMemo<{
    panes: PaneView[];
    dividers: ReturnType<typeof layoutView>["dividers"];
  }>(() => {
    let current = layout();
    if (!current) return { panes: [], dividers: [] };
    if (size().w === 0 || size().h === 0) return { panes: [], dividers: [] };
    const transientRatios = dragRatios();
    for (const id in transientRatios) {
      current = { ...current, root: setRatio(current.root, id, transientRatios[id]) };
    }
    if (isCompact()) {
      const leaves = allLeaves(current.root);
      const leaf = leaves.find((candidate) => candidate.paneId === current!.focusedPaneId) ?? leaves[0];
      if (!leaf) return { panes: [], dividers: [] };
      return {
        panes: [{
          paneId: leaf.paneId,
          rect: { x: 0, y: 0, w: size().w, h: size().h },
          tabIds: leaf.tabs,
          selectedTab: leaf.selectedTab,
          focused: true,
        }],
        dividers: [],
      };
    }
    return layoutView(current, size().w, size().h);
  });
  const mobileTabs = createMemo<Session[]>(() => {
    if (!isCompact()) return [];
    const current = layout();
    if (!current) return [];
    return flatTabs(current.root)
      .map((tab) => rootStore.sessions[tab.tabId])
      .filter(Boolean) as Session[];
  });
  const spotlightPane = createMemo(() => {
    const sessionId = spotlightSessionId();
    if (!sessionId || isCompact()) return null;
    return view().panes.find((pane) => pane.selectedTab === sessionId) ?? null;
  });
  const spotlightRect = createMemo<Rect | null>(() => {
    if (!spotlightPane()) return null;
    const deckSize = size();
    if (!deckSize.w || !deckSize.h) return null;
    const marginX = Math.max(deckSize.w * 0.06, 24);
    const marginY = Math.max(deckSize.h * 0.06, 24);
    return {
      x: marginX,
      y: marginY,
      w: deckSize.w - 2 * marginX,
      h: deckSize.h - 2 * marginY,
    };
  });
  const slotBySession = createMemo(() => {
    const slots = new Map<string, TerminalSessionSlot>();
    for (const pane of view().panes) {
      if (pane.selectedTab) {
        slots.set(pane.selectedTab, {
          rect: pane.rect,
          paneId: pane.paneId,
          focused: pane.focused,
        });
      }
    }
    const spotlitPane = spotlightPane();
    const spotlitRect = spotlightRect();
    if (spotlitPane?.selectedTab && spotlitRect) {
      slots.set(spotlitPane.selectedTab, {
        rect: spotlitRect,
        paneId: spotlitPane.paneId,
        focused: true,
        spotlit: true,
      });
    }
    const currentSwipe = swipe();
    if (currentSwipe?.neighborId && isCompact()) {
      const currentPane = view().panes[0];
      if (currentPane) {
        slots.set(currentSwipe.neighborId, {
          rect: currentPane.rect,
          paneId: currentPane.paneId,
          focused: false,
        });
      }
    }
    return slots;
  });

  createEffect(() => {
    const openIds = new Set(openSessions().map((session) => session.id));
    setWarmSessionIds((previous) =>
      nextWarmSessionIds(previous, openIds, [...slotBySession().keys()]));
  });
  const mountedSessionIds = createMemo(() => {
    const warmIds = warmSessionIds();
    const selectedIds = slotBySession();
    return openSessions()
      .filter((session) => warmIds.has(session.id) || selectedIds.has(session.id))
      .map((session) => session.id);
  });

  let panesCache = new Map<string, PaneView>();
  const panes = createMemo(() => {
    const next = new Map<string, PaneView>();
    const stablePanes = view().panes.map((pane) => {
      const previous = panesCache.get(pane.paneId);
      const stable = previous && sameTerminalPaneView(previous, pane) ? previous : pane;
      next.set(pane.paneId, stable);
      return stable;
    });
    panesCache = next;
    return stablePanes;
  });
  const paneRectById = createMemo(() => {
    const rects = new Map<string, Rect>();
    for (const pane of view().panes) rects.set(pane.paneId, pane.rect);
    return rects;
  });
  const paneFocusById = createMemo(() => {
    const focus = new Map<string, boolean>();
    for (const pane of view().panes) focus.set(pane.paneId, pane.focused);
    return focus;
  });

  createEffect(() => setVisiblePaneCount(view().panes.length));
  createEffect(() => {
    const sessionId = spotlightSessionId();
    if (sessionId && !openSessions().some((session) => session.id === sessionId)) clearSpotlight();
  });
  createEffect(on(folderKey, () => {
    clearSpotlight();
    setSwipe(null);
  }, { defer: true }));

  const stripH = () => isCompact()
    ? MOBILE_TERMINAL_STRIP_HEIGHT
    : TERMINAL_STRIP_HEIGHT;
  const parkSizeBySession = createMemo(
    () => {
      const sizes = new Map<string, { w: number; h: number }>();
      const stripHeight = stripH();
      for (const pane of view().panes) {
        const terminalSize = {
          w: pane.rect.w,
          h: Math.max(0, pane.rect.h - stripHeight),
        };
        for (const id of pane.tabIds) sizes.set(id, terminalSize);
      }
      return sizes;
    },
    undefined,
    { equals: sameTerminalParkSizes },
  );
  function termStyle(
    slot: TerminalSessionSlot | null,
    park?: { w: number; h: number },
  ): Record<string, string> {
    return terminalSessionStyle(slot, park, size(), stripH());
  }

  const barNeighborId = createMemo<string | null>(() =>
    barNeighborIdOf(swipe(), isCompact()));
  function apply(transform: (current: Layout) => Layout): void {
    const currentFolder = folderKey();
    const current = layout();
    if (currentFolder && current) commitLayout(currentFolder, transform(current));
  }
  const opsCtx: DeckOpsCtx = {
    folderKey,
    layout,
    activeSessionId: () => props.activeSessionId,
    navigate,
  };
  function selectSession(id: string): void {
    if (activeComposeSessionId() !== id) releaseActiveComposeFocus();
    selectTabOp(opsCtx, id, spotlightPane()?.paneId ?? null);
  }

  return {
    activeSession,
    apply,
    barNeighborId,
    dragRatios,
    dropOverlay,
    folderKey,
    layout,
    liveIds,
    mobileTabs,
    mountedSessionIds,
    navigate,
    newTermFolder,
    openSessions,
    opsCtx,
    paneFocusById,
    paneRectById,
    panes,
    parkSizeBySession,
    selectSession,
    setDragRatios,
    setDropOverlay,
    size,
    slotBySession,
    spotlightPane,
    spotlightRect,
    stripH,
    termStyle,
    view,
  };
}

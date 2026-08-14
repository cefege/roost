// Layout-driven terminal deck. Open terminals retain their immutable spawn cwd,
// share selection/close/reorder/split behavior, and stay mounted while parked
// so switching tabs never loses live state.
// Compact/mobile collapses to the focused pane's selected tab.

import { For, Index, Show, createMemo, createSignal, createEffect, onMount, onCleanup, on } from "solid-js";
import { useNavigate } from "@solidjs/router";
import { rootStore } from "../store/root.ts";
import { CellTerminal } from "./CellTerminal.tsx";
import { activeComposeSessionId, releaseActiveComposeFocus } from "./TerminalComposeButton.tsx";
import { PaneStrip } from "./PaneStrip.tsx";
import { MobileDeckBar } from "./MobileDeckBar.tsx";
import { PaneDivider } from "./PaneDivider.tsx";
import { isCompact } from "../lib/windowSizeClass.ts";
import { isResizeDragging, pulseArrange } from "../lib/resizeDrag.ts";
import { folderKeyOf, folderPathOf } from "../lib/folderKey.ts";
import { isPendingClose } from "../lib/pendingClose.ts";
import { shortCwd } from "../lib/sidebarFormat.ts";
import { spawnShell, spawnInWorkspace, waitForSession, maybeAutoLaunchAgent } from "../lib/spawnSession.ts";
import {
  beginOptimisticSpawn, endOptimisticSpawn, failOptimisticSpawn,
  wasAborted, clearAborted,
} from "../store/optimisticSpawn.ts";
import { coordClient } from "../connect.ts";
import { commitLayout, seedIfAbsent, resolveLayout } from "../store/paneLayoutStore.ts";
import {
  layoutView, setRatio, selectTab, focusPane, reorderTab, splitLeaf, moveTab,
  findLeafOfTab, allLeaves, flatTabs, type Layout, type PaneDir, type PaneView, type Rect,
} from "../store/paneLayout.ts";
import { spotlightSessionId, clearSpotlight, setSpotlightSessionId, setVisiblePaneCount } from "../store/spotlight.ts";
import { zoneToSplit, zoneRect, tileTargetFor, type DropZone, type TileTarget } from "../lib/dropZones.ts";
import { selectTabOp, focusPaneOp, closeSessionOp, type DeckOpsCtx } from "../lib/deckOps.ts";
import { shouldCommitSwitch, settleDurationMs, endMode, newFabProgress, peekCard, newFabScale, NEW_FAB_MIN_SCALE, type SwipeMode } from "../lib/deckSwipe.ts";
import { EDGE_PX, lockAxis, openOffsetPx } from "../lib/edgeSwipeDrawer.ts";
import { dragDrawer, settleDrawerOpen } from "../lib/drawerDrag.ts";
import { arrangeLayout, type ArrangeKind } from "../store/paneLayoutPresets.ts";
import { ArrangeMenu } from "./ArrangeMenu.tsx";
import type { Session } from "@roost/shared/wire";
import { diag } from "@roost/shared/diag";
import { prefersReducedMotion } from "../lib/prefersReducedMotion.ts";

const STRIP_H = 40; // per-pane tab strip height (px)

/** Mobile (compact) deck-level bar height (px) — the Chrome-style workspace
 *  bar ([menu][title][+][count]) rendered above the full-bleed terminal. */
const MOBILE_STRIP_H = 48;
// Emphasized-decelerate easing for the swipe settle (shared by slot transform +
// end-affordance placeholder). Mirrors the M3 token used across the mobile deck.
const SWIPE_DECEL = "var(--md-sys-motion-easing-emphasized-decelerate, cubic-bezier(0.05, 0.7, 0.1, 1))";
// Tab-switch slide settle — Chromium ToolbarSwipeLayout animates the offset with
// Android's DecelerateInterpolator(1.0) = 1-(1-t)² (easeOutQuad). Gentler than the
// M3 emphasized-decelerate SWIPE_DECEL, which front-loads ~70% of travel into the
// first ~15% of time and hid the swipe direction. Slide only; affordances keep SWIPE_DECEL.
const SWIPE_SLIDE_EASE = "cubic-bezier(0.25, 0.46, 0.45, 0.94)";
const NEW_BLOOM_MS = 300;       // container-transform reveal (M3 emphasized medium2)
/** ⌘/Ctrl+⌥+arrow → geometric pane walk. Arrows are deliberately disjoint from
 *  the ⌘⌥B/E/R/G/V arrange presets, so both live in the same handler. */
type PaneMoveDir = "left" | "right" | "up" | "down";
const ARROW_PANE_DIR: Readonly<Record<string, PaneMoveDir | undefined>> = {
  ArrowLeft: "left", ArrowRight: "right", ArrowUp: "up", ArrowDown: "down",
};

// Deep-equal two PaneViews so the panes memo can REUSE the prior object ref when
// a layout commit didn't actually change this pane. Keeps <For each={panes()}>
// from recreating (and re-animating the M3 underline of) every strip on a plain
// focus click. Identity deliberately IGNORES rect AND focused: both are read
// live (paneRectById / paneFocusById), so a drag repositions and a focus flip
// re-styles strips WITHOUT re-mounting them (remounting re-upgrades the
// @material/web ripples — the focus-flip DOM churn).
function samePaneView(a: PaneView, b: PaneView): boolean {
  return a.paneId === b.paneId && a.selectedTab === b.selectedTab
    && a.tabIds.length === b.tabIds.length && a.tabIds.every((id, i) => id === b.tabIds[i]);
}

// Slot value-equality for the per-session slot memo: slotBySession mints a new
// Map of new slot objects per layout commit / drag frame / deck resize, so
// ref-equality re-fired termStyle + the data-* effects + CellTerminal's raw
// prop effects for EVERY open session on every commit. Compare exactly the
// slot's fields so only real changes propagate.
type SessionSlot = { rect: Rect; paneId: string; focused: boolean; spotlit?: boolean };
function sameSlot(a: SessionSlot | null, b: SessionSlot | null): boolean {
  if (a === b) return true;
  if (!a || !b) return false;
  return a.paneId === b.paneId && a.focused === b.focused && !!a.spotlit === !!b.spotlit
    && a.rect.x === b.rect.x && a.rect.y === b.rect.y && a.rect.w === b.rect.w && a.rect.h === b.rect.h;
}

// Value-equality for the parked-size map (parkSizeBySession): the memo mints a
// new Map per layout commit / drag frame / deck resize, so ref-equality would
// re-style every parked pane on a focus flip or tab reorder. Compare sizes.
function sameParkSizes(
  a: ReadonlyMap<string, { w: number; h: number }>,
  b: ReadonlyMap<string, { w: number; h: number }>,
): boolean {
  if (a.size !== b.size) return false;
  for (const [id, s] of a) {
    const t = b.get(id);
    if (!t || t.w !== s.w || t.h !== s.h) return false;
  }
  return true;
}

export function TerminalDeck(props: {
  activeSessionId: string | null;
  surfaceVisible: boolean;
}) {
  const navigate = useNavigate();
  // Every open terminal belongs to the shared pane model.
  const openSessions = createMemo(() => Object.values(rootStore.sessions).filter((s) => s.status === "open"));
  const [warmSessionIds, setWarmSessionIds] = createSignal<ReadonlySet<string>>(new Set());

  const activeSession = createMemo(() => (props.activeSessionId ? rootStore.sessions[props.activeSessionId] ?? null : null));
  const newTermFolder = createMemo(() => { const s = activeSession(); return s ? shortCwd(folderPathOf(s)) : ""; });
  const folderKey = createMemo(() => { const s = activeSession(); return s ? folderKeyOf(s) : null; });
  // Sessions that belong in the layout for the active folder. EXCLUDE
  // pending-close (soft-closed) sessions: a closed tab stays status="open" until
  // the delayed kill lands, and without this filter reconcile immediately
  // re-adds it → the pane never collapses on close (the terminal stays mounted
  // in the deck via openSessions so an undo restores it).
  const liveIds = createMemo(() => {
    const fk = folderKey();
    if (!fk) return [];
    return openSessions()
      .filter((s) => folderKeyOf(s) === fk && !isPendingClose(s.id))
      .sort((a, b) => a.created_at - b.created_at)
      .map((s) => s.id);
  });

  // deck pixel size for rect computation
  let deckEl: HTMLDivElement | undefined;
  const [size, setSize] = createSignal({ w: 0, h: 0 });
  onMount(() => {
    const ro = new ResizeObserver(() => { if (deckEl) setSize({ w: deckEl.clientWidth, h: deckEl.clientHeight }); });
    if (deckEl) { ro.observe(deckEl); setSize({ w: deckEl.clientWidth, h: deckEl.clientHeight }); }
    onCleanup(() => ro.disconnect());
  });

  // seed a stable default layout the first time a folder becomes active
  createEffect(() => { const fk = folderKey(); if (fk) seedIfAbsent(fk, liveIds()); });

  // URL → tiling fold: external navigation only (sidebar click, deep link,
  // back/forward, spawn) selects + focuses the active session's pane. A newly
  // spawned cwd can resolve after its opened event (`/tmp` → `/private/tmp` on
  // macOS); react to that folder-key migration too, or the freshly selected
  // terminal remains parked while the destination folder restores an older tab.
  // The live-id dependency is essential for optimistic spawn: the URL can name
  // the new session before its opened event adds it to the saved layout.
  // In-deck clicks do not change either dependency, so their synchronous focus
  // commit cannot be clobbered by the URL's stale session.
  createEffect(on(
    () => [props.activeSessionId, folderKey(), liveIds().join("\u0000")] as const,
    ([active, fk]) => {
      if (!active || !fk) return;
      const l = resolveLayout(fk, liveIds());
      const leaf = findLeafOfTab(l.root, active);
      if (!leaf) return;
      if (l.focusedPaneId === leaf.paneId && leaf.selectedTab === active) return;
      commitLayout(fk, selectTab(l, active));
    },
  ));

  // External nav (sidebar click, deep link, agent command) during an active
  // TRACK-phase swipe aborts the gesture — the neighbor/current pair is now
  // stale. Skip during settle: the commit's own doSelect→navigate changes
  // activeSessionId at the end of the 220ms settle, and clearing there would
  // yank the transform mid-snap. Settle self-clears via its setTimeout.
  createEffect(on(() => props.activeSessionId, (active) => {
    if (!active) return;
    const sw = swipe();
    if (sw && sw.phase === "track" && active !== sw.currentId) setSwipe(null);
  }));

  // transient divider ratios during a drag (don't persist every pointermove)
  const [dragRatios, setDragRatios] = createSignal<Record<string, number>>({});

  // drag-to-tile (P2): live drop-zone preview while a tab is dragged over the deck
  const [dropOverlay, setDropOverlay] = createSignal<{ rect: Rect; zone: DropZone } | null>(null);

  // Reconciled base layout (committed store is the focus source of truth,
  // reconciled against the live set). Does NOT read dragRatios → stays memoized
  // across a drag (no per-frame reconcile); transient drag ratios are applied in
  // view() at the geometry layer so a pointermove re-runs only setRatio+layoutView.
  const layout = createMemo<Layout | null>(() => {
    const fk = folderKey();
    if (!fk) return null;
    return resolveLayout(fk, liveIds());
  });

  const view = createMemo<{ panes: PaneView[]; dividers: ReturnType<typeof layoutView>["dividers"] }>(() => {
    let l = layout();
    if (!l) return { panes: [], dividers: [] };
    // Before the first ResizeObserver tick the deck size is 0×0 — keep every
    // terminal hidden rather than positioning it at 0×0 (the RO-0×0 grid
    // corruption trigger). onMount sets a real size synchronously after render.
    if (size().w === 0 || size().h === 0) return { panes: [], dividers: [] };
    // Transient drag ratios applied HERE only (geometry layer): a pointermove
    // re-runs this cheap setRatio+layoutView, never the reconcile in layout().
    const dr = dragRatios();
    for (const id in dr) l = { ...l, root: setRatio(l.root, id, dr[id]) };
    if (isCompact()) {
      // mobile = one terminal: the focused pane's selected tab, full-bleed
      const leaf = allLeaves(l.root).find((le) => le.paneId === l.focusedPaneId) ?? allLeaves(l.root)[0];
      if (!leaf) return { panes: [], dividers: [] };
      return { panes: [{ paneId: leaf.paneId, rect: { x: 0, y: 0, w: size().w, h: size().h }, tabIds: leaf.tabs, selectedTab: leaf.selectedTab, focused: true }], dividers: [] };
    }
    return layoutView(l, size().w, size().h);
  });
  // Mobile (compact): the flat, ordered list of every tab in the folder — panes
  // collapsed to one scrollable row (topology is desktop-only). Empty
  // off-compact or with no layout; the strip renders only on a terminal route
  // that has sessions. Order is flatTabs (leaf-then-tab); selectedTab is the
  // URL-active session so the highlight tracks navigation instantly.
  const mobileTabs = createMemo<Session[]>(() => {
    if (!isCompact()) return [];
    const l = layout();
    if (!l) return [];
    return flatTabs(l.root)
      .map((t) => rootStore.sessions[t.tabId])
      .filter(Boolean) as Session[];
  });

  // The visible pane holding the floated session (null → inert: compact, folder
  // changed, tab switched away, or pane gone). Floating targets the PANE.
  const spotlightPane = createMemo(() => {
    const sid = spotlightSessionId();
    if (!sid || isCompact()) return null;
    return view().panes.find((p) => p.selectedTab === sid) ?? null;
  });
  // Centered card rect: ~6% inset (min 24px) so the dimmed stack shows at the edges.
  const spotlightRect = createMemo<Rect | null>(() => {
    if (!spotlightPane()) return null;
    const { w, h } = size();
    if (!w || !h) return null;
    const mx = Math.max(w * 0.06, 24), my = Math.max(h * 0.06, 24);
    return { x: mx, y: my, w: w - 2 * mx, h: h - 2 * my };
  });

  // ── Mobile swipe-to-switch (Chrome Android toolbar gesture) ────────────
  // Swiping left/right on MobileDeckBar slides the current terminal out and
  // the next/prev in, in parallel. The neighbor is already mounted (parked at
  // -99999px); surfacing a slot flips its inLayout true (CellTerminal TAB_VISIBLE
  // claim — canvas keeps its last frame, so it never slides in blank). Commit
  // is delayed to the end of the 200ms settle so reactivity matches the visual
  // at switch time (no flash). Mobile-only; reads isCompact() at arm + render.
  type SwipePhase = "track" | "settle";
  type Swipe = {
    phase: SwipePhase;
    currentId: string;          // URL-active session at swipe start
    neighborId: string | null;  // null → at an end (new-terminal / workspace affordance)
    dir: 1 | -1;                 // 1 = finger-left → next; -1 = finger-right → prev
    offset: number;              // live finger dx (px), clamped to [-w, w]
    mode: SwipeMode;             // slide (real neighbor) | new-terminal | workspace
    settleTarget?: "commit" | "cancel"; // set only in endSwipe → keys the settle geometry
    settleMs?: number;           // per-settle duration (momentum); undefined during track
  };
  const [swipe, setSwipe] = createSignal<Swipe | null>(null);
  let newFabArmed = false; // per-gesture latch so the arm haptic fires once (reset in armSwipe)

  const slotBySession = createMemo(() => {
    const m = new Map<string, { rect: Rect; paneId: string; focused: boolean; spotlit?: boolean }>();
    for (const p of view().panes) if (p.selectedTab) m.set(p.selectedTab, { rect: p.rect, paneId: p.paneId, focused: p.focused });
    const sp = spotlightPane(), sr = spotlightRect();
    if (sp && sr && sp.selectedTab) m.set(sp.selectedTab, { rect: sr, paneId: sp.paneId, focused: true, spotlit: true });
    // During a swipe, give the neighbor the SAME rect as the single mobile pane
    // so termStyle positions it full-bleed (top:48 / bottom inset), then the
    // swipe transform (swipeStyleFor) slides it in from the opposite edge.
    const sw0 = swipe();
    if (sw0 && sw0.neighborId && isCompact()) {
      const cur = view().panes[0]; // mobile: exactly one pane
      if (cur) m.set(sw0.neighborId, { rect: cur.rect, paneId: cur.paneId, focused: false });
    }
    return m;
  });

  createEffect(() => {
    const openIds = new Set<string>(openSessions().map((session) => session.id));
    const selectedIds = slotBySession();
    setWarmSessionIds((previous) => {
      let changed = false;
      const next = new Set<string>();
      for (const id of previous) {
        if (openIds.has(id)) next.add(id);
        else changed = true;
      }
      for (const id of selectedIds.keys()) {
        if (!next.has(id)) {
          next.add(id);
          changed = true;
        }
      }
      return changed ? next : previous;
    });
  });


  const mountedSessions = createMemo(() => {
    const warmIds = warmSessionIds();
    const selectedIds = slotBySession();
    return openSessions().filter((session) => warmIds.has(session.id) || selectedIds.has(session.id));
  });

  // Ref-stable pane list for the strips <For>: reuse the prior PaneView object
  // whenever a commit left that pane unchanged, so unchanged strips don't
  // recreate (their sliding underline stays put instead of re-animating).
  let panesCache = new Map<string, PaneView>();
  const panes = createMemo(() => {
    const next = new Map<string, PaneView>();
    const out = view().panes.map((p) => {
      const prev = panesCache.get(p.paneId);
      const stable = prev && samePaneView(prev, p) ? prev : p;
      next.set(p.paneId, stable);
      return stable;
    });
    panesCache = next;
    return out;
  });

  // Live paneId → rect for the strip wrappers, read reactively. Kept SEPARATE
  // from the ref-stable `panes` list so a drag (rect-only change) repositions
  // strips WITHOUT recreating them (samePaneView ignores rect → stable refs).
  const paneRectById = createMemo(() => {
    const m = new Map<string, Rect>();
    for (const p of view().panes) m.set(p.paneId, p.rect);
    return m;
  });

  // Live paneId → focused for the strips, read reactively — same pattern as
  // paneRectById: samePaneView ignores focused, so a focus flip updates this
  // map instead of minting new PaneView refs (no strip re-mount).
  const paneFocusById = createMemo(() => {
    const m = new Map<string, boolean>();
    for (const p of view().panes) m.set(p.paneId, p.focused);
    return m;
  });


  createEffect(() => setVisiblePaneCount(view().panes.length));
  // Spotlit session closed → drop the peek.
  createEffect(() => { const sid = spotlightSessionId(); if (sid && !openSessions().some((s) => s.id === sid)) clearSpotlight(); });
  // Navigate to another folder → drop the peek + abort any in-flight swipe
  // (its neighbor/current tabs belong to the old folder).
  createEffect(on(folderKey, () => { clearSpotlight(); setSwipe(null); }, { defer: true }));

  const stripH = () => (isCompact() ? MOBILE_STRIP_H : STRIP_H);
  // Every tab of every pane → that pane's terminal-area size. A parked pane keeps
  // the size it will be revealed at: any other box changes the .wterm scroll
  // container's clientHeight across park/reveal, which moves the scroll maximum
  // under a pane that is still applying frames — CellGridRenderer's pre-mutation
  // atBottom() check then latches off and bottom-follow (plus scrollback
  // eviction) stays dead until the user manually scrolls to the end.
  // Declared after stripH() because createMemo evaluates eagerly.
  const parkSizeBySession = createMemo(
    () => {
      const m = new Map<string, { w: number; h: number }>();
      const h = stripH();
      for (const p of view().panes) {
        const size = { w: p.rect.w, h: Math.max(0, p.rect.h - h) };
        for (const id of p.tabIds) m.set(id, size);
      }
      return m;
    },
    undefined,
    { equals: sameParkSizes },
  );
  function termStyle(
    slot: { rect: Rect; focused: boolean; spotlit?: boolean } | null,
    park?: { w: number; h: number },
  ): Record<string, string> {
    // A parked pane MUST keep the geometry it will be revealed at (park, from
    // parkSizeBySession) — a differing box moves its scroll maximum while frames
    // are still being applied and latches bottom-follow off. parkSizeBySession
    // only enumerates the CURRENT folder's panes, so a warm cross-folder session
    // (and any pane before the first ResizeObserver tick) has no entry: fall
    // back to the deck's own measured box — the closest truthful guess at the
    // rect it will be revealed at (single-pane folders are exact; multi-pane
    // splits self-heal via noteBoxResize at reveal). 800×600 only when the deck
    // itself is unmeasured (pre-first-layout).
    // Hidden panes park off-screen but stay LAID OUT (visibility:hidden, NOT
    // content-visibility). Skipping their layout cuts per-switch forced layout
    // (~50ms→~11ms across 15+ panes), BUT because content-visibility:hidden drops
    // the whole subtree, REVEALING a pane then re-renders it cold: a deep-history
    // (8000-row) pane measured ~134ms cold vs ~72ms kept-warm. Deep-history reveal
    // is the freeze users hit, so keeping hidden panes warm wins there. The
    // O(open-sessions) per-switch floor needs a different lever (not eagerly
    // mounting every pane), not a content-visibility layout skip.
    if (!slot) {
      const d = size();
      const fw = park?.w ?? (d.w > 0 ? d.w : 800);
      const fh = park?.h ?? (d.h > 0 ? Math.max(0, d.h - stripH()) : 600);
      return {
        position: "absolute", left: "-99999px", top: "0",
        width: `${fw}px`, height: `${fh}px`,
        visibility: "hidden", "pointer-events": "none",
      };
    }
    const r = slot.rect;
    // Spotlit pane floats as a full card with NO strip above it (its strip is
    // hidden below): the terminal fills the whole card rect, all corners rounded.
    // The .pane-spotlight-card frame (z8) rings/shadows it from behind.
    if (slot.spotlit) {
      return {
        position: "absolute", left: `${r.x}px`, top: `${r.y}px`, width: `${r.w}px`, height: `${r.h}px`,
        visibility: "inherit", "z-index": "9", overflow: "hidden", "border-radius": "12px",
      };
    }
    return {
      position: "absolute", left: `${r.x}px`, top: `${r.y + stripH()}px`,
      width: `${r.w}px`, height: `${Math.max(0, r.h - stripH())}px`,
      visibility: "inherit", "z-index": slot.focused ? "2" : "1",
    };
  }

  // ── Swipe transform per terminal slot ─────────────────────────────────
  // Composed AFTER termStyle (which never sets transform) so the slot's base
  // geometry is intact. During track: no transition (finger-follow). During
  // settle: constant-speed slide (settleDurationMs = 500ms per screen-width) with a
  // decelerate ease-out so a full traverse never blinks past and its direction reads.
  const SETTLE_SLACK_MS = 20; // setTimeout outlasts the CSS transition so commit/clear lands after the visual
  function swipeOffsetsPx(sw: Swipe, w: number): { current: number; neighbor: number } {
    if (sw.phase === "settle")
      return sw.settleTarget === "commit"
        ? { current: -sw.dir * w, neighbor: 0 }
        : { current: 0, neighbor: sw.dir * w };
    return { current: sw.offset, neighbor: sw.offset + sw.dir * w };
  }
  function swipeStyleFor(id: string): Record<string, string> {
    const sw = swipe();
    if (!sw || (id !== sw.currentId && id !== sw.neighborId)) return {};
    const w = size().w;
    const transition = sw.phase === "settle" ? `transform ${sw.settleMs ?? 200}ms ${SWIPE_SLIDE_EASE}` : "none";
    const o = swipeOffsetsPx(sw, w);
    if (id === sw.currentId) {
      if (sw.mode === "new-terminal") {
        const p = sw.phase === "settle" ? (sw.settleTarget === "commit" ? 1 : 0) : newFabProgress(sw.offset, w);
        const { scale, shiftFrac, radius } = peekCard(p);
        const ms = sw.settleMs ?? 200;
        return {
          transform: `translateX(${shiftFrac * w}px) scale(${scale})`,
          "transform-origin": "center center",
          "border-radius": `${radius}px`,
          overflow: "hidden",
          "box-shadow": p > 0 ? `0 ${Math.round(8 * p)}px ${Math.round(30 * p)}px color-mix(in srgb, var(--md-shadow) ${Math.round(50 * p)}%, transparent)` : "none",
          transition: sw.phase === "settle"
            ? `transform ${ms}ms ${SWIPE_DECEL}, border-radius ${ms}ms ${SWIPE_DECEL}, box-shadow ${ms}ms ${SWIPE_DECEL}`
            : "none",
        };
      }
      if (sw.mode === "workspace") return {};
      return { transform: `translateX(${o.current}px)`, transition };
    }
    if (!sw.neighborId) return {}; // new-terminal/workspace: no session neighbor slot (placeholder owns that side)
    return { transform: `translateX(${o.neighbor}px)`, transition };
  }

  // The neighbor terminal's own full bar rides in as a distinct card during a real
  // neighbor slide (Chrome-mobile whole-tab slide). null for new-terminal / workspace
  // end affordances and off-compact. slide mode always has a real neighbor.
  const barNeighborId = createMemo<string | null>(() => {
    const sw = swipe();
    if (!sw || !isCompact() || sw.mode !== "slide") return null;
    return sw.neighborId;
  });


  // Behind-surface that the shrinking terminal reveals (predictive-back peek).
  function newPeekStyle(): Record<string, string> {
    const sw = swipe();
    if (!sw || sw.mode !== "new-terminal") return { display: "none" };
    const r = view().panes[0]?.rect; const w = size().w;
    if (!r || w <= 0) return { display: "none" };
    const p = newFabProgress(sw.offset, w);
    const committing = sw.phase === "settle" && sw.settleTarget === "commit";
    const settling = sw.phase === "settle";
    return {
      position: "absolute", left: "0px", top: `${r.y + stripH()}px`,
      width: `${r.w}px`, height: `${Math.max(0, r.h - stripH())}px`, "z-index": "1",
      opacity: committing ? "1" : settling ? "0" : `${Math.min(1, p)}`,
      transition: settling ? `opacity ${sw.settleMs ?? NEW_BLOOM_MS}ms ${SWIPE_DECEL}` : "none",
    };
  }
  // Circular + FAB that grows under the finger and container-transforms to full deck on commit.
  function newFabStyle(): Record<string, string> {
    const sw = swipe();
    if (!sw || sw.mode !== "new-terminal") return { display: "none" };
    const r = view().panes[0]?.rect; const w = size().w;
    if (!r || w <= 0) return { display: "none" };
    const areaTop = r.y + stripH();
    const areaH = Math.max(0, r.h - stripH());
    const committing = sw.phase === "settle" && sw.settleTarget === "commit";
    const settling = sw.phase === "settle";
    const p = newFabProgress(sw.offset, w);
    const FAB = 56;
    if (committing) {
      return {
        position: "absolute", left: "0px", top: `${areaTop}px`,
        width: `${r.w}px`, height: `${areaH}px`, "border-radius": "0px",
        transform: "scale(1)", opacity: "1", "z-index": "6",
        transition: `left ${NEW_BLOOM_MS}ms ${SWIPE_DECEL}, top ${NEW_BLOOM_MS}ms ${SWIPE_DECEL}, width ${NEW_BLOOM_MS}ms ${SWIPE_DECEL}, height ${NEW_BLOOM_MS}ms ${SWIPE_DECEL}, border-radius ${NEW_BLOOM_MS}ms ${SWIPE_DECEL}`,
      };
    }
    const ms = sw.settleMs ?? 200;
    return {
      position: "absolute", left: `${r.w - 20 - FAB}px`, top: `${areaTop + areaH / 2 - FAB / 2}px`,
      width: `${FAB}px`, height: `${FAB}px`, "border-radius": "50%",
      transform: `scale(${settling ? NEW_FAB_MIN_SCALE : newFabScale(p)})`, "transform-origin": "center center",
      opacity: settling ? "0" : `${Math.min(1, p * 1.4)}`, "z-index": "6",
      transition: settling ? `transform ${ms}ms ${SWIPE_DECEL}, opacity ${ms}ms ${SWIPE_DECEL}` : "none",
    };
  }

  // ── ops (apply a pure transform to the CURRENT layout + persist) ───────────
  function apply(fn: (l: Layout) => Layout): void {
    const fk = folderKey();
    const l = layout();
    if (fk && l) commitLayout(fk, fn(l));
  }
  // select/focus/close logic lives in lib/deckOps.ts so the agent command
  // channel (lib/uiCommandDispatch.ts) drives the exact same code paths. The
  // ctx hands over the live memos — ops re-read them around their own commits,
  // preserving the original closure semantics (doClose capture-before-async,
  // doSelect post-commit spotlight follow).
  const opsCtx: DeckOpsCtx = {
    folderKey,
    layout,
    activeSessionId: () => props.activeSessionId,
    navigate,
  };
  function doSelect(id: string): void {
    if (activeComposeSessionId() !== id) releaseActiveComposeFocus();
    selectTabOp(opsCtx, id, spotlightPane()?.paneId ?? null);
  }

  // ── Swipe arm/track/end (driven by MobileDeckBar's onSwipe* callbacks) ──
  // armSwipe picks direction from the first dx sign, resolves the neighbor
  // from mobileTabs() order (flatTabs = leaf-then-tab), and enters track.
  function armSwipe(dx: number): void {
    newFabArmed = false; // reset the arm-haptic latch per gesture
    const cur = swipe();
    if (cur?.phase === "settle") return; // mid-settle, ignore re-grab
    if (!isCompact()) return;
    const tabs = mobileTabs();
    const idx = tabs.findIndex((t) => t.id === props.activeSessionId);
    if (idx < 0) return;
    const dir: 1 | -1 = dx < 0 ? 1 : -1; // finger-left → next, finger-right → prev
    const neighborId = dir === 1 ? tabs[idx + 1]?.id ?? null : tabs[idx - 1]?.id ?? null;
    // No delta guard: a single tab still arms — forward → new-terminal, backward → workspace.
    const mode = endMode(dir, !!neighborId);
    setSwipe({ phase: "track", currentId: props.activeSessionId!, neighborId, dir, offset: dx, mode });
  }
  function trackSwipe(dx: number): void {
    setSwipe((prev) => {
      if (!prev || prev.phase !== "track") return prev;
      const w = size().w;
      // clamp raw finger travel to one screen width either way
      const clamped = Math.max(-w, Math.min(w, dx));
      return { ...prev, offset: clamped };
    });
    const sw = swipe();
    if (sw?.mode === "workspace") { dragDrawer(openOffsetPx(sw.offset, window.innerWidth)); return; }
    if (sw?.mode === "new-terminal" && !newFabArmed && newFabProgress(sw.offset, size().w) >= 1) {
      newFabArmed = true;
      navigator.vibrate?.(8); // progressive enhancement; no-op where unsupported
    }
  }
  function endSwipe(dx: number, velocity: number): void {
    const cur = swipe();
    if (cur?.mode === "workspace" && cur.phase === "track") {
      settleDrawerOpen(shouldCommitSwitch(dx, velocity, cur.dir, window.innerWidth));
      setSwipe(null);
      return;
    }
    setSwipe((prev) => {
      if (!prev || prev.phase !== "track") return prev;
      const w = size().w;
      const off = Math.abs(prev.offset); // clamped in trackSwipe → [0, w]
      // Commit is directional + travel-gated (shouldCommitSwitch), same gate for
      // every mode: a reversed release, a weak drag, or a backward end-of-touch
      // flick must NOT commit. All three modes share the commit geometry — the
      // current slot slides fully off in the swipe dir, the neighbor/placeholder
      // lands at 0 — so settleTarget:"commit" drives swipeOffsetsPx uniformly.
      const commit = shouldCommitSwitch(dx, velocity, prev.dir, w);
      if (commit) {
        if (prev.mode === "new-terminal") {
          navigator.vibrate?.(12);
          setTimeout(() => { void doNewTab(layout()?.focusedPaneId ?? ""); setSwipe(null); }, NEW_BLOOM_MS + SETTLE_SLACK_MS);
          return { ...prev, phase: "settle", settleTarget: "commit", settleMs: NEW_BLOOM_MS };
        }
        // Action fires after the settle so the visual already matches reactivity.
        // Constant-speed settle: 500ms per screen-width over the remaining travel.
        const ms = settleDurationMs(w - off, w);
        const neighborId = prev.neighborId;
        setTimeout(() => { doSelect(neighborId!); setSwipe(null); }, ms + SETTLE_SLACK_MS);
        return { ...prev, phase: "settle", settleTarget: "commit", offset: -prev.dir * w, settleMs: ms };
      }
      // cancel: spring back. current returns to 0, neighbor/placeholder off-edge.
      const ms = settleDurationMs(off, w); // remaining travel back to 0
      setTimeout(() => setSwipe(null), ms + SETTLE_SLACK_MS);
      return { ...prev, phase: "settle", settleTarget: "cancel", offset: 0, settleMs: ms };
    });
  }
  function doReorder(paneId: string, ids: string[]): void { apply((l) => reorderTab(l, paneId, ids)); }
  function doFocusPane(paneId: string): void { focusPaneOp(opsCtx, paneId); }
  function doClose(s: Session): void { closeSessionOp(opsCtx, s); }
  function anchorFor(paneId: string): Session | null {
    const p = view().panes.find((pv) => pv.paneId === paneId);
    return (p && rootStore.sessions[p.selectedTab]) || activeSession();
  }
  async function spawnSibling(anchor: Session, sessionId?: string): Promise<string> {
    const folder = folderPathOf(anchor);
    return anchor.workspace_id
      ? await spawnInWorkspace(anchor.worker_fp, anchor.workspace_id, folder, sessionId)
      : await spawnShell(anchor.worker_fp, folder, sessionId);
  }
  async function doNewTab(paneId: string): Promise<void> {
    const anchor = anchorFor(paneId);
    if (!anchor) return;
    releaseActiveComposeFocus();
    apply((l) => focusPane(l, paneId)); // reconcile appends the optimistic placeholder into this pane
    const sid = beginOptimisticSpawn(anchor); // tab + pane + CellTerminal render THIS frame
    navigate(`/s/${sid}`); // URL-fold selects the new tab in the focused pane
    const t0 = Date.now();
    try {
      await spawnSibling(anchor, sid);
      diag("spawn.optimistic", { session_id: sid, rtt_ms: Date.now() - t0 });
      // Closed mid-flight → reap the now-real PTY and leave the tab gone.
      if (wasAborted(sid)) { clearAborted(sid); void coordClient.sessionsKill({ sessionId: sid }); return; }
      endOptimisticSpawn(sid); // clears pending → CellTerminal fires INITIAL claim + paints
      maybeAutoLaunchAgent(sid);
    } catch (e) {
      failOptimisticSpawn(sid, e); // removes placeholder → reconcile prunes the tab + toast
    }
  }
  async function doSplit(dir: PaneDir): Promise<void> {
    const l = layout();
    if (!l) return;
    const paneId = l.focusedPaneId;
    const anchor = anchorFor(paneId);
    if (!anchor) return;
    releaseActiveComposeFocus();
    const id = await spawnSibling(anchor);
    await waitForSession(id); // must be live before it can occupy a split pane
    maybeAutoLaunchAgent(id);
    apply((cur) => splitLeaf(cur, paneId, dir, id, false));
    navigate(`/s/${id}`);
  }
  function doSpotlight(): void {
    if (spotlightSessionId()) { clearSpotlight(); return; }
    const p = view().panes.find((pv) => pv.paneId === layout()?.focusedPaneId);
    if (p?.selectedTab) setSpotlightSessionId(p.selectedTab);
  }
  /** The pane that owns the keyboard — the target of every deck shortcut. The
   *  view's own focused flag is the fallback because compact synthesizes a
   *  single focused pane rather than echoing focusedPaneId. */
  function focusedPaneView(): PaneView | null {
    const ps = view().panes;
    const fid = layout()?.focusedPaneId;
    return ps.find((p) => p.paneId === fid) ?? ps.find((p) => p.focused) ?? null;
  }
  // History, URL folds, optimistic reconciliation, and shortcuts can all change
  // the keyboard-owning tab without unmounting its old pane composer. React only
  // to that selection transition (not a new claim in an unfocused pane), then
  // clear a claim that belongs to the session being left.
  createEffect(on(
    () => focusedPaneView()?.selectedTab ?? null,
    (selectedId) => {
      const ownerId = activeComposeSessionId();
      if (ownerId && ownerId !== selectedId) releaseActiveComposeFocus();
    },
    { defer: true },
  ));
  /** ⌘/Ctrl+1..8 → Nth tab of the focused pane, 9 → its LAST (browser
   *  convention). Routes through doSelect — the very call a strip tab click
   *  makes — so select + pane focus + navigate + spotlight-follow stay one path.
   *  Fewer tabs than the digit → nothing happens. */
  function activateTabAt(digit: number): void {
    const p = focusedPaneView();
    if (!p) return;
    const id = digit === 9 ? p.tabIds[p.tabIds.length - 1] : p.tabIds[digit - 1];
    if (id) doSelect(id);
  }
  /** Focus the pane adjacent to the focused one, resolved on the rects the deck
   *  already paints (layoutView → layoutRects): among panes whose centre lies
   *  strictly beyond the focused centre on that axis, take the nearest centre.
   *  Geometry, not tree order — the keyboard walks the tiling the user sees. */
  function focusAdjacentPane(dir: PaneMoveDir): void {
    const from = focusedPaneView();
    if (!from) return;
    const cx = from.rect.x + from.rect.w / 2, cy = from.rect.y + from.rect.h / 2;
    let bestId: string | null = null;
    let bestD = Infinity;
    for (const p of view().panes) {
      if (p.paneId === from.paneId) continue;
      const dx = p.rect.x + p.rect.w / 2 - cx, dy = p.rect.y + p.rect.h / 2 - cy;
      const ahead = dir === "left" ? dx < 0 : dir === "right" ? dx > 0 : dir === "up" ? dy < 0 : dy > 0;
      if (!ahead) continue;
      const d = dx * dx + dy * dy;
      if (d < bestD) { bestD = d; bestId = p.paneId; }
    }
    if (bestId) {
      // A pane-navigation chord owns the keyboard even when another pane's
      // composer textarea is focused. Release that old token before focus can
      // mount a compact replacement; the new pane's terminal then wins focus.
      releaseActiveComposeFocus();
      doFocusPane(bestId);
    }
  }
  // "Arrange" — balance keeps the current tree (panes/tabs/focus), only
  // re-balancing ratios; the rebuild kinds replace the layout with a preset
  // tiling of the folder's sessions (one session per pane).
  function doArrange(kind: ArrangeKind): void {
    const fk = folderKey();
    const l = layout();
    if (!fk || !l) return;
    clearSpotlight();
    let next = arrangeLayout(kind, l, liveIds());
    // Rebuild presets reset focusedPaneId to the first leaf, desyncing the
    // priority ring from the URL-active session. Re-point focus/selection at it
    // (balance preserves focus by construction, so leave it — a pending
    // doFocusPane navigate may not have reached props.activeSessionId yet).
    const active = props.activeSessionId;
    if (kind !== "balance" && active && findLeafOfTab(next.root, active)) next = selectTab(next, active);
    commitLayout(fk, next);
    pulseArrange();
  }

  // divider drag → live ratio (visual) → commit on release
  function onDividerDrag(splitId: string, ratio: number): void { setDragRatios((p) => ({ ...p, [splitId]: ratio })); }
  function onDividerCommit(splitId: string, ratio: number): void {
    apply((l) => ({ ...l, root: setRatio(l.root, splitId, ratio) }));
    setDragRatios((p) => { const n = { ...p }; delete n[splitId]; return n; });
  }

  // ── drag-to-tile (P2): a tab dragged out of its strip onto a pane EDGE splits
  //    that pane; onto a pane's strip / body-center MERGES into it. dropZones.ts
  //    does the edge-band math; the overlay previews the target region. ─────────
  function deckLocal(clientX: number, clientY: number): { x: number; y: number } {
    const r = deckEl?.getBoundingClientRect();
    return { x: clientX - (r?.left ?? 0), y: clientY - (r?.top ?? 0) };
  }
  function tileTarget(originPaneId: string, clientX: number, clientY: number): TileTarget {
    const { x, y } = deckLocal(clientX, clientY);
    return tileTargetFor(view().panes, originPaneId, x, y, STRIP_H);
  }
  function onTabDragMove(originPaneId: string, clientX: number, clientY: number): void {
    const t = tileTarget(originPaneId, clientX, clientY);
    setDropOverlay(t ? { rect: zoneRect(t.rect, t.zone), zone: t.zone } : null);
  }
  function onTabTileDrop(tabId: string, originPaneId: string, clientX: number, clientY: number): boolean {
    setDropOverlay(null);
    const t = tileTarget(originPaneId, clientX, clientY);
    if (!t || t.zone === "reorder") return false; // reorder → let the strip handle it
    const split = zoneToSplit(t.zone);
    if (split) apply((l) => splitLeaf(l, t.paneId, split.dir, tabId, split.insertFirst));
    else apply((l) => moveTab(l, tabId, t.paneId));
    navigate(`/s/${tabId}`);
    return true;
  }
  function onTabDragEnd(): void { setDropOverlay(null); }

  // Click in a pane's BODY → focus that pane (the mousedown-prevent net in
  // CellTerminal lets clicks inside [data-pane] through for exactly this). A
  // press inside a STRIP is skipped: it may start a tab drag, and focusing here
  // re-renders + recreates the tab node mid-gesture (breaking the drag). Tab
  // clicks focus via onSelect instead.
  function onDeckPointerDown(e: PointerEvent): void {
    const t = e.target as HTMLElement | null;
    if (t?.closest("[data-pane-strip]")) return;
    const el = t?.closest<HTMLElement>("[data-pane-id]");
    const pid = el?.getAttribute("data-pane-id");
    if (pid) doFocusPane(pid);
    // Middle-click = bring-to-front toggle, same path as ⌘⏎. doFocusPane above
    // already moved focus to the clicked pane synchronously (commitLayout is a
    // plain signal set — paneLayoutStore.ts:70), so doSpotlight() floats THAT
    // pane. While a pane is floated, the z7 backdrop swallows pointerdown on
    // everything except the floated slot (z9) → middle-click there toggles back;
    // backdrop middle-click clears via its own onPointerDown (stopPropagation
    // keeps this handler out — no double-toggle).
    if (e.button === 1 && pid && !isCompact() && !t?.closest("a")) {
      e.preventDefault(); // suppress win/linux autoscroll + compat mousedown into the TUI mouse-forward path
      doSpotlight();
    }
  }

  // Keyboard and assistive-tech focus does not produce pointerdown. Let a
  // composer focus claim bubble first, then align the deck's shortcut target
  // with the pane that actually owns that focused control.
  function onDeckFocusIn(e: FocusEvent): void {
    const target = e.target;
    if (!(target instanceof Element) || !target.closest('[data-testid="mobile-chat-input"]')) return;
    const pane = target.closest<HTMLElement>("[data-pane-id]");
    const paneId = pane?.getAttribute("data-pane-id");
    if (paneId && paneId !== layout()?.focusedPaneId) doFocusPane(paneId);
  }

  // ⌘D split right · ⌘⇧D split down · ⌘⏎ bring-to-front (spotlight) ·
  // ⌘⌥B/E/R/G/V arrange presets · ⌘/Ctrl+1..9 tab N of the focused pane ·
  // ⌘/Ctrl+⌥arrow walk to the adjacent pane · ⌘/Ctrl+⌥T new terminal in the
  // focused pane (plain ⌘T is a browser accelerator a page can never see).
  // Cmd-combos never reach the PTY, so intercepting them is safe. Only while a
  // terminal folder is active.
  onMount(() => {
    const onKey = (e: KeyboardEvent) => {
      if (e.defaultPrevented) return;

      if (e.key === "Escape" && spotlightSessionId()) { e.preventDefault(); clearSpotlight(); return; }
      // These three accept EITHER modifier: the ⌘-only gate below is
      // macOS-shaped, but digits/arrows/new-tab are exactly what Linux and
      // Windows users reach for with Ctrl. preventDefault keeps the browser's
      // own Ctrl+N tab switch, Alt+Arrow history nav and Ctrl+Alt+T launcher
      // from firing on top of ours.
      const mod = e.metaKey || e.ctrlKey;
      if (mod && folderKey()) {
        if (e.altKey) {
          const dir = ARROW_PANE_DIR[e.key];
          if (dir) { e.preventDefault(); focusAdjacentPane(dir); return; }
          if (e.key.toLowerCase() === "t") { e.preventDefault(); void doNewTab(layout()?.focusedPaneId ?? ""); return; }
        } else if (e.key.length === 1 && e.key >= "1" && e.key <= "9") {
          e.preventDefault();
          activateTabAt(Number(e.key));
          return;
        }
      }
      if (!e.metaKey || e.ctrlKey) return;
      if (!folderKey()) return;
      const k = e.key.toLowerCase();
      if (e.altKey) { // ⌘⌥ = arrange presets
        const preset: ArrangeKind | null =
          k === "b" ? "balance" : k === "e" ? "even" : k === "r" ? "rows" :
          k === "g" ? "tiled" : k === "v" ? "main-vertical" : null;
        if (preset) { e.preventDefault(); doArrange(preset); }
        return;
      }
      if (k === "d") { e.preventDefault(); void doSplit(e.shiftKey ? "col" : "row"); }
      else if (e.key === "Enter") { e.preventDefault(); doSpotlight(); }
    };
    document.addEventListener("keydown", onKey);
    onCleanup(() => document.removeEventListener("keydown", onKey));
  });

  // ── Deck-wide swipe detection (bar + terminal body) ────────────────────
  // ONE capture-phase touch listener on deckEl covers the whole compact deck.
  // Capture runs before CellTerminal's displayRef listeners, so a claimed
  // horizontal move (stopPropagation) silences the terminal's scroll/mouse
  // forwarding; vertical is released untouched so scrollback/mouse-mode work.
  // AppShell's window-capture edge-drawer runs earlier and stopPropagations
  // edge gestures, so a ≤EDGE_PX start never reaches here (we also bail on it).
  onMount(() => {
    const el = deckEl;
    if (!el) return;
    let startX = 0, startY = 0, lastX = 0;
    let axis: "none" | "x" | "y" = "none";
    let armed = false;
    let tracking = false; // a valid single-touch start was recorded
    let samples: { x: number; t: number }[] = [];
    const onStart = (e: TouchEvent) => {
      armed = false;
      axis = "none";
      tracking = false;
      if (!isCompact() || e.touches.length !== 1) return;
      const t = e.touches[0]!;
      if (t.clientX <= EDGE_PX) return; // defer to the drawer edge-swipe
      startX = t.clientX; startY = t.clientY; lastX = t.clientX;
      samples = [{ x: t.clientX, t: performance.now() }];
      tracking = true;
    };
    const onMove = (e: TouchEvent) => {
      if (!tracking) return;
      const t = e.touches[0];
      if (!t) return;
      const dx = t.clientX - startX, dy = t.clientY - startY;
      if (axis === "none") {
        const lock = lockAxis(dx, dy);
        if (lock === "none") return;
        axis = lock;
      }
      if (axis !== "x") return; // vertical → let the terminal scroll/forward
      e.preventDefault();
      e.stopPropagation();
      const now = performance.now();
      samples.push({ x: t.clientX, t: now });
      while (samples.length > 2 && samples[0]!.t < now - 120) samples.shift();
      lastX = t.clientX;
      // Finger-follow: real dx drives the gesture — finger-left → next, finger-right → prev.
      if (!armed) { armed = true; armSwipe(dx); }
      trackSwipe(dx);
    };
    const onEnd = () => {
      if (!armed) { tracking = false; return; }
      armed = false;
      tracking = false;
      const now = performance.now();
      while (samples.length > 1 && samples[0]!.t < now - 80) samples.shift();
      let velocity = 0;
      if (samples.length >= 2) {
        const first = samples[0]!, last = samples[samples.length - 1]!;
        const dt = last.t - first.t;
        if (dt > 0) velocity = (last.x - first.x) / dt;
      }
      endSwipe(lastX - startX, velocity);
      samples = [];
    };
    el.addEventListener("touchstart", onStart, { capture: true, passive: true });
    el.addEventListener("touchmove", onMove, { capture: true, passive: false });
    el.addEventListener("touchend", onEnd, { capture: true, passive: true });
    el.addEventListener("touchcancel", onEnd, { capture: true, passive: true });
    onCleanup(() => {
      el.removeEventListener("touchstart", onStart, { capture: true });
      el.removeEventListener("touchmove", onMove, { capture: true });
      el.removeEventListener("touchend", onEnd, { capture: true });
      el.removeEventListener("touchcancel", onEnd, { capture: true });
    });
  });

  // Composer growth must not change this element's client box: its
  // ResizeObserver drives pane geometry and terminal PTY row claims. Shift the
  // painted deck on the compositor instead, with no transition that could leave
  // an intermediate terminal/composer overlap.
  return (
    <div
      ref={deckEl}
      data-testid="terminal-deck"
      data-multi-pane={view().panes.length > 1 ? "true" : "false"}
      data-resizing={isResizeDragging() ? "true" : undefined}
      onPointerDown={onDeckPointerDown}
      onFocusIn={onDeckFocusIn}
      style={{
        flex: "1",
        position: "relative",
        overflow: "hidden",
        background: "var(--term-bg)",
        "touch-action": isCompact() ? "pan-y" : "auto",
        transform: "translate3d(0, calc(0px - var(--term-chat-growth, 0px)), 0)",
      }}
    >
      <Show when={openSessions().length === 0}>
        <div style={{ position: "absolute", inset: "0", display: "flex", "align-items": "center", "justify-content": "center", color: "var(--text-lo)", "font-size": "13px" }}>
          No session selected.
        </div>
      </Show>

      {/* Cold sessions stay unmounted; selected and previously visible sessions
           retain stable slots for this deck lifetime. */}
      <For each={mountedSessions()}>
        {(s) => {
          const slot = createMemo(() => slotBySession().get(s.id) ?? null, undefined, { equals: sameSlot });
          return (
            <div data-testid={`terminal-slot-${s.id}`} data-pane-slot data-pane data-pane-id={slot()?.paneId ?? ""} data-focused={slot()?.focused ? "true" : "false"} data-spotlit={slot()?.spotlit ? "true" : undefined} style={{ ...termStyle(slot(), parkSizeBySession().get(s.id)), ...swipeStyleFor(s.id) }}>
              <CellTerminal
                session={s}
                inLayout={!!slot()}
                focused={slot()?.focused ?? false}
                spotlit={slot()?.spotlit ?? false}

                surfaceVisible={props.surfaceVisible}
                surfaceActive={
                  props.surfaceVisible
                  && (spotlightPane() === null || slot()?.spotlit === true)
                }
              />
            </div>
          );
        }}
      </For>

      {/* mobile (compact): Chrome-style workspace bar above the full-bleed
           terminal. Off-compact this block is inert. */}
      <Show when={isCompact() && mobileTabs().length > 0}>
        {/* Whole-tab card (Chrome mobile): the bar rides the SAME per-slot swipe
             transform as its terminal body (swipeStyleFor), so the current bar+body
             slide off together and the neighbor's own bar slides in as a distinct card. */}
        <div
          data-testid="mobile-strip-wrap"
          style={{
            position: "absolute", left: "0", top: "0", width: "100%",
            height: `${MOBILE_STRIP_H}px`, "z-index": "3",
            ...swipeStyleFor(props.activeSessionId ?? ""),
          }}
        >
          <MobileDeckBar
            tabs={mobileTabs()}
            selectedTab={props.activeSessionId ?? ""}
            onSelect={doSelect}
            onClose={doClose}
            onNewTab={() => void doNewTab(layout()?.focusedPaneId ?? "")}
          />
        </div>
        <Show when={barNeighborId()}>
          {(nid) => (
            <div
              data-testid="mobile-strip-wrap-neighbor"
              style={{
                position: "absolute", left: "0", top: "0", width: "100%",
                height: `${MOBILE_STRIP_H}px`, "z-index": "3",
                ...swipeStyleFor(nid()),
              }}
            >
              <MobileDeckBar
                tabs={mobileTabs()}
                selectedTab={nid()}
                onSelect={doSelect}
                onClose={doClose}
                onNewTab={() => void doNewTab(layout()?.focusedPaneId ?? "")}
              />
            </div>
          )}
        </Show>
      </Show>
      {/* mobile forward-swipe affordance: the current terminal shrinks into a rounded
           card, a new-terminal surface peeks behind it, and a + FAB grows under the
           finger; on commit the FAB container-transforms into the full new terminal. */}
      <Show when={isCompact() && swipe()?.mode === "new-terminal"}>
        <div class="deck-new-peek" data-testid="deck-new-peek" style={newPeekStyle()} aria-hidden="true">
          <div class="deck-new-peek__label">
            <svg width="20" height="20" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2.5" stroke-linecap="round" aria-hidden="true"><path d="M12 5v14M5 12h14" /></svg>
            <span>New terminal · {newTermFolder()}</span>
          </div>
        </div>
        <div
          class="deck-new-fab"
          data-testid="deck-new-fab"
          data-armed={newFabProgress(swipe()!.offset, size().w) >= 1 ? "true" : undefined}
          style={newFabStyle()}
          aria-hidden="true"
        >
          <svg width="30" height="30" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2.5" stroke-linecap="round" aria-hidden="true"><path d="M12 5v14M5 12h14" /></svg>
        </div>
      </Show>
      {/* per-pane tab strips + dividers (desktop only — mobile uses the deck bar above) */}
      <Show when={!isCompact()}>
        <For each={panes()}>
          {(p) => {
            const tabs = createMemo(() => p.tabIds.map((id) => rootStore.sessions[id]).filter(Boolean) as Session[]);
            const rect = () => paneRectById().get(p.paneId) ?? p.rect;
            return (
              <Show when={p.paneId !== spotlightPane()?.paneId}>
                <div data-pane data-pane-id={p.paneId} style={{ position: "absolute", left: `${rect().x}px`, top: `${rect().y}px`, width: `${rect().w}px`, height: `${STRIP_H}px`, "z-index": "3" }}>
                  <Show when={tabs().length > 0}>
                    <PaneStrip
                      paneId={p.paneId}
                      tabs={tabs()}
                      selectedTab={p.selectedTab}
                      focused={paneFocusById().get(p.paneId) ?? false}
                      onSelect={doSelect}
                      onClose={doClose}
                      onReorder={(ids) => doReorder(p.paneId, ids)}
                      onNewTab={() => void doNewTab(p.paneId)}
                      onTabDragMove={(x, y) => onTabDragMove(p.paneId, x, y)}
                      onTabTileDrop={(tid, x, y) => onTabTileDrop(tid, p.paneId, x, y)}
                      onTabDragEnd={onTabDragEnd}
                    />
                  </Show>
                </div>
              </Show>
            );
          }}
        </For>
        <Index each={view().dividers}>
          {(d) => <PaneDivider divider={d} deckEl={() => deckEl} onDrag={onDividerDrag} onCommit={onDividerCommit} />}
        </Index>
        <Show when={dropOverlay()}>
          {(o) => (
            <div
              class="pane-drop-overlay"
              data-zone={o().zone}
              style={{ position: "absolute", left: `${o().rect.x}px`, top: `${o().rect.y}px`, width: `${o().rect.w}px`, height: `${o().rect.h}px`, "z-index": "5", "pointer-events": "none" }}
            />
          )}
        </Show>
        <Show when={liveIds().length >= 2}>
          <div style={{ position: "absolute", top: "0", right: "0", height: `${STRIP_H}px`, display: "flex", "align-items": "center", padding: "0 6px", "z-index": "4" }}>
            <ArrangeMenu onArrange={doArrange} />
          </div>
        </Show>
      </Show>
      <Show when={spotlightRect()}>
        {(r) => (
          <>
            {/* Reduced motion: skip the 120ms scrim fade and paint the END
                state (fully opaque) on the first frame. Read here, at the
                moment the spotlight opens — i.e. at animation start. */}
            <div
              class="pane-spotlight-backdrop"
              data-testid="pane-spotlight-backdrop"
              style={{ position: "absolute", inset: "0", "z-index": "7", ...(prefersReducedMotion() ? { animation: "none" } : {}) }}
              onPointerDown={(e) => { e.stopPropagation(); clearSpotlight(); }}
              onContextMenu={(e) => { e.preventDefault(); clearSpotlight(); }}
              aria-hidden="true"
            />
            <div
              class="pane-spotlight-card"
              style={{ position: "absolute", left: `${r().x}px`, top: `${r().y}px`, width: `${r().w}px`, height: `${r().h}px`, "z-index": "8", "pointer-events": "none" }}
              aria-hidden="true"
            />
          </>
        )}
      </Show>
    </div>
  );
}

// Per-pane tab strip (tabs-in-panes). One strip per layout pane: renders
// that pane's tabs, click selects, drag reorders within the strip, ✕ closes,
// + spawns a sibling. Data + actions come from props (the layout store drives
// it) — extracted from the old URL-scoped TabBar so the machinery isn't forked.
// The M3 sliding indicator + pointer drag-reorder are ported verbatim.
// Callers: MainPane.tsx TerminalDeck (one per pane). Drag-to-EDGE (split) is
// added in P2 on top of this reorder.

import { For, createMemo, createSignal, createEffect, on, onCleanup, Show } from "solid-js";
import { ClaudeMark } from "./sidebar/StatusGlyph.tsx";
import { isClaudeSession } from "../lib/isClaudeSession.ts";
import { sessionTitle } from "../lib/sessionTitle.ts";
import { IconButton } from "./Settings/md/IconButton.tsx";
import { attentionOf, presentationOf, isActionable } from "../lib/agentStatus.ts";
import { dragArmed } from "../lib/dragThreshold.ts";
import type { Session } from "@roost/shared/wire";
import "@material/web/ripple/ripple.js";

export interface PaneStripProps {
  paneId: string;
  tabs: Session[];
  selectedTab: string;
  focused: boolean;
  onSelect: (id: string) => void;
  onClose: (s: Session) => void;
  onReorder: (orderedIds: string[]) => void;
  onNewTab: () => void;
  // Drag-to-tile (P2): report the live pointer so the deck can preview a drop
  // zone; on release the deck may split/merge and return true (skip reorder).
  onTabDragMove?: (clientX: number, clientY: number) => void;
  onTabTileDrop?: (tabId: string, clientX: number, clientY: number) => boolean;
  onTabDragEnd?: () => void;
}

export function PaneStrip(props: PaneStripProps) {
  // M3 sliding active-indicator (ported from TabBar). Painted as a 1px bar
  // scaled via transform (translateX + scaleX) — compositor-only, no width
  // paint (sidebar.css .df-tab-indicator).
  let barEl: HTMLDivElement | undefined;
  const [indicator, setIndicator] = createSignal({ left: 0, width: 0, ready: false });
  // Re-measure the sliding indicator on two triggers: (1) selection / tab-set
  // changes drive the M3 slide (the effect below); (2) tabResizeObs re-measures
  // on any real .df-tab pixel-size change — live titles, status dot toggling,
  // font load. RO fires ONLY on actual size changes and its callback reads
  // layout post-layout/pre-paint, so per-tick title updates that don't change
  // width cost nothing (setIndicator also dedupes identical values).
  const tabIdsKey = createMemo(() => props.tabs.map((s) => s.id).join("\u0000"));
  let indicatorRaf = 0;
  function measureTabIndicator(): void {
    const active = barEl?.querySelector<HTMLElement>('[data-active="true"]');
    if (active) {
      const left = active.offsetLeft, width = active.offsetWidth;
      setIndicator((p) => (p.ready && p.left === left && p.width === width ? p : { left, width, ready: true }));
    } else {
      setIndicator((p) => (p.ready ? { ...p, ready: false } : p));
    }
  }
  const tabResizeObs = new ResizeObserver(measureTabIndicator);
  createEffect(on([() => props.selectedTab, tabIdsKey], () => {
    cancelAnimationFrame(indicatorRaf);
    indicatorRaf = requestAnimationFrame(() => {
      indicatorRaf = 0;
      tabResizeObs.disconnect();
      const els = barEl?.querySelectorAll<HTMLElement>(".df-tab") ?? [];
      for (const el of els) tabResizeObs.observe(el);
      measureTabIndicator();
    });
  }));
  onCleanup(() => { cancelAnimationFrame(indicatorRaf); tabResizeObs.disconnect(); });

  // --- Drag-to-reorder within the strip (ported from TabBar) ---
  type TabRect = { left: number; width: number; center: number };
  type DragState = {
    id: string; fromIdx: number; toIdx: number; dx: number; slot: number; rects: TabRect[]; released: boolean;
  };
  const [drag, setDrag] = createSignal<DragState | null>(null);
  const SETTLE_MS = 160;

  function tabRects(): TabRect[] {
    const els = barEl?.querySelectorAll<HTMLElement>(".df-tab") ?? [];
    return Array.from(els).map((el) => ({
      left: el.offsetLeft, width: el.offsetWidth, center: el.offsetLeft + el.offsetWidth / 2,
    }));
  }
  function restingDx(d: DragState): number {
    if (d.toIdx === d.fromIdx) return 0;
    const r = d.rects;
    if (d.toIdx > d.fromIdx) return r[d.toIdx].left + r[d.toIdx].width - r[d.fromIdx].width - r[d.fromIdx].left;
    return r[d.toIdx].left - r[d.fromIdx].left;
  }
  // Window-listener drag (mirrors PaneDivider.tsx). Snapshot start point +
  // callbacks at pointerdown, then listen on WINDOW so a drag that leaves the
  // 40px-tall tab button keeps flowing: a straight-down split-drag exits the
  // button before any horizontal delta, and the old per-button handler + X-only
  // threshold dropped it (drag never armed). Arm on Euclidean distance so ANY
  // direction triggers. No setPointerCapture → survives a <For> node recreate
  // and stays synthetic-pointer testable (feedback_for_recreates_node_kills_pointer_capture).
  function onTabPointerDown(e: PointerEvent, id: string) {
    if (e.button !== 0) return;
    const idx = props.tabs.findIndex((t) => t.id === id);
    if (idx < 0) return;
    const start = { x: e.clientX, y: e.clientY, id, idx };
    const onDragMove = props.onTabDragMove;
    const onTileDrop = props.onTabTileDrop;
    const onDragEnd = props.onTabDragEnd;
    const onReorder = props.onReorder;

    const onMove = (ev: PointerEvent) => {
      const current = drag();
      if (!current) {
        if (!dragArmed(start, ev.clientX, ev.clientY)) return;
        const rects = tabRects();
        setDrag({ id: start.id, fromIdx: start.idx, toIdx: start.idx, dx: ev.clientX - start.x, slot: rects[start.idx].width + 2, rects, released: false });
        onDragMove?.(ev.clientX, ev.clientY);
        return;
      }
      const dx = ev.clientX - start.x;
      const center = current.rects[current.fromIdx].center + dx;
      let toIdx = current.fromIdx;
      while (toIdx < current.rects.length - 1 && center > current.rects[toIdx + 1].center) toIdx++;
      while (toIdx > 0 && center < current.rects[toIdx - 1].center) toIdx--;
      setDrag({ ...current, dx, toIdx });
      onDragMove?.(ev.clientX, ev.clientY);
    };
    const onUp = (ev: PointerEvent) => {
      teardown();
      const d = drag();
      if (!d) return; // not armed → onClick fires → selects

      // Swallow the trailing click so a drag (tile-drop or reorder) doesn't
      // also fire onClick→onSelect. One-shot capture-phase listener mirrors
      // fabDragOffset.ts:82-89 — fires before the button's onClick, cancels it.
      const swallowClick = () => {
        window.addEventListener("click",
          (ce) => { ce.stopPropagation(); ce.preventDefault(); },
          { capture: true, once: true });
      };

      // Drag-to-tile wins: if the drop lands on a pane edge / another pane, the
      // deck splits/merges and returns true → skip the in-strip reorder.
      const tiled = onTileDrop?.(d.id, ev.clientX, ev.clientY) ?? false;
      onDragEnd?.();
      if (tiled) { swallowClick(); setDrag(null); return; }

      // Reorder: settle animation → reorder → clear. Swallow click.
      if (d.toIdx !== d.fromIdx) {
        swallowClick();
        const { fromIdx, toIdx } = d;
        setDrag({ ...d, dx: restingDx(d), released: true });
        setTimeout(() => {
          const ids = props.tabs.map((t) => t.id);
          const [moved] = ids.splice(fromIdx, 1);
          ids.splice(toIdx, 0, moved);
          onReorder(ids);
          setDrag(null);
        }, SETTLE_MS);
        return;
      }

      // Armed but no reorder (touch jitter): clear immediately, let onClick
      // through to select. This was the double-tap-on-touch bug — the old
      // draggedRecently flag swallowed the click even when fromIdx===toIdx.
      setDrag(null);
    };
    const onCancel = () => { teardown(); setDrag(null); onDragEnd?.(); };
    function teardown() {
      window.removeEventListener("pointermove", onMove);
      window.removeEventListener("pointerup", onUp);
      window.removeEventListener("pointercancel", onCancel);
    }
    window.addEventListener("pointermove", onMove);
    window.addEventListener("pointerup", onUp);
    window.addEventListener("pointercancel", onCancel);
  }
  function tabDragStyle(i: number): Record<string, string> {
    const d = drag();
    if (!d) return {};
    if (i === d.fromIdx) {
      return { transform: `translateX(${d.dx}px)`, transition: d.released ? `transform ${SETTLE_MS}ms cubic-bezier(.2,0,0,1)` : "none", "z-index": "3" };
    }
    let shift = 0;
    if (d.fromIdx < d.toIdx && i > d.fromIdx && i <= d.toIdx) shift = -d.slot;
    else if (d.fromIdx > d.toIdx && i >= d.toIdx && i < d.fromIdx) shift = d.slot;
    return { transform: shift ? `translateX(${shift}px)` : "translateX(0)", transition: "transform 200ms cubic-bezier(.2,0,0,1)" };
  }

  return (
    <div
      class="df-tab-bar"
      data-testid={`pane-strip-${props.paneId}`}
      data-pane-strip={props.paneId}
      data-focused={props.focused ? "true" : "false"}
      data-dragging={drag() ? "true" : "false"}
      ref={barEl}
    >
      <div
        class="df-tab-indicator"
        aria-hidden="true"
        style={{
          transform: `translateX(${indicator().left}px) scaleX(${indicator().width})`,
          opacity: indicator().ready && !drag() ? "1" : "0",
        }}
      />
      <For each={props.tabs}>
        {(s, i) => {
          const isActive = createMemo(() => s.id === props.selectedTab);
          const level = createMemo(() => attentionOf(s));
          const vis = createMemo(() => presentationOf(level()));
          const showDot = createMemo(() => isActionable(level()));
          const isClaude = createMemo(() => isClaudeSession(s));
          const label = createMemo(() => sessionTitle(s));
          return (
            <button
              type="button"
              class="df-tab"
              data-testid={`tab-${s.id}`}
              data-active={isActive() ? "true" : "false"}
              data-dragging={drag()?.id === s.id ? "true" : "false"}
              style={tabDragStyle(i())}
              onPointerDown={(e) => onTabPointerDown(e, s.id)}
              onClick={() => props.onSelect(s.id)}
              title={label()}
            >
              <md-ripple />
              <Show when={showDot()}>
                <span class="df-tab-dot" style={{ background: vis().color }} title={vis().label} aria-label={vis().label} />
              </Show>
              <span class="df-tab-glyph" data-claude={isClaude() ? "1" : "0"}>
                {isClaude() ? <ClaudeMark /> : "$"}
              </span>
              <span class="df-tab-label">{label()}</span>
              <IconButton
                icon="close"
                label="Close terminal"
                class="df-tab-close"
                data-testid={`tab-close-${s.id}`}
                style={{ "--md-icon-button-icon-size": "14px" }}
                onClick={(e: MouseEvent) => { e.stopPropagation(); e.preventDefault(); props.onClose(s); }}
              />
            </button>
          );
        }}
      </For>
      <button
        type="button"
        class="df-tab-new"
        data-testid="tab-new"
        aria-label="New terminal — same folder & server"
        title="New terminal in this folder (or double-click the empty bar)"
        onClick={() => props.onNewTab()}
      >
        <md-ripple />
        <svg width="16" height="16" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2.5" stroke-linecap="round">
          <path d="M12 5v14M5 12h14" />
        </svg>
      </button>
      <div
        class="df-tab-filler"
        data-testid="tab-filler"
        title="Double-click to open a new terminal in this folder"
        onDblClick={() => props.onNewTab()}
      />
    </div>
  );
}

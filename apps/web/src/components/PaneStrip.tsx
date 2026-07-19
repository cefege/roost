// Per-pane tab strip (tabs-in-panes). One strip per layout pane: renders
// that pane's tabs, click selects, drag reorders within the strip, ✕ closes,
// + spawns a sibling. Data + actions come from props (the layout store drives
// it) — extracted from the old URL-scoped TabBar so the machinery isn't forked.
// The M3 sliding indicator + pointer drag-reorder are ported verbatim.
// Callers: MainPane.tsx TerminalDeck (one per pane). Drag-to-EDGE (split) is
// added in P2 on top of this reorder.

import { For, createMemo, createSignal, createEffect, on, onCleanup, onMount, Show, type JSX } from "solid-js";
import { Portal } from "solid-js/web";
import { ClaudeMark } from "./sidebar/StatusGlyph.tsx";
import { isClaudeSession } from "../lib/isClaudeSession.ts";
import { sessionTitle, cloudSubtitle } from "../lib/sessionTitle.ts";
import { shortCwd } from "../lib/sidebarFormat.ts";
import { IconButton } from "./Settings/md/IconButton.tsx";
import { attentionOf, presentationOf, isActionable } from "../lib/agentStatus.ts";
import { dragArmed } from "../lib/dragThreshold.ts";
import { animateSpring, SPRING_SNAP } from "../lib/spring.ts";
import { prefersReducedMotion } from "../lib/prefersReducedMotion.ts";
import { isCompact, isTouchDevice } from "../lib/windowSizeClass.ts";
import { renderPreview } from "../lib/terminalPreview.ts";
import { formatCostUsd } from "./sidebar/CostChip.tsx";
import { ctxMenuSurfaceStyle } from "./contextMenuPrimitives.tsx";
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
  // Overflow (Chrome tab-list chevron): true when the strip's tabs exceed its
  // width so the trailing chevron + filterable all-tabs popup appear. Measured
  // off the same rAF as the indicator (post-layout) plus a barEl ResizeObserver
  // for pane resizes that don't change tab count.
  const [overflow, setOverflow] = createSignal(false);
  function measureOverflow(): void {
    if (!barEl) return;
    const over = barEl.scrollWidth > barEl.clientWidth + 1;
    setOverflow((p) => (p === over ? p : over));
  }
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
    measureOverflow();
  }
  const tabResizeObs = new ResizeObserver(measureTabIndicator);
  // A dedicated RO on the bar itself: pane resize changes clientWidth without a
  // tab-set/selection change, so tabResizeObs (tabs only) wouldn't refire.
  const barResizeObs = new ResizeObserver(measureOverflow);
  onMount(() => { if (barEl) barResizeObs.observe(barEl); measureOverflow(); });
  onCleanup(() => barResizeObs.disconnect());
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
  onCleanup(() => { cancelAnimationFrame(indicatorRaf); tabResizeObs.disconnect(); cancelSettle?.(); });

  // --- Drag-to-reorder within the strip (ported from TabBar) ---
  type TabRect = { left: number; width: number; center: number };
  type DragState = {
    id: string; fromIdx: number; toIdx: number; dx: number; slot: number; rects: TabRect[]; released: boolean;
  };
  const [drag, setDrag] = createSignal<DragState | null>(null);
  // Tabs mid close-collapse (Chrome-style width→0 exit); onClose fires after.
  const [closing, setClosing] = createSignal<Set<string>>(new Set());
  // Cancel handle for the in-flight spring reorder settle (rAF driver).
  let cancelSettle: (() => void) | undefined;

  // --- Overflow tab-list popup + desktop hover cards ---
  // Popup: anchored below the chevron (right-aligned, like ArrangeMenu).
  let overflowBtnEl: HTMLButtonElement | undefined;
  const [listOpen, setListOpen] = createSignal<{ right: number; y: number } | null>(null);
  const toggleList = () => {
    if (listOpen()) { setListOpen(null); return; }
    clearHover(); // mutual exclusion: a hover card and the popup never coexist
    const r = overflowBtnEl!.getBoundingClientRect();
    setListOpen({ right: Math.max(6, window.innerWidth - r.right), y: r.bottom + 4 });
  };
  // Hover card: desktop-only, ~450ms dwell. Inert on touch/compact, while
  // dragging, or while the popup is open.
  const [hover, setHover] = createSignal<{ id: string; rect: DOMRect } | null>(null);
  let hoverTimer = 0;
  const hoverEnabled = () => !isCompact() && !isTouchDevice();
  function clearHover(): void {
    if (hoverTimer) { clearTimeout(hoverTimer); hoverTimer = 0; }
    setHover((p) => (p ? null : p));
  }
  function armHover(id: string, rect: DOMRect): void {
    if (!hoverEnabled() || drag() || listOpen()) return;
    clearHover();
    hoverTimer = window.setTimeout(() => setHover({ id, rect }), 450);
  }
  onCleanup(() => clearTimeout(hoverTimer));

  // Chrome-style close: collapse the tab's width (siblings reflow) then commit.
  // Reduced-motion or a double-fire → close immediately.
  function closeTab(s: Session): void {
    if (prefersReducedMotion() || closing().has(s.id)) { props.onClose(s); return; }
    setClosing((prev) => new Set(prev).add(s.id));
    window.setTimeout(() => {
      props.onClose(s);
      setClosing((prev) => { const n = new Set(prev); n.delete(s.id); return n; });
    }, 220); // ≈ --md-sys-motion-duration-short4 (200ms) + slack
  }

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
    cancelSettle?.(); // a new grab interrupts any in-flight reorder settle
    clearHover();
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

      // Reorder: spring the grabbed tab to its resting slot (Chrome tab-drop
      // feel), then commit the order and clear. Swallow the trailing click.
      if (d.toIdx !== d.fromIdx) {
        swallowClick();
        const { fromIdx, toIdx } = d;
        const commit = (): void => {
          const ids = props.tabs.map((t) => t.id);
          const [moved] = ids.splice(fromIdx, 1);
          ids.splice(toIdx, 0, moved);
          onReorder(ids);
          setDrag(null);
        };
        const rest = restingDx(d);
        if (prefersReducedMotion()) { commit(); return; }
        setDrag({ ...d, released: true });
        cancelSettle = animateSpring(
          { position: d.dx, velocity: 0 }, rest, SPRING_SNAP,
          (pos) => setDrag((cur) => (cur ? { ...cur, dx: pos, released: true } : null)),
          commit,
        );
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
      // Grabbed tab: spring drives dx per frame on release (transition none);
      // no transition while actively dragged either.
      return { transform: `translateX(${d.dx}px)`, transition: "none", "z-index": "3" };
    }
    let shift = 0;
    if (d.fromIdx < d.toIdx && i > d.fromIdx && i <= d.toIdx) shift = -d.slot;
    else if (d.fromIdx > d.toIdx && i >= d.toIdx && i < d.fromIdx) shift = d.slot;
    return { transform: shift ? `translateX(${shift}px)` : "translateX(0)", transition: "transform var(--md-sys-motion-duration-short4) var(--md-sys-motion-easing-emphasized)" };
  }
  // Filterable all-tabs popup (Chrome's overflow tab-list). Anchored below the
  // chevron; doc-click / Escape dismiss (deterministic, like ArrangeMenu).
  function PaneTabList(p: { pos: { right: number; y: number }; onClose: () => void }) {
    const [filter, setFilter] = createSignal("");
    const [hi, setHi] = createSignal(0);
    let menuEl: HTMLDivElement | undefined;
    let inputEl: HTMLInputElement | undefined;
    const matches = createMemo(() =>
      props.tabs.filter((s) => sessionTitle(s).toLowerCase().includes(filter().trim().toLowerCase())),
    );
    const choose = (s: Session) => {
      p.onClose();
      props.onSelect(s.id);
      queueMicrotask(() =>
        barEl?.querySelector(`[data-testid="tab-${s.id}"]`)?.scrollIntoView({ inline: "nearest", block: "nearest" }),
      );
    };
    const onKey = (e: KeyboardEvent) => {
      if (e.key === "Escape") { e.preventDefault(); p.onClose(); return; }
      const list = matches();
      if (e.key === "ArrowDown") { e.preventDefault(); setHi((i) => Math.min(list.length - 1, i + 1)); }
      else if (e.key === "ArrowUp") { e.preventDefault(); setHi((i) => Math.max(0, i - 1)); }
      else if (e.key === "Enter") { e.preventDefault(); const s = list[hi()]; if (s) choose(s); }
    };
    const onDocClick = (e: MouseEvent) => {
      const t = e.target as Node;
      if (overflowBtnEl?.contains(t) || menuEl?.contains(t)) return;
      p.onClose();
    };
    onMount(() => {
      inputEl?.focus();
      document.addEventListener("click", onDocClick);
      document.addEventListener("keydown", onKey);
      onCleanup(() => {
        document.removeEventListener("click", onDocClick);
        document.removeEventListener("keydown", onKey);
      });
    });
    const surfaceStyle = (): JSX.CSSProperties => {
      const s: JSX.CSSProperties = {
        ...ctxMenuSurfaceStyle(0, p.pos.y),
        "min-width": "240px", "max-width": "340px", right: `${p.pos.right}px`,
        padding: "0", overflow: "hidden", display: "flex", "flex-direction": "column",
      };
      delete s.left;
      return s;
    };
    return (
      <Portal>
        <div ref={menuEl} data-testid="tab-list-popup" class="df-menu-enter" style={surfaceStyle()}>
          <div style={{ display: "flex", "align-items": "center", gap: "8px", padding: "8px 12px", "border-bottom": "1px solid var(--md-sys-color-outline-variant)" }}>
            <input
              ref={inputEl}
              type="text"
              value={filter()}
              onInput={(e) => { setFilter(e.currentTarget.value); setHi(0); }}
              placeholder="Filter terminals…"
              data-testid="tab-list-filter"
              style={{ flex: "1", background: "transparent", border: "none", outline: "none", color: "var(--text-hi)", "font-size": "var(--md-body-s-size)" }}
            />
          </div>
          <div style={{ "max-height": "50vh", "overflow-y": "auto", padding: "4px" }}>
            <Show
              when={matches().length > 0}
              fallback={<div style={{ padding: "16px 12px", "text-align": "center", color: "var(--text-lo)", "font-size": "var(--md-body-s-size)" }}>No matches</div>}
            >
              <For each={matches()}>
                {(s, i) => {
                  const level = createMemo(() => attentionOf(s));
                  const vis = createMemo(() => presentationOf(level()));
                  const showDot = createMemo(() => isActionable(level()));
                  const isClaude = createMemo(() => isClaudeSession(s));
                  return (
                    <button
                      type="button"
                      data-testid={`tab-list-item-${s.id}`}
                      onMouseEnter={() => setHi(i())}
                      onClick={() => choose(s)}
                      style={{
                        width: "100%", display: "flex", "align-items": "center", gap: "8px",
                        padding: "6px 8px", border: "none", cursor: "pointer",
                        "border-radius": "var(--md-shape-xs)", "text-align": "left",
                        background: hi() === i() ? "var(--md-state-hover)" : "transparent",
                        color: "var(--text-hi)", "font-size": "var(--md-body-s-size)",
                      }}
                    >
                      <span style={{ width: "7px", height: "7px", "border-radius": "50%", "flex-shrink": "0", background: showDot() ? vis().color : "transparent" }} />
                      <span class="df-tab-glyph" data-claude={isClaude() ? "1" : "0"}>{isClaude() ? <ClaudeMark /> : "$"}</span>
                      <span style={{ flex: "1", overflow: "hidden", "text-overflow": "ellipsis", "white-space": "nowrap", "min-width": "0" }}>{sessionTitle(s)}</span>
                      <Show when={s.id === props.selectedTab}>
                        <span style={{ color: "var(--md-sys-color-primary)", "flex-shrink": "0" }}>✓</span>
                      </Show>
                    </button>
                  );
                }}
              </For>
            </Show>
          </div>
        </div>
      </Portal>
    );
  }

  // Desktop hover card (Chrome tab hover-card): title + process line + cwd +
  // model/cost + live preview thumbnail. Fixed-positioned via Portal.
  function TabHoverCard(p: { s: Session; rect: DOMRect }) {
    let previewRef: HTMLDivElement | undefined;
    const [hasPreview, setHasPreview] = createSignal(false);
    onMount(() => { if (previewRef) setHasPreview(renderPreview(p.s.id, previewRef)); });
    const level = createMemo(() => attentionOf(p.s));
    const vis = createMemo(() => presentationOf(level()));
    const showChip = createMemo(() => isActionable(level()));
    const isClaude = createMemo(() => isClaudeSession(p.s));
    const subtitle = createMemo(() => cloudSubtitle(p.s));
    const model = createMemo(() => { const m = p.s.agent?.model?.trim(); return m ? m : null; });
    const cost = createMemo(() => formatCostUsd(p.s.agent?.cost_usd));
    const left = Math.max(8, Math.min(p.rect.left, window.innerWidth - 280 - 8));
    return (
      <Portal>
        <div class="df-tab-hovercard" data-testid="tab-hovercard" style={{ left: `${left}px`, top: `${p.rect.bottom + 4}px` }}>
          <div class="df-tab-hovercard-head">
            <span class="df-tab-glyph" data-claude={isClaude() ? "1" : "0"}>{isClaude() ? <ClaudeMark /> : "$"}</span>
            <span class="df-tab-hovercard-title">{sessionTitle(p.s)}</span>
            <Show when={showChip()}>
              <span class="df-tab-hovercard-chip"><span class="df-tab-dot" style={{ background: vis().color }} />{vis().label}</span>
            </Show>
          </div>
          <Show when={subtitle()}>
            <div class="df-tab-hovercard-line">{subtitle()}</div>
          </Show>
          <div class="df-tab-hovercard-cwd">{shortCwd(p.s.cwd)}</div>
          <Show when={isClaude() && (model() || cost())}>
            <div class="df-tab-hovercard-meta">
              <Show when={model()}><span>{model()}</span></Show>
              <Show when={cost()}><span>{cost()}</span></Show>
            </div>
          </Show>
          <div class="df-tab-hovercard-preview" style={{ display: hasPreview() ? "block" : "none" }}>
            <div ref={previewRef} class="terminal-card-preview-text" />
          </div>
        </div>
      </Portal>
    );
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
              data-closing={closing().has(s.id) ? "true" : "false"}
              style={tabDragStyle(i())}
              onPointerDown={(e) => onTabPointerDown(e, s.id)}
              onClick={() => { clearHover(); props.onSelect(s.id); }}
              onMouseEnter={(e) => armHover(s.id, e.currentTarget.getBoundingClientRect())}
              onMouseLeave={clearHover}
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
                onClick={(e: MouseEvent) => { e.stopPropagation(); e.preventDefault(); closeTab(s); }}
              />
            </button>
          );
        }}
      </For>
      <Show when={overflow()}>
        <button
          ref={overflowBtnEl}
          type="button"
          class="df-tab-overflow"
          data-testid="tab-overflow"
          aria-label="All terminals in this pane"
          title="All terminals"
          onClick={toggleList}
        >
          <md-ripple />
          <svg width="16" height="16" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2.5" stroke-linecap="round" stroke-linejoin="round">
            <path d="M6 9l6 6 6-6" />
          </svg>
        </button>
      </Show>
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
      <Show when={listOpen()}>
        {(pos) => <PaneTabList pos={pos()} onClose={() => setListOpen(null)} />}
      </Show>
      <Show when={hover()}>
        {(h) => {
          const s = createMemo(() => props.tabs.find((t) => t.id === h().id));
          return <Show when={s()}>{(sess) => <TabHoverCard s={sess()} rect={h().rect} />}</Show>;
        }}
      </Show>
    </div>
  );
}

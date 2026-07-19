// Mobile (compact) workspace bar — Chrome-style replacement for the old
// scrollable MobileTabStrip. ONE 48px row, left→right:
//   [menu] [current-tab title] [+] [count ▢]
// `+` spawns a sibling into the focused pane (unchanged). The count square
// opens WorkspaceTabsSheet: a full-screen card grid of the folder's
// terminals, mirroring the home page's FolderCard grid — tap a card to
// switch, ✕ to close. The count square also rolls every tab's attention
// level (rollupLevels) so a needs-input terminal lights it up amber + pulses
// — the spine ("needs-input must never be hidden behind a drawer") survives
// the strip removal.
//
// Rendered by TerminalDeck at the top of the deck when isCompact(). Same
// props contract the old MobileTabStrip had; select/close/spawn reuse the
// deck's doSelect/doClose/doNewTab.

import { For, Show, createMemo, createSignal, onCleanup, onMount } from "solid-js";
import { Portal } from "solid-js/web";
import { ClaudeMark } from "./sidebar/StatusGlyph.tsx";
import { isClaudeSession } from "../lib/isClaudeSession.ts";
import { sessionTitle, cloudSubtitle } from "../lib/sessionTitle.ts";
import { IconButton } from "./Settings/md/IconButton.tsx";
import { attentionOf, presentationOf, rollupLevels } from "../lib/agentStatus.ts";
import { openSidebar } from "../store/uiStore.ts";
import { relTimeSince } from "../lib/relTime.ts";
import { relTimeTickMs } from "./sidebar/SessionRow.tsx";
import type { Session } from "@roost/shared/wire";
import { NotificationBellTrigger } from "./NotificationBell.tsx";
import { renderPreview } from "../lib/terminalPreview.ts";
import { ctxMenuSurfaceStyle, CtxMenuItem } from "./contextMenuPrimitives.tsx";

const TITLE_TEXT: Record<string, string> = {
  "font-size": "14px",
  "font-weight": "600",
  overflow: "hidden",
  "text-overflow": "ellipsis",
  "white-space": "nowrap",
  "line-height": "48px",
};

export interface MobileDeckBarProps {
  /** Flattened, ordered sessions in the folder (all panes, leaf-then-tab order). */
  tabs: Session[];
  /** The session id currently painted full-bleed (URL-active). */
  selectedTab: string;
  onSelect: (id: string) => void;
  onClose: (s: Session) => void;
  onNewTab: () => void;
}

export function MobileDeckBar(props: MobileDeckBarProps) {
  const [sheetOpen, setSheetOpen] = createSignal(false);
  const active = createMemo(() => props.tabs.find((t) => t.id === props.selectedTab) ?? null);
  const title = createMemo(() => {
    const s = active();
    return s ? sessionTitle(s) : "Terminal";
  });
  // Aggregate attention across the folder so a needs-input terminal still
  // flags the count square — the old strip surfaced this per-tab dot.
  const attention = createMemo(() => rollupLevels(props.tabs.map(attentionOf)));
  const countLabel = () => `${props.tabs.length} terminal${props.tabs.length === 1 ? "" : "s"} in this workspace`;

  return (
    <>
      <div
        class="mobile-deck-bar"
        data-testid="mobile-deck-bar"
        data-attention={attention()}
        style={{
          display: "flex",
          "align-items": "center",
          gap: "8px",
          height: "48px",
          "flex-shrink": "0",
          padding: "0 8px",
          background: "var(--surface-1)",
          "border-bottom": "1px solid var(--border-subtle)",
          color: "var(--text-hi)",
          "touch-action": "pan-y",
        }}
      >
        <IconButton
          icon="menu"
          label="Open sidebar"
          data-testid="mobile-deck-bar-menu"
          onClick={openSidebar}
          style={{ "flex-shrink": "0" }}
        />
        <div style={{ flex: "1 1 0", "min-width": "0", position: "relative", overflow: "hidden", height: "48px" }}>
          <span title={title()} style={{ ...TITLE_TEXT, display: "block", width: "100%" }}>
            {title()}
          </span>
        </div>

        {/* New terminal — same folder & server (unchanged behavior). */}
        <button
          type="button"
          class="mobile-deck-new"
          data-testid="tab-new"
          aria-label="New terminal — same folder & server"
          title="New terminal in this folder"
          onClick={() => props.onNewTab()}
        >
          <svg width="18" height="18" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2.5" stroke-linecap="round">
            <path d="M12 5v14M5 12h14" />
          </svg>
        </button>

        {/* Count square — Chrome's tab-grid button. Opens the terminal grid. */}
        <button
          type="button"
          class="mobile-deck-count"
          data-testid="mobile-tab-count"
          data-attention={attention()}
          aria-label={`Open terminal grid — ${countLabel()}`}
          title={countLabel()}
          onClick={() => setSheetOpen(true)}
        >
          <span>{props.tabs.length}</span>
        </button>
        <NotificationBellTrigger style={{ "flex-shrink": "0" }} />
      </div>

      <Show when={sheetOpen()}>
        <WorkspaceTabsSheet
          tabs={props.tabs}
          selectedTab={props.selectedTab}
          onSelect={props.onSelect}
          onClose={props.onClose}
          onNewTab={props.onNewTab}
          onCloseSheet={() => setSheetOpen(false)}
        />
      </Show>
    </>
  );
}

// ── Full-screen terminal card grid (Chrome tab grid spec) ───────────────
// Two vertical zones: a single top toolbar (56px) + a scrollable 2-column
// card grid. Normal mode: [back] [+ new] [N terminals] [⋮]. Selection mode:
// [✕ exit] [N selected] [⋮], cards toggle selection (ring + check). The ⋮
// overflow menu offers Close all / Select tabs (normal) or Select all /
// Close selected (selection). Card structure mirrors Chrome's
// tab_grid_card_item_layout: 40px header (favicon + title + close ✕) over a
// faux-terminal preview area with asymmetric corner radius (12px top / 20px
// bottom — Chrome's signature thumbnail shape).

interface WorkspaceTabsSheetProps {
  tabs: Session[];
  selectedTab: string;
  onSelect: (id: string) => void;
  onClose: (s: Session) => void;
  onNewTab: () => void;
  onCloseSheet: () => void;
}

function WorkspaceTabsSheet(props: WorkspaceTabsSheetProps) {
  const [selectionMode, setSelectionMode] = createSignal(false);
  const [selectedIds, setSelectedIds] = createSignal<string[]>([]);
  const isSelected = (id: string) => selectedIds().includes(id);
  const toggleSelect = (id: string) =>
    setSelectedIds((p) => (p.includes(id) ? p.filter((x) => x !== id) : [...p, id]));
  const enterSelection = () => { setSelectedIds([]); setSelectionMode(true); };
  const exitSelection = () => { setSelectionMode(false); setSelectedIds([]); };
  const selectAll = () => setSelectedIds(props.tabs.map((t) => t.id));
  // Snapshot BEFORE closing — props.tabs is reactive (mobileTabs()) and shrinks
  // as each onClose commits. onClose is the existing soft-close (closeSessionOp):
  // each schedules its own independent undo snackbar, matching Chrome's undo.
  const closeAll = () => {
    const snap = [...props.tabs];
    snap.forEach((s) => props.onClose(s));
    props.onCloseSheet();
  };
  const closeSelected = () => {
    const ids = new Set(selectedIds());
    const snap = props.tabs.filter((t) => ids.has(t.id));
    exitSelection();
    snap.forEach((s) => props.onClose(s));
  };

  return (
    <Portal mount={document.body}>
      <div
        class="workspace-tabs-sheet"
        data-testid="workspace-tabs-sheet"
        style={{
          position: "fixed",
          inset: "0",
          "z-index": "60",
          background: "var(--md-surface-container-lowest)",
          display: "flex",
          "flex-direction": "column",
          "padding-top": "env(safe-area-inset-top, 0px)",
        }}
      >
        {/* Top toolbar — Chrome tab_grid_dialog_toolbar (56dp). */}
        <div
          class="workspace-tabs-head"
          style={{
            display: "flex",
            "align-items": "center",
            gap: "8px",
            height: "56px",
            "flex-shrink": "0",
            padding: "0 8px",
            "border-bottom": "1px solid var(--border-subtle)",
            color: "var(--text-hi)",
          }}
        >
          <Show when={!selectionMode()}>
            <IconButton
              icon="arrow_back"
              label="Close terminal grid"
              data-testid="workspace-tabs-back"
              onClick={props.onCloseSheet}
            />
            <button
              type="button"
              class="mobile-deck-new"
              data-testid="workspace-tabs-new"
              aria-label="New terminal"
              title="New terminal in this folder"
              onClick={() => props.onNewTab()}
            >
              <svg width="18" height="18" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2.5" stroke-linecap="round">
                <path d="M12 5v14M5 12h14" />
              </svg>
            </button>
          </Show>
          <Show when={selectionMode()}>
            <IconButton
              icon="close"
              label="Exit selection"
              data-testid="workspace-tabs-selection-exit"
              onClick={exitSelection}
            />
          </Show>
          <span style={{ flex: "1 1 0", "font-size": "16px", "font-weight": "500" }}>
            {selectionMode()
              ? `${selectedIds().length} selected`
              : `${props.tabs.length} terminal${props.tabs.length === 1 ? "" : "s"}`}
          </span>
          <WorkspaceTabsMenu
            selectionMode={selectionMode()}
            onCloseAll={closeAll}
            onSelectTabs={enterSelection}
            onSelectAll={selectAll}
            onCloseSelected={closeSelected}
          />
        </div>

        {/* Card grid — scrollable middle zone. */}
        <Show
          when={props.tabs.length > 0}
          fallback={
            <div
              class="home-landing-empty"
              data-testid="workspace-tabs-empty"
              style={{ "padding-top": "64px" }}
            >
              <div class="home-landing-empty-title">No terminals</div>
              <div class="home-landing-empty-sub">Open one with the + above.</div>
            </div>
          }
        >
          <div class="workspace-tabs-grid" style={{ padding: "16px", "overflow-y": "auto", flex: "1 1 0" }}>
            <For each={props.tabs}>
              {(s) => (
                <div class="terminal-card-wrap">
                  <TerminalCard
                    session={s}
                    active={s.id === props.selectedTab}
                    onSelect={props.onSelect}
                    onClose={props.onClose}
                    onCloseSheet={props.onCloseSheet}
                    selectionMode={selectionMode()}
                    selected={isSelected(s.id)}
                    onToggleSelect={toggleSelect}
                  />
                </div>
              )}
            </For>
          </div>
        </Show>

      </div>
    </Portal>
  );
}

// Right-anchored overflow menu for the tab grid. Mirrors ArrangeMenu's
// toggle/anchor/doc-click/Escape dismissal. zIndex 70 clears the sheet (60).
function WorkspaceTabsMenu(props: {
  selectionMode: boolean;
  onCloseAll: () => void;
  onSelectTabs: () => void;
  onSelectAll: () => void;
  onCloseSelected: () => void;
}) {
  const [open, setOpen] = createSignal<{ right: number; y: number } | null>(null);
  let btnEl: HTMLButtonElement | undefined;
  let menuEl: HTMLDivElement | undefined;

  const toggle = () => {
    if (open()) {
      setOpen(null);
      return;
    }
    const r = btnEl!.getBoundingClientRect();
    setOpen({ right: Math.max(6, window.innerWidth - r.right), y: r.bottom + 4 });
  };

  const surfaceStyle = (right: number, y: number) => {
    const s = { ...ctxMenuSurfaceStyle(0, y, 70), "min-width": "200px", right: `${right}px` };
    delete s.left;
    return s;
  };

  const choose = (fn: () => void) => { setOpen(null); fn(); };

  const onDocClick = (e: MouseEvent) => {
    const t = e.target as Node;
    if (btnEl?.contains(t) || menuEl?.contains(t)) return;
    setOpen(null);
  };
  const onEsc = (e: KeyboardEvent) => { if (e.key === "Escape") setOpen(null); };

  onMount(() => {
    document.addEventListener("click", onDocClick);
    document.addEventListener("keydown", onEsc);
    onCleanup(() => {
      document.removeEventListener("click", onDocClick);
      document.removeEventListener("keydown", onEsc);
    });
  });

  return (
    <>
      <IconButton
        ref={btnEl}
        icon="more_vert"
        label="More options"
        data-testid="workspace-tabs-menu"
        onClick={toggle}
      />
      <Show when={open()}>
        {(pos) => (
          <Portal>
            <div
              ref={menuEl}
              data-testid="workspace-tabs-menu-popup"
              class="df-menu-enter"
              style={surfaceStyle(pos().right, pos().y)}
            >
              <Show when={!props.selectionMode}>
                <CtxMenuItem testid="workspace-tabs-close-all" danger onClick={() => choose(props.onCloseAll)}>
                  Close all tabs
                </CtxMenuItem>
                <CtxMenuItem testid="workspace-tabs-select" onClick={() => choose(props.onSelectTabs)}>
                  Select tabs
                </CtxMenuItem>
              </Show>
              <Show when={props.selectionMode}>
                <CtxMenuItem testid="workspace-tabs-select-all" onClick={() => choose(props.onSelectAll)}>
                  Select all
                </CtxMenuItem>
                <CtxMenuItem testid="workspace-tabs-close-selected" danger onClick={() => choose(props.onCloseSelected)}>
                  Close selected tabs
                </CtxMenuItem>
              </Show>
            </div>
          </Portal>
        )}
      </Show>
    </>
  );
}
// One terminal card — Chrome tab_grid_card_item_layout adapted for terminals.
// Compact header (favicon + title + truncated subtitle) over a terminal
// preview area: a real low-quality canvas screenshot of the terminal's
// current viewport, or a faux glyph fallback if no frame is available yet.
// Asymmetric corners (12px top / 20px bottom). Swipe left/right past 144px
// to close (Chrome's swipe_to_dismiss_threshold), with slight tilt.
function TerminalCard(props: {
  session: Session;
  active: boolean;
  onSelect: (id: string) => void;
  onClose: (s: Session) => void;
  onCloseSheet: () => void;
  selectionMode: boolean;
  selected: boolean;
  onToggleSelect: (id: string) => void;
}) {
  const s = () => props.session;
  const level = createMemo(() => attentionOf(s()));
  const vis = createMemo(() => presentationOf(level()));
  const isClaude = createMemo(() => isClaudeSession(s()));
  const name = createMemo(() => sessionTitle(s()));
  const sub = createMemo(() => cloudSubtitle(s()));
  const branch = () => s().git_branch ?? null;
  // data-stage reuses the FolderCard needs-input accent hook.
  const stage = () =>
    level() === "blocked" ? "needs-input" : level() === "working" ? "running" : "";
  // Compact header subtitle: branch if available, else cloud subtitle.
  const subtitle = () => branch() ?? sub();

  // ── Terminal preview render ───────────────────────────────────────────
  let previewRef: HTMLDivElement | undefined;
  const [hasPreview, setHasPreview] = createSignal(false);
  onMount(() => {
    if (previewRef) setHasPreview(renderPreview(s().id, previewRef));
  });

  // ── Swipe-to-close (touch only) ───────────────────────────────────────
  // Chrome's swipe_to_dismiss: drag card past halfway off-screen → close.
  // Card slides straight left/right — NO rotation (Chrome doesn't tilt).
  // Below threshold → spring back. Vertical movement → let the grid scroll.
  const [swipeX, setSwipeX] = createSignal(0);
  const [swiping, setSwiping] = createSignal(false);
  let _touchStartX = 0;
  let _touchStartY = 0;
  let _swipeAxis: "none" | "x" | "y" = "none";
  let _swiped = false;

  function onTouchStart(e: TouchEvent) {
    if (props.selectionMode) return;
    const t = e.touches[0];
    if (!t) return;
    _touchStartX = t.clientX;
    _touchStartY = t.clientY;
    _swipeAxis = "none";
    _swiped = false;
    setSwiping(true);
  }
  function onTouchMove(e: TouchEvent) {
    const t = e.touches[0];
    if (!t) return;
    const dx = t.clientX - _touchStartX;
    const dy = t.clientY - _touchStartY;
    if (_swipeAxis === "none") {
      if (Math.abs(dx) < 10 && Math.abs(dy) < 10) return;
      _swipeAxis = Math.abs(dx) > Math.abs(dy) * 1.5 ? "x" : "y";
    }
    if (_swipeAxis !== "x") return; // vertical → let the grid scroll
    e.preventDefault();
    setSwipeX(dx);
    if (Math.abs(dx) > 24) _swiped = true;
  }
  function onTouchEnd() {
    setSwiping(false);
    const threshold = window.innerWidth * 0.5;
    if (Math.abs(swipeX()) >= threshold) {
      const off = swipeX() > 0 ? window.innerWidth : -window.innerWidth;
      setSwipeX(off);
      setTimeout(() => props.onClose(s()), 180);
    } else {
      setSwipeX(0);
      _swiped = false; // short drag that didn't dismiss → don't block next tap
    }
  }

  function activate() {
    if (props.selectionMode) { props.onToggleSelect(s().id); return; }
    if (_swiped) { _swiped = false; return; }
    props.onSelect(s().id);
    props.onCloseSheet();
  }

  return (
    <div
      class="terminal-card"
      data-testid={`terminal-card-${s().id}`}
      data-stage={stage()}
      data-active={props.active ? "true" : "false"}
      data-selected={props.selected ? "true" : "false"}
      data-attention={level()}
      role="button"
      tabindex="0"
      title={name()}
      style={{
        transform: `translateX(${swipeX()}px)`,
        transition: swiping()
          ? "none"
          : "transform var(--md-sys-motion-duration-short4, 200ms) var(--md-sys-motion-easing-emphasized-decelerate, cubic-bezier(0.05, 0.7, 0.1, 1))",
      }}
      onClick={activate}
      onTouchStart={onTouchStart}
      onTouchMove={onTouchMove}
      onTouchEnd={onTouchEnd}
      onKeyDown={(e: KeyboardEvent) => {
        if (e.key === "Enter" || e.key === " ") {
          e.preventDefault();
          activate();
        }
      }}
    >
      {/* Header — favicon + title + compact subtitle. Close ✕ overlaps top-right. */}
      <div class="terminal-card-header">
        <span class="terminal-card-favicon" style={{ color: vis().color }}>
          {isClaude() ? <ClaudeMark /> : <span class="terminal-card-glyph">$</span>}
        </span>
        <div class="terminal-card-title-wrap">
          <span class="terminal-card-title">{name()}</span>
          <Show when={subtitle()}>
            <span class="terminal-card-subtitle">{subtitle()}</span>
          </Show>
        </div>
      </div>

      {/* Close ✕ — 48px touch target, 18px visible icon, top-right corner. */}
      <Show when={!props.selectionMode}>
        <button
          type="button"
          class="terminal-card-close"
          data-testid={`terminal-card-close-${s().id}`}
          aria-label="Close terminal"
          onClick={(e: MouseEvent) => {
            e.stopPropagation();
            e.preventDefault();
            props.onClose(s());
          }}
        >
          <svg width="18" height="18" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" aria-hidden="true">
            <path d="M18 6 6 18M6 6l12 12" />
          </svg>
        </button>
      </Show>

      {/* Selection check — filled tint when selected, top-right corner. */}
      <Show when={props.selectionMode}>
        <span
          class="terminal-card-check"
          data-testid={`terminal-card-check-${s().id}`}
          data-checked={props.selected ? "true" : "false"}
        >
          <svg width="14" height="14" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="3" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true">
            <path d="M20 6 9 17l-5-5" />
          </svg>
        </span>
      </Show>

      {/* Preview area — real terminal text or faux glyph fallback.
          Asymmetric corners (12/20px). Fixed 160px height = uniform cards. */}
      <div class="terminal-card-preview" data-stage={stage()}>
        <div
          ref={previewRef}
          class="terminal-card-preview-text"
          style={{ display: hasPreview() ? "block" : "none" }}
        />
        <Show when={!hasPreview()}>
          <span class="terminal-card-preview-glyph" style={{ color: vis().color }}>
            {isClaude() ? <ClaudeMark /> : <span class="terminal-card-glyph">$</span>}
          </span>
        </Show>
      </div>
    </div>
  );
}

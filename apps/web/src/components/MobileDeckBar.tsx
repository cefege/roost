// Mobile (compact) workspace bar. ONE 48px row, left→right:
//   [menu] [current-tab title] [+] [count ▢]
// `+` spawns a sibling into the focused pane. The count square opens
// WorkspaceTabsSheet: a full-screen card grid of the folder's terminals,
// mirroring the home page's FolderCard grid — tap a card to switch, ✕ to close.
//
// Rendered by TerminalDeck at the top of the deck when isCompact().
import { For, Show, createEffect, createMemo, createSignal, on } from "solid-js";
import { Portal } from "solid-js/web";
import { sessionTitle } from "../lib/sessionTitle.ts";
import { IconButton } from "./Settings/md/IconButton.tsx";
import { openSidebar } from "../store/uiStore.ts";
import type { Session } from "@roost/shared/wire";
import { WorkspaceTabsMenu } from "./WorkspaceTabsMenu.tsx";
import { flipGrid } from "../lib/gridFlip.ts";
import { TerminalCard } from "./TerminalCard.tsx";
import { deckTabBadge } from "../lib/deckTabBadge.ts";

const TITLE_TEXT: Record<string, string> = {
  "font-size": "14px",
  "font-weight": "600",
  overflow: "hidden",
  "text-overflow": "ellipsis",
  "white-space": "nowrap",
  "line-height": "48px",
};

export interface MobileDeckBarProps {
  /** Flattened, ordered terminals in the folder (all panes, leaf-then-tab order). */
  tabs: Session[];
  /** The terminal id currently painted full-bleed (URL-active). */
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
  const badge = createMemo(() => deckTabBadge(props.tabs.length, props.tabs.findIndex((tab) => tab.id === props.selectedTab)));

  return (
    <>
      <div
        class="mobile-deck-bar"
        data-testid="mobile-deck-bar"
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
          classList={{ "mobile-deck-count": true, "mobile-deck-count--fraction": badge().fraction }}
          data-testid="mobile-tab-count"
          aria-label={`Open terminal grid — ${badge().description}`}
          title={badge().description}
          onClick={() => setSheetOpen(true)}
        >
          <span>{badge().text}</span>
        </button>
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

  // Chrome DefaultItemAnimator move: when a card leaves, survivors slide to
  // their new slots instead of the grid snapping. FLIP keyed by tab id.
  let gridEl: HTMLDivElement | undefined;
  let _flipRects = new Map<string, DOMRect>();
  createEffect(
    on(
      () => props.tabs.map((t) => t.id).join(","),
      () => { if (gridEl) _flipRects = flipGrid(gridEl, _flipRects); },
    ),
  );

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
          <div ref={gridEl} class="workspace-tabs-grid" style={{ padding: "16px", "overflow-y": "auto", flex: "1 1 0" }}>
            <For each={props.tabs}>
              {(s) => (
                <div class="terminal-card-wrap" data-flip-key={s.id}>
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

// Deck-level "Arrange" control: a small top-right button (VS Code layout-button
// convention) opening a 5-option preset menu. Equalize keeps the current tree
// (panes/tabs/focus) and only re-balances split ratios; Grid/Columns/Rows/
// Main+stack rebuild one session per pane. The parent (TerminalDeck) owns the
// actual layout mutation via onArrange(kind) → arrangeLayout; this file is
// placement-agnostic UI only. Menu look reuses the shared context-menu
// primitives; anchoring/dismissal come from the shared anchored-menu helpers.

import { Show, createSignal, type JSX } from "solid-js";
import { Portal } from "solid-js/web";
import { platformShortcutLabel } from "../lib/browserPlatform.ts";
import type { ArrangeKind } from "../store/paneLayoutPresets.ts";
import {
	anchoredMenuPosition,
	anchoredMenuSurfaceStyle,
	CtxMenuItem,
	CtxMenuSeparator,
	trackFloatingMenuDismiss,
} from "./contextMenuPrimitives.tsx";

interface Props {
  onArrange: (kind: ArrangeKind) => void;
}

interface Item {
  kind: ArrangeKind;
  label: string;
  hint: string;
  testid: string;
}

const ITEMS: Item[] = [
  { kind: "tiled", label: "Grid", hint: platformShortcutLabel("arrangeGrid", "Cmd+Opt+G"), testid: "arrange-grid" },
  { kind: "even", label: "Columns", hint: platformShortcutLabel("arrangeColumns", "Cmd+Opt+E"), testid: "arrange-columns" },
  { kind: "rows", label: "Rows", hint: platformShortcutLabel("arrangeRows", "Cmd+Opt+R"), testid: "arrange-rows" },
  { kind: "main-vertical", label: "Main + stack", hint: platformShortcutLabel("arrangeMain", "Cmd+Opt+V"), testid: "arrange-main" },
];

// Icon shapes per preset, 24-unit grid, stroke style matches the df-arrange-btn
// glyph. Functions (not stored JSX) so each menu open creates fresh DOM nodes.
const GLYPHS: Record<ArrangeKind, () => JSX.Element> = {
  // "=" — equalize: same tree, ratios rebalanced to equal areas.
  balance: () => (
    <>
      <line x1="5" y1="9" x2="19" y2="9" />
      <line x1="5" y1="15" x2="19" y2="15" />
    </>
  ),
  // 2×2 quads — grid tiling (same shapes as the arrange button glyph).
  tiled: () => (
    <>
      <rect x="3" y="3" width="7" height="7" rx="1" />
      <rect x="14" y="3" width="7" height="7" rx="1" />
      <rect x="3" y="14" width="7" height="7" rx="1" />
      <rect x="14" y="14" width="7" height="7" rx="1" />
    </>
  ),
  // 2 tall bars — equal columns.
  even: () => (
    <>
      <rect x="3" y="3" width="7" height="18" rx="1" />
      <rect x="14" y="3" width="7" height="18" rx="1" />
    </>
  ),
  // 2 wide bars — equal full-width rows.
  rows: () => (
    <>
      <rect x="3" y="3" width="18" height="7" rx="1" />
      <rect x="3" y="14" width="18" height="7" rx="1" />
    </>
  ),
  // Big left pane + two stacked right panes.
  "main-vertical": () => (
    <>
      <rect x="3" y="3" width="10" height="18" rx="1" />
      <rect x="16" y="3" width="5" height="7" rx="1" />
      <rect x="16" y="14" width="5" height="7" rx="1" />
    </>
  ),
};

export function ArrangeMenu(props: Props) {
  // `right`/`y` = the button's right edge (as an offset from the viewport's
  // right) + its bottom. The menu CSS-anchors its right edge here and grows
  // leftward, content-sized — no width measurement, so it can't overflow the
  // screen (the button always sits at the deck's top-right).
  const [open, setOpen] = createSignal<{ right: number; y: number } | null>(null);
  let btnEl: HTMLButtonElement | undefined;
  let menuEl: HTMLDivElement | undefined;

  const toggle = () => {
    if (open()) { setOpen(null); return; }
    setOpen(anchoredMenuPosition(btnEl!));
  };

  const choose = (kind: ArrangeKind) => {
    props.onArrange(kind);
    setOpen(null);
  };

  // ctxMenuSurfaceStyle hardcodes `left` (cursor-anchored); this menu anchors
  // by its RIGHT edge and shrink-fits leftward — the primitive deletes it.
  const surfaceStyle = (pos: { right: number; y: number }) =>
    anchoredMenuSurfaceStyle(pos, { minWidth: "224px" });

  trackFloatingMenuDismiss({ within: [() => btnEl, () => menuEl], onClose: () => setOpen(null) });

  return (
    <>
      <button
        ref={btnEl}
        type="button"
        class="df-arrange-btn"
        data-testid="arrange-btn"
        aria-label="Arrange panes"
        title="Arrange panes"
        onClick={toggle}
      >
        <svg width="16" height="16" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2">
          <rect x="3" y="3" width="7" height="7" rx="1" />
          <rect x="14" y="3" width="7" height="7" rx="1" />
          <rect x="3" y="14" width="7" height="7" rx="1" />
          <rect x="14" y="14" width="7" height="7" rx="1" />
        </svg>
      </button>
      <Show when={open()}>
        {(pos) => (
          // Portal to <body>: an ancestor <main> carries a transform, which
          // makes position:fixed resolve against <main>'s box instead of the
          // viewport (see TerminalContextMenu). Portaling escapes it.
          <Portal>
            <div
              ref={menuEl}
              data-testid="arrange-menu"
              class="df-menu-enter"
              style={surfaceStyle(pos())}
            >
              <CtxMenuItem testid="arrange-balance" onClick={() => choose("balance")}>
                <ArrangeRow kind="balance" label="Equalize sizes" hint={platformShortcutLabel("arrangeBalance", "Cmd+Opt+B")} />
              </CtxMenuItem>
              <CtxMenuSeparator />
              {ITEMS.map((it) => (
                <CtxMenuItem testid={it.testid} onClick={() => choose(it.kind)}>
                  <ArrangeRow kind={it.kind} label={it.label} hint={it.hint} />
                </CtxMenuItem>
              ))}
            </div>
          </Portal>
        )}
      </Show>
    </>
  );
}

function ArrangeRow(props: { kind: ArrangeKind; label: string; hint: string }) {
  return (
    <div style={{ display: "flex", "align-items": "center", gap: "8px", "white-space": "nowrap" }}>
      <svg
        width="14" height="14" viewBox="0 0 24 24" fill="none"
        stroke="currentColor" stroke-width="2" stroke-linecap="round"
        aria-hidden="true" style={{ flex: "none", color: "var(--text-lo)" }}
      >
        {GLYPHS[props.kind]()}
      </svg>
      <span>{props.label}</span>
      <span style={{ "margin-left": "auto", "padding-left": "16px", color: "var(--text-lo)" }}>{props.hint}</span>
    </div>
  );
}

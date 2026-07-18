// Deck-level "Arrange" control: a small top-right button (VS Code layout-button
// convention) opening a 5-option preset menu. Equalize keeps the current tree
// (panes/tabs/focus) and only re-balances split ratios; Grid/Columns/Rows/
// Main+stack rebuild one session per pane. The parent (TerminalDeck) owns the
// actual layout mutation via onArrange(kind) → arrangeLayout; this file is
// placement-agnostic UI only. Menu look reuses the shared context-menu
// primitives; dismissal + Portal copy TerminalContextMenu.

import { Show, createSignal, onCleanup, onMount, type JSX } from "solid-js";
import { Portal } from "solid-js/web";
import type { ArrangeKind } from "../store/paneLayoutPresets.ts";
import { ctxMenuSurfaceStyle, CtxMenuItem, CtxMenuSeparator } from "./contextMenuPrimitives.tsx";

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
  { kind: "tiled", label: "Grid", hint: "Cmd+Opt+G", testid: "arrange-grid" },
  { kind: "even", label: "Columns", hint: "Cmd+Opt+E", testid: "arrange-columns" },
  { kind: "rows", label: "Rows", hint: "Cmd+Opt+R", testid: "arrange-rows" },
  { kind: "main-vertical", label: "Main + stack", hint: "Cmd+Opt+V", testid: "arrange-main" },
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
    if (open()) {
      setOpen(null);
      return;
    }
    const r = btnEl!.getBoundingClientRect();
    setOpen({ right: Math.max(6, window.innerWidth - r.right), y: r.bottom + 4 });
  };

  const choose = (kind: ArrangeKind) => {
    props.onArrange(kind);
    setOpen(null);
  };

  // ctxMenuSurfaceStyle hardcodes `left` (it's a cursor-anchored menu); this
  // menu instead anchors by its RIGHT edge (`right` = viewport-right → button-
  // right offset) and shrink-fits leftward. Delete the inherited `left` so a
  // stale left + our right don't both constrain the box into full width.
  const surfaceStyle = (right: number, y: number) => {
    const s = { ...ctxMenuSurfaceStyle(0, y), "min-width": "224px", right: `${right}px` };
    delete s.left;
    return s;
  };

  // Deterministic dismissal (vs Solid's delegated-click ordering): a
  // document-level click closes UNLESS it landed on the button or inside the
  // menu; Escape always closes. Menu item clicks close explicitly.
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
              style={surfaceStyle(pos().right, pos().y)}
            >
              <CtxMenuItem testid="arrange-balance" onClick={() => choose("balance")}>
                <ArrangeRow kind="balance" label="Equalize sizes" hint="Cmd+Opt+B" />
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

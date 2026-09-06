// Deck-level pane-layout menu: preset arrangement plus local portable-document
// copy, download, and import actions. TerminalDeck owns every mutation and I/O
// callback; this file only anchors and dismisses the shared floating menu.
// Presets keep their existing pane/tab/focus behavior and disable for a
// one-session folder while document actions remain available.

import { Show, createSignal, type JSX } from "solid-js";
import { Portal } from "solid-js/web";
import { platformShortcutLabel } from "../lib/browserPlatform.ts";
import type { ArrangeKind } from "../store/paneLayoutPresets.ts";
import {
  anchoredMenuPosition,
  anchoredMenuSurfaceStyle,
  CtxMenuItem,
  CtxMenuSeparator,
  focusMenuEdge,
  handleMenuKeyboardNavigation,
  trackFloatingMenuDismiss,
} from "./contextMenuPrimitives.tsx";
import type { MenuFocusEdge } from "./contextMenuPrimitives.tsx";

interface Props {
  canArrange: boolean;
  onArrange: (kind: ArrangeKind) => void;
  onCopyLayout: () => void;
  onDownloadLayout: () => void;
  onImportLayout: () => void;
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
  // right) + its bottom. The menu grows leftward so it cannot overflow.
  const [open, setOpen] = createSignal<{ right: number; y: number } | null>(null);
  let triggerElement: HTMLButtonElement | undefined;
  let menuElement: HTMLDivElement | undefined;
  let cancelPendingFocus: (() => void) | null = null;

  const openMenu = (edge: MenuFocusEdge = "first") => {
    if (!triggerElement) return;
    cancelPendingFocus?.();
    setOpen(anchoredMenuPosition(triggerElement));
    cancelPendingFocus = focusMenuEdge(() => menuElement, edge);
  };

  const closeMenu = (restoreTriggerFocus = false) => {
    cancelPendingFocus?.();
    cancelPendingFocus = null;
    setOpen(null);
    if (restoreTriggerFocus) queueMicrotask(() => triggerElement?.focus());
  };

  const toggle = () => {
    if (open()) {
      closeMenu();
      return;
    }
    openMenu();
  };

  const choose = (kind: ArrangeKind) => {
    closeMenu(true);
    props.onArrange(kind);
  };

  const chooseAction = (action: () => void) => {
    closeMenu(true);
    action();
  };

  const onTriggerKeyDown = (event: KeyboardEvent) => {
    if (event.key !== "ArrowDown" && event.key !== "ArrowUp") return;
    event.preventDefault();
    event.stopPropagation();
    openMenu(event.key === "ArrowDown" ? "first" : "last");
  };

  const onMenuKeyDown = (event: KeyboardEvent) => {
    handleMenuKeyboardNavigation(
      event,
      menuElement,
      () => closeMenu(true),
      () => closeMenu(),
    );
  };

  // ctxMenuSurfaceStyle hardcodes `left` (cursor-anchored); this menu anchors
  // by its RIGHT edge and shrink-fits leftward — the primitive deletes it.
  const surfaceStyle = (position: { right: number; y: number }) =>
    anchoredMenuSurfaceStyle(position, { minWidth: "224px" });

  trackFloatingMenuDismiss({
    within: [() => triggerElement, () => menuElement],
    onClose: () => closeMenu(),
  });

  return (
    <>
      <button
        ref={triggerElement}
        id="arrange-menu-trigger"
        type="button"
        class="df-arrange-btn"
        data-testid="arrange-btn"
        aria-label="Arrange or transfer pane layout"
        aria-haspopup="menu"
        aria-controls="arrange-menu"
        aria-expanded={open() !== null}
        title="Arrange or transfer pane layout"
        onClick={toggle}
        onKeyDown={onTriggerKeyDown}
      >
        <svg
          width="16"
          height="16"
          viewBox="0 0 24 24"
          fill="none"
          stroke="currentColor"
          stroke-width="2"
          aria-hidden="true"
        >
          <rect x="3" y="3" width="7" height="7" rx="1" />
          <rect x="14" y="3" width="7" height="7" rx="1" />
          <rect x="3" y="14" width="7" height="7" rx="1" />
          <rect x="14" y="14" width="7" height="7" rx="1" />
        </svg>
      </button>
      <Show when={open()}>
        {(position) => (
          // Portal escapes the transformed main element so fixed positioning
          // remains viewport-relative.
          <Portal>
            <div
              ref={menuElement}
              id="arrange-menu"
              role="menu"
              aria-labelledby="arrange-menu-trigger"
              data-testid="arrange-menu"
              class="df-menu-enter"
              style={surfaceStyle(position())}
              onKeyDown={onMenuKeyDown}
            >
              <CtxMenuItem
                testid="arrange-balance"
                disabled={!props.canArrange}
                onClick={() => choose("balance")}
              >
                <ArrangeRow kind="balance" label="Equalize sizes" hint={platformShortcutLabel("arrangeBalance", "Cmd+Opt+B")} />
              </CtxMenuItem>
              <CtxMenuSeparator />
              {ITEMS.map((item) => (
                <CtxMenuItem
                  testid={item.testid}
                  disabled={!props.canArrange}
                  onClick={() => choose(item.kind)}
                >
                  <ArrangeRow kind={item.kind} label={item.label} hint={item.hint} />
                </CtxMenuItem>
              ))}
              <CtxMenuSeparator />
              <CtxMenuItem testid="layout-copy" onClick={() => chooseAction(props.onCopyLayout)}>
                Copy layout
              </CtxMenuItem>
              <CtxMenuItem testid="layout-download" onClick={() => chooseAction(props.onDownloadLayout)}>
                Download layout
              </CtxMenuItem>
              <CtxMenuItem testid="layout-import" onClick={() => chooseAction(props.onImportLayout)}>
                Import layout…
              </CtxMenuItem>
            </div>
          </Portal>
        )}
      </Show>
    </>
  );
}

function ArrangeRow(props: { kind: ArrangeKind; label: string; hint: string }) {
  return (
    <span style={{ display: "flex", "align-items": "center", gap: "8px", "white-space": "nowrap" }}>
      <svg
        width="14" height="14" viewBox="0 0 24 24" fill="none"
        stroke="currentColor" stroke-width="2" stroke-linecap="round"
        aria-hidden="true" style={{ flex: "none", color: "var(--text-lo)" }}
      >
        {GLYPHS[props.kind]()}
      </svg>
      <span>{props.label}</span>
      <span style={{ "margin-left": "auto", "padding-left": "16px", color: "var(--text-lo)" }}>{props.hint}</span>
    </span>
  );
}

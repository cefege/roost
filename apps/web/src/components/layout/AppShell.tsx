// Main 2-pane layout: sidebar (left) + main pane (right).
// Mobile (<768px): sidebar hidden behind hamburger-triggered slide-out drawer.
// Desktop (≥768px): sidebar always visible; sidebarOpen ignored.
// Children slot renders the active route's main content.

import { ParentProps, Show, onMount, onCleanup, createEffect, on } from "solid-js";
import { useLocation } from "@solidjs/router";
import { SidebarRoot } from "../sidebar/SidebarRoot.tsx";
import { MobileTopBar } from "./MobileTopBar.tsx";
import { uiStore, closeSidebar, toggleSidebarCollapsed, setSidebarWidth } from "../../store/uiStore.ts";
import { isCompact } from "../../lib/windowSizeClass.ts";
import { keyboardResize } from "../../lib/keyboardResizePref.ts";
import { beginPointerResizeDrag, resetResizeDrags } from "../../lib/resizeDrag.ts";
import { EDGE_PX, lockAxis, openOffsetPx, shouldOpen, closeOffsetPx, shouldClose } from "../../lib/edgeSwipeDrawer.ts";
import { registerDrawer, dragDrawer, settleDrawerOpen, settleDrawerClose } from "../../lib/drawerDrag.ts";
import { attachElasticOverscroll } from "../../lib/overscroll.ts";
import { composerActive, composerHeightPx } from "../TerminalComposeButton.tsx";
import { matchesPlatformShortcut } from "../../lib/browserPlatform.ts";

// ─── inline CSS helpers ─────────────────────────────────────────────────
// Style objects are evaluated once; any dynamic value must live in JSX
// Show/Switch or computed via functions returning plain objects.

function shellStyle() {
  return {
    display: "flex",
    // Soft-keyboard behavior is pref-driven (lib/keyboardResizePref.ts):
    //  - push (DEFAULT, keyboardResize() === false): shell height stays 100svh;
    //    AppShell's mainStyle() translateY's the content up instead — the grid
    //    size is unchanged, so no scrollback recompute while the keyboard
    //    slides in. 100svh base (NOT dvh) so chrome wobble never changes the
    //    shell, only --kb-offset does.
    //  - resize (toggle on): shell height = 100svh − --kb-offset, so the
    //    terminal's ResizeObserver re-claims a smaller grid and grows back on
    //    dismiss.
    // A mounted composer reserves only its resting row in mainStyle(). Measured
    // multiline growth shifts the painted deck instead of resizing its observed
    // box and reclaiming PTY rows; the dock offset already owns the keyboard.
    height: keyboardResize() && !composerActive() ? "calc(100svh - var(--kb-offset, 0px))" : "100svh",
    transition: "height var(--md-sys-motion-duration-medium1) var(--md-sys-motion-easing-emphasized)",
    overflow: "hidden",
    background: "var(--bg-base)",
    color: "var(--text-hi)",
    position: "relative" as const,
  };
}

// Collapsed = the SAME sidebar (SidebarRoot), CSS-narrowed to a 52px
// icon rail via [data-collapsed="true"] in sidebar.css. NOT a separate
// worker-initials component — that diverged from the real folder list.
const COLLAPSED_RAIL_PX = 52;
const SIDEBAR_RESIZER_PX = 4;

function desktopSidebarStyle() {
  return {
    width: uiStore.sidebarCollapsed ? `${COLLAPSED_RAIL_PX}px` : `${uiStore.sidebarWidth}px`,
    // bg + font set in sidebar.css under aside[data-testid="sidebar-desktop"]
    "border-right": "none",
    overflow: "hidden auto",
    "flex-shrink": 0,
  };
}

// Draggable divider between desktop sidebar and main pane. Pointer-down
// captures the pointer; pointer-move updates uiStore.sidebarWidth;
// pointer-up releases + persists. Double-click resets to the default
// 300px. localStorage persistence handled inside setSidebarWidth.
function SidebarResizer() {
  let disposeGesture: (() => void) | undefined;

  onCleanup(() => disposeGesture?.());

  function onPointerDown(e: PointerEvent) {
    if (e.button !== 0 || disposeGesture) return;

    const target = e.currentTarget as HTMLElement;
    const startX = e.clientX;
    const startW = uiStore.sidebarWidth;

    document.body.style.cursor = "col-resize";
    document.body.style.userSelect = "none";
    disposeGesture = beginPointerResizeDrag({
      target,
      pointerId: e.pointerId,
      initialGeometry: startW,
      geometryFor: (ev) => startW + (ev.clientX - startX),
      onMove: setSidebarWidth,
      onCommit: setSidebarWidth,
      onRelease: () => {
        disposeGesture = undefined;
        document.body.style.cursor = "";
        document.body.style.userSelect = "";
      },
    });
  }
  function onDoubleClick() { setSidebarWidth(300); }
  return (
    <div
      data-testid="sidebar-resizer"
      onPointerDown={onPointerDown}
      onDblClick={onDoubleClick}
      style={{
        width: "4px",
        flex: "0 0 auto",
        cursor: "col-resize",
        background: "transparent",
        "border-right": "1px solid var(--border-subtle)",
        "touch-action": "none",
      }}
      // Wider hit-target via padding without visual width
      onMouseEnter={(e) => { e.currentTarget.style.background = "var(--accent)"; }}
      onMouseLeave={(e) => { e.currentTarget.style.background = "transparent"; }}
    />
  );
}

// Drawer + overlay slide is driven by the reactive `data-open` ATTRIBUTE
// (see .roost-drawer / .roost-drawer-overlay in sidebar.css). Inline
// style={fn(store.x)} is evaluated once here and does NOT react (file-header
// convention) — that left the mobile sidebar permanently stuck off-screen.
// Attribute bindings ARE reactive (same path as aria-hidden), so the CSS
// keyed on [data-open="true"] animates correctly.

// Soft-keyboard handling is pref-driven (keyboardResizePref):
//  - push (DEFAULT): shell stays full height; shift main up by --kb-offset so
//    the input rides above the keyboard (grid size unchanged, top scrolls off).
//  - resize (toggle on): the SHELL already shrank by --kb-offset (shellStyle),
//    so main needs NO transform — the terminal re-claims a smaller grid.
// lib/keyboardInset.ts sets --kb-offset; 0px on desktop.
function mainStyle() {
  const base = {
    flex: "1 1 0",
    overflow: "hidden",
    display: "flex",
    "flex-direction": "column",
    "box-sizing": "border-box",
    "min-height": "0",
    "--term-chat-growth": "0px",
  } as const;
  // Keep the deck's layout box at the one-row reserve. ResizeObserver publishes
  // only the dock's border-box height; its excess becomes an inherited
  // paint-only translate so TerminalDeck and the PTY grid never resize/reclaim
  // while the draft wraps. The dock offset is the shared safe-area, keyboard,
  // and 8px bottom-gap expression.
  if (composerActive()) {
    return {
      ...base,
      "padding-bottom": "calc(var(--term-chat-rest-height) + var(--term-chat-dock-offset))",
      "--term-chat-growth": `max(0px, calc(${composerHeightPx()}px - var(--term-chat-rest-height)))`,
    };
  }
  if (keyboardResize()) return base;
  return {
    ...base,
    transform: "translateY(calc(var(--kb-offset, 0px) * -1))",
    transition: "transform var(--md-sys-motion-duration-medium1) var(--md-sys-motion-easing-emphasized)",
  };
}

// ─── component ─────────────────────────────────────────────────────────

export function AppShell(props: ParentProps) {
  const isMobile = isCompact;
  const location = useLocation();

  // Keep the portaled composer inside the main pane as the desktop rail resizes.
  createEffect(() => {
    const mainLeft = isMobile()
      ? 0
      : uiStore.sidebarCollapsed
        ? COLLAPSED_RAIL_PX
        : uiStore.sidebarWidth + SIDEBAR_RESIZER_PX;
    document.documentElement.style.setProperty("--roost-main-left", `${mainLeft}px`);
  });
  // Mobile drawer overlays the main pane. Navigating from inside the drawer
  // (any sidebar nav source: SessionRow, MachineSection spawn, FlatNewTerminal,
  // context menu, empty-state CTA, command palette, deep link) switches the
  // route underneath but the drawer keeps covering it — so the chosen terminal
  // looked unchanged and the user had to tap a second time (backdrop / main
  // pane) to reveal it. One route-change effect auto-closes the drawer on
  // mobile so a single tap both navigates AND reveals. defer:true skips the
  // mount run (drawer already closed → no-op, but avoids churn).
  createEffect(on(() => location.pathname, () => {
    if (isMobile()) closeSidebar();
  }, { defer: true }));

  // Platform map keeps macOS/Linux behavior and moves Windows off plain Ctrl+B.
  function onKeyDown(e: KeyboardEvent) {
    if (e.defaultPrevented) return;
    if (!matchesPlatformShortcut(e, "toggleSidebar")) return;
    e.preventDefault();
    toggleSidebarCollapsed();
  }

  // ── Left-edge swipe-to-open the mobile drawer ──────────────────────────
  // Native drawer gesture: a drag starting within EDGE_PX of the left edge on
  // mobile drives the drawer's translateX live and snaps open / springs back on
  // release. Window-level, capture-phase, so we run before CellTerminal's own
  // touch listeners and can claim the gesture via stopPropagation. Transforms
  // are written IMPERATIVELY onto drawerEl.style (not reactive inline style —
  // see file-header convention); on settle we hand back to the data-open CSS.
  let _startX = 0;
  let _startY = 0;
  let _lastX = 0;
  let _axis: "none" | "x" | "y" = "none";
  let _armed = false;
  let _candidate = false;
  let _samples: { x: number; t: number }[] = [];
  let _mode: "open" | "close" | null = null;

  function onTouchStart(e: TouchEvent) {
    _mode = null;
    if (!isMobile() || e.touches.length !== 1) { _candidate = false; return; }
    const t = e.touches[0]!;
    if (uiStore.sidebarOpen) {
      // Skip touches that begin on the horizontally-scrollable folder tab bar
      // (so tab scroll doesn't dismiss) or on a SessionRow swipe wrapper (so a
      // leftward swipe there runs its own swipe-to-delete instead of closing).
      if ((e.target as Element | null)?.closest?.(".df-tab-bar, .df-row-swipe")) { _candidate = false; return; }
      _mode = "close";
      _candidate = true;
    } else if ((t.clientX ?? Infinity) <= EDGE_PX) {
      _mode = "open";
      _candidate = true;
    } else { _candidate = false; return; }
    _startX = t.clientX;
    _startY = t.clientY;
    _lastX = t.clientX;
    _axis = "none";
    _armed = false;
    _samples = [{ x: t.clientX, t: performance.now() }];
  }

  function onTouchMove(e: TouchEvent) {
    if (!_candidate) return;
    const t = e.touches[0];
    if (!t) return;
    const dx = t.clientX - _startX;
    const dy = t.clientY - _startY;
    if (_axis === "none") {
      const lock = lockAxis(dx, dy);
      if (lock === "none") return;
      if (lock === "y") { _candidate = false; return; } // vertical scroll → release
      _axis = "x";
    }
    if (_mode === "open" ? dx <= 0 : dx >= 0) { _candidate = false; return; } // open needs rightward, close needs leftward
    e.preventDefault();
    e.stopPropagation(); // capture-phase: keep CellTerminal's listeners silent
    if (!_armed) {
      _armed = true;
    }
    const now = performance.now();
    _samples.push({ x: t.clientX, t: now });
    while (_samples.length > 2 && _samples[0]!.t < now - 120) _samples.shift();
    _lastX = t.clientX;
    const off = _mode === "close"
      ? closeOffsetPx(dx, window.innerWidth)
      : openOffsetPx(dx, window.innerWidth);
    dragDrawer(off);
  }

  function onTouchEnd() {
    if (!_armed) { _candidate = false; return; } // tap or vertical → nothing to settle
    const dx = _lastX - _startX;
    // velocity over the last ~80ms of samples (px/ms); a flick reads high here
    const now = performance.now();
    while (_samples.length > 1 && _samples[0]!.t < now - 80) _samples.shift();
    let velocity = 0;
    if (_samples.length >= 2) {
      const first = _samples[0]!;
      const last = _samples[_samples.length - 1]!;
      const dt = last.t - first.t;
      if (dt > 0) velocity = (last.x - first.x) / dt;
    }
    if (_mode === "open") settleDrawerOpen(shouldOpen(dx, velocity, window.innerWidth));
    else settleDrawerClose(shouldClose(dx, velocity, window.innerWidth));
    _armed = false;
    _candidate = false;
    _samples = [];
    _mode = null;
  }
  const onPageShow = () => resetResizeDrags();

  onMount(() => {
    window.addEventListener("keydown", onKeyDown);
    window.addEventListener("pageshow", onPageShow);
    window.addEventListener("touchstart", onTouchStart, { capture: true, passive: true });
    window.addEventListener("touchmove", onTouchMove, { capture: true, passive: false });
    window.addEventListener("touchend", onTouchEnd, { capture: true, passive: true });
    window.addEventListener("touchcancel", onTouchEnd, { capture: true, passive: true });
  });
  onCleanup(() => {
    window.removeEventListener("keydown", onKeyDown);
    window.removeEventListener("pageshow", onPageShow);
    window.removeEventListener("touchstart", onTouchStart, { capture: true });
    window.removeEventListener("touchmove", onTouchMove, { capture: true });
    window.removeEventListener("touchend", onTouchEnd, { capture: true });
    window.removeEventListener("touchcancel", onTouchEnd, { capture: true });
    document.documentElement.style.removeProperty("--roost-main-left");
  });

  return (
    <div style={shellStyle()}>
      {/* ── Desktop sidebar: SAME SidebarRoot whether expanded or the 52px
          icon rail. data-collapsed drives the CSS narrow-to-icons; the
          resizer is hidden while collapsed (a fixed-width rail). ── */}
      <Show when={!isMobile()}>
        <aside
          data-testid="sidebar-desktop"
          data-collapsed={uiStore.sidebarCollapsed ? "true" : "false"}
          ref={(el) => onCleanup(attachElasticOverscroll(el))}
          style={desktopSidebarStyle()}
        >
          <SidebarRoot />
        </aside>
        <Show when={!uiStore.sidebarCollapsed}>
          <SidebarResizer />
        </Show>
      </Show>

      {/* ── Mobile: backdrop overlay — tap closes drawer ── */}
      <Show when={isMobile()}>
        <div
          data-testid="sidebar-overlay"
          class="roost-drawer-overlay"
          data-open={uiStore.sidebarOpen ? "true" : "false"}
          onClick={closeSidebar}
          aria-hidden="true"
        />
      </Show>

      {/* ── Mobile: slide-out drawer ── */}
      <Show when={isMobile()}>
        <aside
          data-testid="sidebar-drawer"
          ref={(el) => { registerDrawer(el); onCleanup(attachElasticOverscroll(el)); }}
          class="roost-drawer"
          data-open={uiStore.sidebarOpen ? "true" : "false"}
          aria-hidden={!uiStore.sidebarOpen}
        >
          <SidebarRoot />
        </aside>
      </Show>

      {/* ── Main content (mobile top bar stacks above it in the column) ── */}
      <main style={mainStyle()}>
        {/* Home ("/") and terminal routes own their own header + bar; suppress
            the redundant MobileTopBar there. Every other mobile route keeps it. */}
        <Show when={isMobile() && location.pathname !== "/" && !location.pathname.startsWith("/browse") && !(location.pathname.startsWith("/s/") || location.pathname.startsWith("/t/") || location.pathname.startsWith("/w/"))}>
          <MobileTopBar />
        </Show>
        {props.children}
      </main>
    </div>
  );
}

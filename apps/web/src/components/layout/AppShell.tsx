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
import { beginResizeDrag, endResizeDrag } from "../../lib/resizeDrag.ts";

// ─── inline CSS helpers ─────────────────────────────────────────────────
// Style objects are evaluated once; any dynamic value must live in JSX
// Show/Switch or computed via functions returning plain objects.

function shellStyle() {
  return {
    display: "flex",
    // Soft-keyboard behavior is pref-driven (lib/keyboardResizePref.ts):
    //  - resize (default): shell height = 100svh − --kb-offset, so the layout
    //    shrinks to the space above the keyboard → the terminal's
    //    ResizeObserver re-claims a smaller grid and grows back on dismiss.
    //    Safe in cell mode (client never reflows history). 100svh base (NOT
    //    dvh) so ONLY the keyboard inset drives the change, not chrome wobble.
    //  - push (toggle off): height stays 100svh; AppShell's mainStyle()
    //    translateY's the content up instead (grid size unchanged).
    height: keyboardResize() ? "calc(100svh - var(--kb-offset, 0px))" : "100svh",
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
  let startX = 0;
  let startW = 0;
  let dragging = false;
  function onPointerDown(e: PointerEvent) {
    if (e.button !== 0) return;
    dragging = true;
    beginResizeDrag();
    startX = e.clientX;
    startW = uiStore.sidebarWidth;
    (e.currentTarget as HTMLElement).setPointerCapture(e.pointerId);
    document.body.style.cursor = "col-resize";
    document.body.style.userSelect = "none";
  }
  function onPointerMove(e: PointerEvent) {
    if (!dragging) return;
    setSidebarWidth(startW + (e.clientX - startX));
  }
  function onPointerUp(e: PointerEvent) {
    if (!dragging) return;
    dragging = false;
    endResizeDrag();
    (e.currentTarget as HTMLElement).releasePointerCapture(e.pointerId);
    document.body.style.cursor = "";
    document.body.style.userSelect = "";
  }
  function onPointerCancel() {
    if (!dragging) return;
    dragging = false;
    document.body.style.cursor = "";
    document.body.style.userSelect = "";
    endResizeDrag();
  }
  function onDoubleClick() { setSidebarWidth(300); }
  return (
    <div
      data-testid="sidebar-resizer"
      onPointerDown={onPointerDown}
      onPointerMove={onPointerMove}
      onPointerUp={onPointerUp}
      onPointerCancel={onPointerCancel}
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
//  - resize (default): the SHELL already shrank by --kb-offset (shellStyle),
//    so main needs NO transform — the terminal just re-claims a smaller grid.
//  - push (toggle off): shell stays full height; shift main up by --kb-offset
//    so the input rides above the keyboard (grid size unchanged, top scrolls
//    off). lib/keyboardInset.ts sets --kb-offset; 0px on desktop.
function mainStyle() {
  const base = {
    flex: "1 1 0",
    overflow: "hidden",
    display: "flex",
    "flex-direction": "column",
  } as const;
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

  // ⌘B / Ctrl+B toggles the desktop sidebar collapse (matches claude.ai/code).
  function onKeyDown(e: KeyboardEvent) {
    if ((e.metaKey || e.ctrlKey) && e.key.toLowerCase() === "b" && !e.shiftKey) {
      e.preventDefault();
      toggleSidebarCollapsed();
    }
  }
  onMount(() => window.addEventListener("keydown", onKeyDown));
  onCleanup(() => window.removeEventListener("keydown", onKeyDown));

  return (
    <div style={shellStyle()}>
      {/* ── Desktop sidebar: SAME SidebarRoot whether expanded or the 52px
          icon rail. data-collapsed drives the CSS narrow-to-icons; the
          resizer is hidden while collapsed (a fixed-width rail). ── */}
      <Show when={!isMobile()}>
        <aside
          data-testid="sidebar-desktop"
          data-collapsed={uiStore.sidebarCollapsed ? "true" : "false"}
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

      {/* ── Mobile: slide-out drawer (claude.ai/code pattern) ── */}
      <Show when={isMobile()}>
        <aside
          data-testid="sidebar-drawer"
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

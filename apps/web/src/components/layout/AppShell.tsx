// Canonical application shell. Owns the desktop workbench grid and compact route slot.
// The existing SidebarRoot, terminal routes, drawer gestures, and composer geometry remain intact.
// Workbench chrome is structural only: MainPane still owns the persistent terminal deck.

import { createEffect, createMemo, on, onCleanup, onMount, Show, type ParentProps } from "solid-js";
import { useLocation } from "@solidjs/router";
import { SidebarRoot } from "../sidebar/SidebarRoot.tsx";
import { MobileTopBar } from "./MobileTopBar.tsx";
import { MobileSidebarDrawer } from "./MobileSidebarDrawer.tsx";
import { SidebarResizer } from "./SidebarResizer.tsx";
import { WorkbenchActivityBar } from "./WorkbenchActivityBar.tsx";
import { WorkbenchStatusBar } from "./WorkbenchStatusBar.tsx";
import { WorkbenchTitleBar } from "./WorkbenchTitleBar.tsx";
import { uiStore, closeSidebar, toggleSidebarCollapsed } from "../../store/uiStore.ts";
import { isCompact } from "../../lib/windowSizeClass.ts";
import { keyboardResize } from "../../lib/keyboardResizePref.ts";
import { resetResizeDrags } from "../../lib/resizeDrag.ts";
import { composerActive, composerHeightPx } from "../TerminalComposeButton.tsx";
import { matchesPlatformShortcut } from "../../lib/browserPlatform.ts";
import { ROUTES } from "../../routes.ts";

function shellStyle() {
  return {
    height: keyboardResize() && !composerActive()
      ? "calc(100svh - var(--kb-offset))"
      : "100svh",
    "--workbench-sidebar-expanded-width": `${uiStore.sidebarWidth}px`,
    "--workbench-sidebar-width": uiStore.sidebarCollapsed
      ? "0px"
      : "var(--workbench-sidebar-expanded-width)",
    "--workbench-sidebar-resizer-active-width": uiStore.sidebarCollapsed
      ? "0px"
      : "var(--workbench-sidebar-resizer-width)",
  };
}

function editorStyle(isTerminalRoute: boolean) {
  const base = { "--term-chat-growth": "0" };
  if (!isTerminalRoute || keyboardResize()) return base;
  if (composerActive()) {
    return {
      ...base,
      "padding-bottom": "calc(var(--term-chat-rest-height) + var(--term-chat-dock-offset))",
      "--term-chat-growth": `max(0px, calc(${composerHeightPx()}px - var(--term-chat-rest-height)))`,
    };
  }
  return { ...base, transform: "translateY(calc(var(--kb-offset) * -1))" };
}

export function AppShell(props: ParentProps) {
  const location = useLocation();
  let desktopSidebarRegion: HTMLElement | undefined;
  const compact = isCompact;
  const terminalRoute = createMemo(() => {
    const pathname = location.pathname;
    return pathname.startsWith("/s/") || pathname.startsWith("/t/") || pathname.startsWith("/w/");
  });
  const showMobileTopBar = createMemo(() => {
    const pathname = location.pathname;
    return compact()
      && pathname !== ROUTES.ROOT
      && !pathname.startsWith("/browse")
      && !pathname.startsWith("/settings")
      && !terminalRoute();
  });

  createEffect(() => {
    const sidebarOffset = uiStore.sidebarCollapsed
      ? "0px"
      : `calc(${uiStore.sidebarWidth}px + var(--workbench-sidebar-resizer-width))`;
    const mainOffset = compact()
      ? "0px"
      : `calc(var(--workbench-activity-width) + ${sidebarOffset})`;
    document.documentElement.style.setProperty("--roost-main-left", mainOffset);
  });

  createEffect(on(() => location.pathname, () => {
    if (compact()) closeSidebar();
  }, { defer: true }));

  function focusActivityBeforeSidebarCollapse(): boolean {
    if (
      compact()
      || uiStore.sidebarCollapsed
      || !desktopSidebarRegion?.contains(document.activeElement)
    ) return false;
    document.getElementById("workbench-activity-sessions")?.focus();
    return true;
  }

  function toggleDesktopSidebar(): void {
    const restoreActivityFocus = focusActivityBeforeSidebarCollapse();
    toggleSidebarCollapsed();
    if (!restoreActivityFocus) return;
    const sessionsControl = document.getElementById("workbench-activity-sessions");
    sessionsControl?.focus();
    requestAnimationFrame(() => window.setTimeout(() => sessionsControl?.focus(), 0));
  }

  function handleShellKeydown(event: KeyboardEvent) {
    if (event.defaultPrevented || !matchesPlatformShortcut(event, "toggleSidebar")) return;
    event.preventDefault();
    toggleDesktopSidebar();
  }

  onMount(() => {
    window.addEventListener("keydown", handleShellKeydown);
    window.addEventListener("pageshow", resetResizeDrags);
  });

  onCleanup(() => {
    window.removeEventListener("keydown", handleShellKeydown);
    window.removeEventListener("pageshow", resetResizeDrags);
    document.documentElement.style.removeProperty("--roost-main-left");
  });

  return (
    <div class="workbench-shell" data-compact={compact() ? "true" : "false"} style={shellStyle()}>
      <Show when={!compact()}>
        <WorkbenchTitleBar />
        <WorkbenchActivityBar onToggleSidebar={toggleDesktopSidebar} />
        <div
          class="workbench-sidebar-region"
          ref={(element) => {
            desktopSidebarRegion = element;
          }}
          data-collapsed={uiStore.sidebarCollapsed ? "true" : "false"}
          inert={uiStore.sidebarCollapsed ? true : undefined}
          aria-hidden={uiStore.sidebarCollapsed ? "true" : undefined}
        >
          <aside
            class="workbench-sidebar"
            data-testid="sidebar-desktop"
            data-collapsed={uiStore.sidebarCollapsed ? "true" : "false"}
          >
            <SidebarRoot />
          </aside>
          <Show when={!uiStore.sidebarCollapsed}>
            <SidebarResizer />
          </Show>
        </div>
      </Show>

      <main
        class="workbench-editor-region"
        data-keyboard-shift={terminalRoute() && !composerActive() && !keyboardResize() ? "true" : undefined}
        style={editorStyle(terminalRoute())}
      >
        <Show when={showMobileTopBar()}>
          <MobileTopBar />
        </Show>
        <div class="workbench-editor-slot">{props.children}</div>
      </main>

      <Show when={!compact()}>
        <WorkbenchStatusBar />
      </Show>
      <Show when={compact()}>
        <MobileSidebarDrawer />
      </Show>
    </div>
  );
}

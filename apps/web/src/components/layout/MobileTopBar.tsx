// Mobile top app bar — a slim bar above the main
// content on compact: menu button (opens the sliding drawer) + the current
// context's title. Replaces the crude floating hamburger box. Rendered by
// AppShell inside <main> (a flex column) so the terminal flows below it — no
// fixed positioning, no occlusion of the input line.
//
// Owners: AppShell.tsx. Depends on: uiStore (openSidebar), rootStore.

import { createMemo } from "solid-js";
import { useLocation } from "@solidjs/router";
import { rootStore } from "../../store/root.ts";
import { activeSessionForPath } from "../../store/selectors.ts";
import { openSidebar } from "../../store/uiStore.ts";
import { IconButton } from "../Settings/md/primitives.tsx";
import { workerPathBasename } from "../../lib/nativePath.ts";


export function MobileTopBar() {
  const location = useLocation();

  // Title from the URL: search/file get labels; a session shows its OSC
  // terminal title (what the shell sets) falling back to the cwd leaf.
  const title = createMemo(() => {
    const path = location.pathname;
    if (path.startsWith("/search")) return "Search";
    if (path.startsWith("/file/")) return "Files";
    const session = activeSessionForPath(path);
    if (session) {
      const osc = rootStore.terminal_title[session.id]?.trim();
      if (osc) return osc.slice(0, 60);
      return workerPathBasename(session.worker_fp, session.cwd) || "~";
    }
    if (path.startsWith("/s/") || path.startsWith("/t/") || path.startsWith("/w/")) return "Terminal";
    return "Roost";
  });

  return (
    <header
      data-testid="mobile-topbar"
      style={{
        display: "flex",
        "align-items": "center",
        gap: "8px",
        height: "48px",
        "flex-shrink": 0,
        padding: "0 10px",
        background: "var(--surface-1)",
        "border-bottom": "1px solid var(--border-subtle)",
        color: "var(--text-hi)",
      }}
    >
      <IconButton
        icon="menu"
        label="Open sidebar"
        data-testid="mobile-topbar-menu"
        onClick={openSidebar}
        style={{ "flex-shrink": 0 }}
      />
      <span
        style={{
          flex: "1 1 0",
          "min-width": 0,
          "font-size": "14px",
          "font-weight": 600,
          overflow: "hidden",
          "text-overflow": "ellipsis",
          "white-space": "nowrap",
        }}
      >
        {title()}
      </span>
    </header>
  );
}

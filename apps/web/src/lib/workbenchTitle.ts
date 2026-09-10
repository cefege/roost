// Route-aware context text shared by desktop and compact application bars.
// Layout chrome reads this pure projection; terminal routing remains in MainPane.
// Session titles prefer worker-observed OSC metadata over a stable cwd fallback.

import { rootStore } from "../store/root.ts";
import { activeSessionForPath } from "../store/selectors.ts";
import { workerPathBasename } from "./nativePath.ts";

export function workbenchTitle(pathname: string): string {
  if (pathname.startsWith("/search")) return "Search";
  if (pathname.startsWith("/file/")) return "Files";
  if (pathname.startsWith("/settings")) return "Settings";
  if (pathname.startsWith("/help")) return "Help";
  const session = activeSessionForPath(pathname);
  if (session) {
    const title = rootStore.terminal_title[session.id]?.trim();
    return title?.slice(0, 60) || workerPathBasename(session.worker_fp, session.cwd) || "~";
  }
  if (pathname.startsWith("/s/") || pathname.startsWith("/t/") || pathname.startsWith("/w/")) return "Terminal";
  return "Roost";
}

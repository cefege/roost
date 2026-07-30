// Shared terminal-session naming for sidebar and tab surfaces. Prefer the
// terminal's OSC title, then a stable cwd fallback.

import type { Session } from "@roost/shared/wire";
import { rootStore } from "../store/root.ts";
import { shortCwd } from "./sidebarFormat.ts";
import { pathBasename } from "./pathBasename.ts";

const MAX = 80;

export function folderHeadline(session: Session): string {
  const custom = session.custom_title?.trim();
  if (custom) return custom.slice(0, MAX);
  return pathBasename(session.cwd) || "Terminal";
}

/** Structured program detail for an open terminal, if the process reports one. */
export function programSubtitle(session: Session): string | null {
  const osc = rootStore.terminal_title[session.id]?.trim();
  if (osc) return osc.slice(0, MAX);
  return null;
}

export function sessionTitle(session: Session): string {
  const custom = session.custom_title?.trim();
  if (custom) return custom.slice(0, MAX);
  return programSubtitle(session) || shortCwd(session.cwd) || "shell";
}

// The one display name for a session, shared by the sidebar row AND the top
// TabBar so a tab reads the same as its sidebar entry. The pattern
// (titleForTab ?? "Terminal"): prefer the program's own OSC-0/2 title (claude
// sets it to its current task; shells set it to the running command), then
// process-driven sources, then the cwd / Terminal-N fallback.
//
// Reactive: reads rootStore.terminal_title/workspaces + the agent selectors, so
// callers inside a createMemo / JSX track it and the title updates live.

import type { Session } from "@roost/shared/wire";
import { rootStore } from "../store/root.ts";
import { currentToolOf, currentBlockOf, lastMessageOf } from "../store/selectors.ts";
import { shortCwd } from "./sidebarFormat.ts";
import { pathBasename } from "./pathBasename.ts";

const MAX = 80;

// Sidebar headline (the prominent line): the folder this session lives in, or
// the user's rename. The customTitle model — rename wins and is sticky.
// What the PROGRAM reports (claude's task, the running command) is demoted to
// cloudSubtitle() so the folder stays the stable, scannable anchor.
export function folderHeadline(session: Session): string {
  const custom = session.custom_title?.trim();
  if (custom) return custom.slice(0, MAX);
  return pathBasename(session.cwd) || "Terminal";
}

// Sidebar subtitle (the smaller line under the headline): "what cloud says" —
// claude's OSC task title / running tool / command / last message. null when
// there's no process-driven name yet (a fresh shell), so the row skips the line
// instead of echoing the folder. Custom rename does NOT suppress it: a renamed
// row still shows what its agent is doing. Also the single source of the
// program-name cascade — sessionTitle() composes on top of it.
export function cloudSubtitle(session: Session): string | null {
  const osc = rootStore.terminal_title[session.id]?.trim();
  if (osc) return osc.slice(0, MAX);
  const tool = currentToolOf(session);
  if (tool) return tool.name;
  const block = currentBlockOf(session);
  if (block) return `$ ${block.command ?? "running"}`;
  const msg = lastMessageOf(session);
  if (msg) return msg.text.slice(0, MAX);
  return null;
}

export function sessionTitle(session: Session): string {
  // User rename wins over everything (the customTitle model). Sticky: the
  // OSC/agent/cwd auto-title keeps flowing into terminal_title but is never
  // consulted while custom_title is set. Cleared (null) → fall through to auto.
  const custom = session.custom_title?.trim();
  if (custom) return custom.slice(0, MAX);
  // The program-name cascade (osc → tool → block → last message) lives once in
  // cloudSubtitle(); the TabBar title just adds a cwd / Terminal-N tail fallback.
  const program = cloudSubtitle(session);
  if (program) return program;
  // No process-driven name: plain shells show their folder; claude panes keep a
  // stable Terminal-N (their 1-based slot in the workspace) so an idle claude
  // isn't churned.
  if (session.kind !== "claude") return shortCwd(session.cwd) || "shell";
  const wid = session.workspace_id;
  if (wid) {
    const ws = rootStore.workspaces[wid];
    const idx = ws ? ws.session_ids.indexOf(session.id) : -1;
    if (idx >= 0) return `Terminal ${idx + 1}`;
  }
  return "Terminal";
}

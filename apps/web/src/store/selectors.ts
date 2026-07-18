// Derived selectors over rootStore. All are createMemo — reactive, cached.
// Components subscribe to selectors, never mutate rootStore directly.
// R0.4 + R4.3 selector deliverables.

import { createMemo, createRoot } from "solid-js";
import { rootStore } from "./root.ts";
import { decodeFolderPath } from "../lib/terminalHref.ts";
import type { Session } from "@roost/shared/wire";
import { folderKeyOf } from "../lib/folderKey.ts";
import { isPendingClose } from "../lib/pendingClose.ts";
import type { Attention } from "../lib/folderSubtitle.ts";

// Module-level memos must be wrapped in createRoot — without an owner,
// Solid warns "computations created outside a `createRoot` or `render`
// will never be disposed". These are intentional app-lifetime singletons
// so we never dispose the root.

// All sessions as an array (reactive).
export const allSessions = createRoot(() =>
  createMemo(() => Object.values(rootStore.sessions)),
);

// Resolve the live session behind a /t/:workerFp/*folderPath URL: the OPEN
// session on `workerFp` spawned in `folderPath`. Collisions (two terminals in
// one folder — normal, spawnShell always mints a new id) tiebreak to the newest
// created_at. Returns null when nothing live matches → MainPane's safety-net
// effect redirects home. Reads rootStore.sessions so it's reactive in a memo.
// spawn_cwd is the immutable spawn folder; fall back to cwd for pre-migration
// rows that predate the field.
export function resolveSessionByFolder(workerFp: string, folderPath: string): Session | null {
  let best: Session | null = null;
  for (const s of Object.values(rootStore.sessions)) {
    if (s.status !== "open" || s.worker_fp !== workerFp) continue;
    if ((s.spawn_cwd ?? s.cwd) !== folderPath) continue;
    if (!best || s.created_at > best.created_at) best = s;
  }
  return best;
}

// Newest OPEN session belonging to a workspace. null when the workspace has no
// live session (empty workspace) → callers fall back / bounce home.
export function resolveSessionByWorkspace(workspaceId: string): Session | null {
  let best: Session | null = null;
  for (const s of Object.values(rootStore.sessions)) {
    if (s.status !== "open" || s.workspace_id !== workspaceId) continue;
    if (!best || s.created_at > best.created_at) best = s;
  }
  return best;
}

// Newest OPEN session in a (worker, live-cwd) folder bucket, excluding `exceptId`.
// Backs MainPane's safety net: when the viewed terminal ends, land on a sibling
// in the SAME folder instead of Home. folderKey = folderKeyOf() form.
export function newestOpenSessionForFolderKey(folderKey: string, exceptId: string | null): Session | null {
  let best: Session | null = null;
  for (const s of Object.values(rootStore.sessions)) {
    if (s.status !== "open" || s.id === exceptId || isPendingClose(s.id)) continue;
    if (folderKeyOf(s) !== folderKey) continue;
    if (!best || s.created_at > best.created_at) best = s;
  }
  return best;
}

// Which tab a sidebar folder row opens on click. A needy folder jumps to its
// lead (the neediest pane). Otherwise reuse the remembered last-visited tab,
// but ONLY when it is still an OPEN pane IN THIS folder (folderKeyOf === the
// row's bucket key). A pane that closed — or `cd`'d into another folder —
// lingers in the lastVisited map under this folder's key; opening it blindly
// surfaces a DIFFERENT folder (the "click one workspace, another opens" bug).
// Falls back to the folder lead.
export function folderRowTargetId(
  folderKey: string,
  attention: Attention,
  leadId: string,
  rememberedId: string | null,
): string {
  if (attention === "needs") return leadId;
  const r = rememberedId ? rootStore.sessions[rememberedId] : null;
  if (r && r.status === "open" && folderKeyOf(r) === folderKey) return r.id;
  return leadId;
}

// Active session behind ANY terminal route — the single source of truth for
// TabBar / FolderList / MobileTopBar scoping, mirroring MainPane.activeSession.
// /s/:id → by id; /t/:workerFp/*folderPath → by folder; /w/:id → by workspace.
// null off a terminal route (/settings, /search, /) — callers fall back.
export function activeSessionForPath(pathname: string): Session | null {
  const s = pathname.match(/^\/s\/([^/]+)/);
  if (s) return rootStore.sessions[s[1]] ?? null;
  const t = pathname.match(/^\/t\/([^/]+)\/(.*)$/);
  if (t) return resolveSessionByFolder(t[1], decodeFolderPath(t[2]));
  const w = pathname.match(/^\/w\/([^/]+)/);
  if (w) return resolveSessionByWorkspace(w[1]);
  return null;
}

// Per-session accessors (not memos — callers wrap in createMemo if needed).
export function lastMessageOf(session: Session) {
  return session.agent?.last_message ?? null;
}
export function currentToolOf(session: Session) {
  return session.agent?.current_tool ?? null;
}
export function currentBlockOf(session: Session) {
  return session.agent?.current_block ?? null;
}

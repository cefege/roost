// Folder identity for a session — the (worker, folder) bucket shared by the
// TabBar tab strip AND the sidebar's FolderList. Both MUST group by
// the SAME key so "click a folder row in the sidebar" surfaces exactly that
// folder's tabs in the tab bar. folder = workspace folder_path if the session
// is linked to a workspace, else the session's raw cwd.
// Callers: TabBar.tsx, components/sidebar/FolderList.tsx.

import type { Session } from "@roost/shared/wire";
import { rootStore } from "../store/root.ts";

/** The folder a session lives in = its live cwd. A `cd` re-homes the session to
 *  the new folder so a workspace emptied by the move drops out of the sidebar
 *  (workspace IS a folder = wherever the terminal currently is). Naming still
 *  tracks the workspace via workspaceForFolder(worker, cwd) below. */
export function folderPathOf(session: Session): string {
  return session.cwd;
}

/** Stable per-(worker, folder) key. Same-folder sessions collapse into one
 *  tab strip / one sidebar folder row. */
export function folderKeyOf(session: Session): string {
  return `${session.worker_fp}::${folderPathOf(session)}`;
}

/** The workspace backing a (worker, folder) bucket, if any — resolved by
 *  (worker_fp, folder_path), NOT by session.workspace_id. Resolving by folder is
 *  what makes a folder's custom name show regardless of which session leads the
 *  row, and it re-attaches the name when a cd'd-away terminal returns to the
 *  workspace's folder. */
export function workspaceForFolder(workerFp: string, folderPath: string) {
  for (const ws of Object.values(rootStore.workspaces)) {
    if (ws.worker_fp === workerFp && ws.folder_path === folderPath) return ws;
  }
  return null;
}

/** Human label for a folder row: the folder's workspace name if it's been
 *  renamed (FolderRowContextMenu → workspacesCreate/Update), else the folder's
 *  BASENAME (last path segment) — just "idea", not "you/roost" or the full
 *  path. Author 2026-07-04: workspace view should show only the folder name. */
export function folderDisplayName(session: Session): string {
  const path = folderPathOf(session);
  const ws = workspaceForFolder(session.worker_fp, path);
  if (ws?.name) return ws.name;
  const parts = path.split("/").filter(Boolean);
  return parts[parts.length - 1] ?? path;
}

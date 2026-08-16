// Folder identity for the sidebar and terminal pane deck. Terminal tabs follow
// their live cwd after `cd`.

import type { Session } from "@roost/shared/wire";
import { rootStore } from "../store/root.ts";
import { sameWorkerPath, workerPathBasename, workerPathIdentity } from "./nativePath.ts";

/** Terminals live in their current cwd. */
export function folderPathOf(session: Session): string {
  return session.cwd;
}

/** Stable per-(worker, folder) key. Same-folder sessions collapse into one
 *  tab strip / one sidebar folder row. */
export function folderKeyOf(session: Session): string {
  return `${session.worker_fp}::${workerPathIdentity(session.worker_fp, folderPathOf(session))}`;
}

/** The workspace backing a (worker, folder) bucket, if any — resolved by
 *  (worker_fp, folder_path), NOT by session.workspace_id. Resolving by folder is
 *  what makes a folder's custom name show regardless of which session leads the
 *  row, and it re-attaches the name when a cd'd-away terminal returns to the
 *  workspace's folder. */
export function workspaceForFolder(workerFp: string, folderPath: string) {
  for (const ws of Object.values(rootStore.workspaces)) {
    if (ws.worker_fp === workerFp && sameWorkerPath(workerFp, ws.folder_path, folderPath)) return ws;
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
  return workerPathBasename(session.worker_fp, path) || path;
}

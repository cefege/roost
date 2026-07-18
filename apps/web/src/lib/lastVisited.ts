// Browser-local "where was I" memory (NOT server-side, per Author 2026-07-06).
// Two things, both localStorage:
//   1. Last terminal you viewed → boot restores straight into it (App.tsx
//      WorkspaceRedirect). Stored as the STABLE /t/ href (terminalHref) so it
//      survives a session death+respawn in that folder; a truly-dead one is
//      caught by MainPane's safety net → home.
//   2. Last session per folder/workspace → clicking a folder row reopens the
//      tab you were last on there, not just the lead (FolderList.tsx).
// Written from MainPane whenever a live terminal is on screen.
//
// Perf: the folder map lives in a module-level signal loaded ONCE at import —
// getLastSessionForFolder used to localStorage.getItem + JSON.parse the whole
// map on EVERY call, and FolderList calls it per row per rebuild. Reads are
// reactive (signal); rememberVisit updates the signal then persists, skipping
// both writes when the stored values are already identical.

import { createSignal } from "solid-js";
import type { Session } from "@roost/shared/wire";
import { terminalHref } from "./terminalHref.ts";

const PATH_KEY = "roost.lastTerminalPath";
const FOLDER_KEY = "roost.lastSessionByFolder";

function folderKey(workerFp: string, folder: string): string {
  return `${workerFp}\u0000${folder}`;
}

function readFolderMap(): Record<string, string> {
  try {
    const raw = localStorage.getItem(FOLDER_KEY);
    const obj = raw ? JSON.parse(raw) : null;
    return obj && typeof obj === "object" ? obj as Record<string, string> : {};
  } catch { return {}; }
}

const [_folderMap, _setFolderMap] = createSignal<Record<string, string>>(readFolderMap());
let _lastPathWritten: string | null = null;

// Record the terminal you're currently viewing: the boot-restore path + the
// per-folder last-tab. Called on every active-terminal change.
export function rememberVisit(session: Session): void {
  const folder = session.spawn_cwd ?? session.cwd;
  try {
    const href = terminalHref(session);
    if (href !== _lastPathWritten) {
      localStorage.setItem(PATH_KEY, href);
      _lastPathWritten = href;
    }
    const key = folderKey(session.worker_fp, folder);
    const map = _folderMap();
    if (map[key] === session.id) return; // already stored — skip the write
    const next = { ...map, [key]: session.id };
    _setFolderMap(next);
    localStorage.setItem(FOLDER_KEY, JSON.stringify(next));
  } catch { /* quota / privacy mode */ }
}

export function getLastTerminalPath(): string | null {
  try {
    const p = localStorage.getItem(PATH_KEY);
    return p && p !== "/" ? p : null;
  } catch { return null; }
}

export function getLastSessionForFolder(workerFp: string, folder: string): string | null {
  return _folderMap()[folderKey(workerFp, folder)] ?? null;
}

// Stable terminal URL helpers. The /t/:workerFp/*folderPath route keys a
// terminal on (server, spawn folder) instead of the ephemeral session id, so a
// bookmark survives a session death+respawn in the same folder.
//
// Called by: terminalHref (build), MainPane/selectors (decode). All native path
// semantics live in lib/nativePath; these route-named wrappers preserve the
// established public helper names used by smoke/tests.

import type { Session } from "@roost/shared/wire";
import { decodeWorkerPathRoute, encodeWorkerPathRoute } from "./nativePath.ts";

// Absolute folder path → splat segment. POSIX bytes remain unchanged; Windows
// drive/UNC roots use the shared codec's explicit tagged route prefixes.
export function encodeFolderPath(abs: string, workerFp = ""): string {
  return encodeWorkerPathRoute(workerFp, abs);
}

// Splat param → canonical worker path. A tagged Windows route can decode before
// the independently-hydrated workers domain has published its OS record.
export function decodeFolderPath(splat: string, workerFp = ""): string {
  return decodeWorkerPathRoute(workerFp, splat);
}

// Build the stable URL for a session. Falls back to /s/:id for pre-migration
// sessions with no spawn_cwd (older rows that predate the field).
export function terminalHref(session: Session): string {
  const folder = session.spawn_cwd ?? null;
  if (!folder) return `/s/${session.id}`;
  return `/t/${session.worker_fp}/${encodeFolderPath(folder, session.worker_fp)}`;
}

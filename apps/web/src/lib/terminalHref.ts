// Stable terminal URL helpers. The /t/:workerFp/*folderPath route keys a
// terminal on (server, spawn folder) instead of the ephemeral session id, so a
// bookmark survives a session death+respawn in the same folder.
//
// Called by: CommandPalette.pickFolder (builds the href on open), MainPane +
// AgentStatusBar (decode the splat → resolveSessionByFolder). Encoding mirrors
// the /file/ link builder in CellTerminal.tsx:90 — per-segment
// encodeURIComponent, leading slash stripped (the @solidjs/router *splat drops
// it and re-adds none).

import type { Session } from "@roost/shared/wire";

// Absolute folder path → splat segment. Per-segment encode, strip the leading
// slash (router splats carry no leading slash).
export function encodeFolderPath(abs: string): string {
  return abs.split("/").map((s) => (s ? encodeURIComponent(s) : s)).join("/").replace(/^\//, "");
}

// Splat param → absolute folder path. Re-add the leading slash the router
// stripped, decode each segment.
export function decodeFolderPath(splat: string): string {
  const inner = splat.split("/").map((s) => (s ? decodeURIComponent(s) : s)).join("/").replace(/^\//, "");
  return "/" + inner;
}

// Build the stable URL for a session. Falls back to /s/:id for pre-migration
// sessions with no spawn_cwd (older rows that predate the field).
export function terminalHref(session: Session): string {
  const folder = session.spawn_cwd ?? null;
  if (!folder) return `/s/${session.id}`;
  return `/t/${session.worker_fp}/${encodeFolderPath(folder)}`;
}

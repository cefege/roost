// Maps a raw mkdir/list failure — errno text from the worker or a bracketed
// Connect status code from the coordinator — onto one sentence the picker can
// show in place. Unrecognized failures pass through verbatim so no diagnostic
// detail is lost; the raw text is what gets logged, this is what gets read.
//
// Callers: browseDirectoryListing.ts, WorkerBrowsePage.tsx (commitNewFolder).

// Ordered: the first needle found in the lower-cased message wins.
const BROWSE_ERROR_COPY: ReadonlyArray<readonly [readonly string[], string]> = [
  [["eacces", "eperm"], "Permission denied on this machine."],
  [["enospc"], "The machine is out of disk space."],
  [["erofs"], "That location is read-only."],
  [["eexist"], "A file with that name already exists here."],
  [["enotdir"], "Part of that path isn't a folder."],
  [["enametoolong"], "That path is too long for this machine."],
  [["enoent"], "That folder no longer exists."],
  [
    ["worker not connected", "worker offline", "[unavailable]", "[failed_precondition]"],
    "The machine went offline. Reconnect and try again.",
  ],
  [["did not reply", "[deadline_exceeded]"], "The machine didn't respond. Try again."],
  [
    ["[unauthenticated]", "authentication required"],
    "Your session expired. Reload to sign in again.",
  ],
  [["worker not found", "[not_found]"], "This machine is no longer registered."],
  [["only accepts relative child parts"], "Folder names can't contain / or \\."],
];

export function browseErrorMessage(error: unknown): string {
  const raw = error instanceof Error ? error.message : String(error);
  const lowered = raw.toLowerCase();
  for (const [needles, message] of BROWSE_ERROR_COPY) {
    if (needles.some((needle) => lowered.includes(needle))) return message;
  }
  return raw;
}

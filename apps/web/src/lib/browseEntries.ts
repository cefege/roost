// Entry selection for the /browse folder picker: which listed names are
// visible under the in-folder filter, in which order, and how a modification
// time reads as relative text. Pure string/array math so the picker's
// components stay wiring-only and this logic is unit-testable.
//
// Callers: BrowseEntryList.tsx, WorkerBrowsePage.tsx.

export interface BrowseEntry { name: string; isDir: boolean; mtimeMs: number }

export function visibleFolders(entries: readonly BrowseEntry[], filter: string): BrowseEntry[] {
  return selectEntries(entries, filter, true);
}

export function visibleFiles(entries: readonly BrowseEntry[], filter: string): BrowseEntry[] {
  return selectEntries(entries, filter, false);
}

// Relative modification time for an entry row. `now` is injectable so the
// thresholds are testable without a clock.
export function relativeEntryTime(mtimeMs: number, now: number = Date.now()): string {
  if (mtimeMs <= 0) return "";
  const diff = now - mtimeMs;
  if (diff < 60_000) return "just now";
  if (diff < 3_600_000) return `${Math.floor(diff / 60_000)}m ago`;
  if (diff < 86_400_000) return `${Math.floor(diff / 3_600_000)}h ago`;
  if (diff < 604_800_000) return `${Math.floor(diff / 86_400_000)}d ago`;
  return new Date(mtimeMs).toLocaleDateString(undefined, { month: "short", day: "numeric" });
}

function selectEntries(
  entries: readonly BrowseEntry[],
  filter: string,
  wantDir: boolean,
): BrowseEntry[] {
  const needle = filter.trim().toLowerCase();
  return entries
    .filter((entry) => {
      if (entry.isDir !== wantDir) return false;
      if (entry.name.startsWith(".")) return false;
      return needle === "" || entry.name.toLowerCase().includes(needle);
    })
    .sort((left, right) => left.name.toLowerCase().localeCompare(right.name.toLowerCase()));
}

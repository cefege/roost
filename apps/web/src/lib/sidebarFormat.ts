// Shared sidebar text formatters. Used by SessionRow (row labels) and
// FolderList (folder-group headers) so the cwd / server shortening
// is identical everywhere.

// `/Users/you/Code/roost` → `you/roost`
// `/home/user/repos/foo`            → `user/foo`
// `/Users/you`                     → `you`   ·   `/` → `/`
export function shortCwd(cwd: string): string {
  const parts = cwd.split("/").filter(Boolean);
  if (parts.length === 0) return "/";
  const last = parts[parts.length - 1];
  const userIdx = parts.findIndex((p) => p === "Users" || p === "home");
  const user = userIdx >= 0 && userIdx + 1 < parts.length ? parts[userIdx + 1] : null;
  if (!user || user === last) return last;
  return `${user}/${last}`;
}

// Drop a leading owner segment (all-letters, e.g. "mike"/"luci") when the
// machine name still has ≥2 segments left: "worker-host" → "m1-air-old".
export function shortServerLabel(label: string): string {
  const clean = label.replace(/\.local$/, "");
  const parts = clean.split("-");
  return parts.length > 2 && /^[a-z]+$/i.test(parts[0]) ? parts.slice(1).join("-") : clean;
}

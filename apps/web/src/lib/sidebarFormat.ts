// Shared sidebar text formatters. Used by SessionRow (row labels) and
// FolderList (folder-group headers) so the cwd / server shortening
// is identical everywhere.
import { shortWorkerPath } from "./nativePath.ts";

// `/Users/you/Code/roost` → `you/roost`
// `/home/user/repos/foo`  → `user/foo`
// `C:/Users/you/roost`    → `you/roost`
// Root/share paths retain their native root label.
export function shortCwd(cwd: string, workerFp = ""): string {
  return shortWorkerPath(workerFp, cwd);
}

// Drop a leading owner segment (all-letters, e.g. "mike"/"luci") when the
// machine name still has ≥2 segments left: "worker-host" → "m1-air-old".
export function shortServerLabel(label: string): string {
  const clean = label.replace(/\.local$/, "");
  const parts = clean.split("-");
  return parts.length > 2 && /^[a-z]+$/i.test(parts[0]) ? parts.slice(1).join("-") : clean;
}

// Worker-native path navigation for /browse. Browser components supply the
// worker fingerprint; all separator/root rules delegate to lib/nativePath.

import { joinWorkerPath, workerPathCrumbs, workerPathDirname } from "./nativePath.ts";

export function childPath(workerFp: string, dir: string, name: string): string {
  return joinWorkerPath(workerFp, dir, name);
}

export interface Crumb { label: string; path: string }

// Breadcrumb segments for the visual file-browser header. Each crumb carries
// the cumulative canonical path so drive and UNC roots remain reversible.
export function pathCrumbs(workerFp: string, dir: string): Crumb[] {
  if (!dir) return [];
  return workerPathCrumbs(workerFp, dir);
}

export type CrumbView =
  | { kind: "crumb"; label: string; path: string }
  | { kind: "ellipsis"; hidden: Crumb[] };

// Fold `hideMiddle` of the MIDDLE crumbs (between root and parent+current) into
// one ellipsis token, hiding from the LEFT of the middle so the segments nearest
// the current folder stay visible longest. 0 → no ellipsis, all visible.
// crumbs.length <= 3 → no middle possible → returned unchanged. This is pure
// string math; the COMPONENT decides `hideMiddle` from measured widths.
export function collapseCrumbsTo(crumbs: Crumb[], hideMiddle: number): CrumbView[] {
  if (crumbs.length <= 3) {
    return crumbs.map((c) => ({ kind: "crumb", label: c.label, path: c.path }));
  }
  const middle = crumbs.slice(1, crumbs.length - 2);
  const k = Math.max(0, Math.min(hideMiddle, middle.length));
  const out: CrumbView[] = [{ kind: "crumb", label: crumbs[0].label, path: crumbs[0].path }];
  if (k > 0) out.push({ kind: "ellipsis", hidden: middle.slice(0, k) });
  for (const c of crumbs.slice(1 + k)) out.push({ kind: "crumb", label: c.label, path: c.path });
  return out;
}

// Parent dir for the Up button. Native roots and the browse `~` sentinel stay
// put; shared native-path owns drive/UNC/POSIX root behavior.
export function parentPath(workerFp: string, dir: string): string {
  if (!dir || dir === "~") return dir;
  return workerPathDirname(workerFp, dir);
}

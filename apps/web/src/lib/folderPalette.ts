// Path-nav helpers for the /browse file manager.
// Pure string math: build child paths, breadcrumb segments, parent dirs.
// Called by: BrowsePage.tsx (click-driven nav).
// Tested by: apps/web/tests/folderPalette.test.ts.

export function childPath(dir: string, name: string): string {
  return dir === "/" ? "/" + name : dir.replace(/\/+$/, "") + "/" + name;
}

export interface Crumb { label: string; path: string }

// Breadcrumb segments for the visual file-browser header. Each crumb carries
// the cumulative path so clicking it jumps there. "/Users/you" → / ▸ Users ▸
// mike ; "~/Code/roost" → ~ ▸ Code ▸ roost.
export function pathCrumbs(dir: string): Crumb[] {
  if (!dir) return [];
  const abs = dir.startsWith("/");
  const segs = dir.split("/").filter(Boolean);
  const out: Crumb[] = abs ? [{ label: "/", path: "/" }] : [];
  let acc = "";
  for (const s of segs) {
    acc = acc === "" ? (abs ? "/" + s : s) : acc + "/" + s;
    out.push({ label: s, path: acc });
  }
  return out;
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

// Parent dir for the "Up" button. "/Users/you" → "/Users" ; "~/Code" → "~" ;
// "~" and "/" are roots (stay put).
export function parentPath(dir: string): string {
  if (!dir || dir === "/" || dir === "~") return dir;
  const idx = dir.lastIndexOf("/");
  return idx <= 0 ? (dir.startsWith("/") ? "/" : "~") : dir.slice(0, idx);
}

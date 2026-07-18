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

// Parent dir for the "Up" button. "/Users/you" → "/Users" ; "~/Code" → "~" ;
// "~" and "/" are roots (stay put).
export function parentPath(dir: string): string {
  if (!dir || dir === "/" || dir === "~") return dir;
  const idx = dir.lastIndexOf("/");
  return idx <= 0 ? (dir.startsWith("/") ? "/" : "~") : dir.slice(0, idx);
}

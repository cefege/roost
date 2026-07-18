// Trailing-slash-tolerant basename. Shared by the workspace-create
// flow (name derivation) and the sidebar (group label + collision
// suppression so the new orphan-by-cwd-basename grouping in
// MachineSection lines up exactly with workspace folder_path basenames
// from spawnSession). Keep these consumers in sync; do NOT inline.

export function pathBasename(p: string): string {
  const t = p.replace(/\/+$/, "");
  const i = t.lastIndexOf("/");
  return (i === -1 ? t : t.slice(i + 1)) || p;
}

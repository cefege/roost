// Optimistic new-terminal spawn. The browser mints the session id and inserts
// a client-only shell placeholder so the shared pane deck can paint the tab
// immediately. The real spawn reuses that id and replaces the placeholder.

import { createSignal } from "solid-js";
import { asSessionId, asChannelId } from "@roost/shared/wire";
import type { Session } from "@roost/shared/wire";
import { rootStore, setRootStore } from "./root.ts";
import { addToast } from "../lib/toastStore.ts";

// Reactive so CellTerminal's `pending` memo re-runs when a placeholder resolves.
// `aborted` needs no reactivity — it is read imperatively in doNewTab's resolve.
const [pendingIds, setPendingIds] = createSignal<ReadonlySet<string>>(new Set());
const aborted = new Set<string>();

export function isPendingSpawn(id: string): boolean {
  return pendingIds().has(id);
}
export function wasAborted(id: string): boolean {
  return aborted.has(id);
}
export function clearAborted(id: string): void {
  aborted.delete(id);
}

// Insert a shell placeholder in the bucket selected by the anchor tab.
export function beginOptimisticSpawn(anchor: Session): string {
  const id = crypto.randomUUID();
  const folder = anchor.cwd;
  const placeholder: Session = {
    id: asSessionId(id),
    worker_fp: anchor.worker_fp,
    channel: asChannelId(0), // sentinel; real channel arrives on `opened`
    kind: "shell",
    cwd: folder,
    spawn_cwd: folder,
    workspace_id: anchor.workspace_id ?? null,
    status: "open",
    created_at: Date.now(), // liveIds sorts by this → appends last
    closed_at: null,
    custom_title: null,
  };
  setRootStore("sessions", id, placeholder);
  setPendingIds((s) => {
    const n = new Set(s);
    n.add(id);
    return n;
  });
  return id;
}

// Spawn confirmed: drop pending so CellTerminal fires its INITIAL claim + paints.
// The `opened` event has already replaced the value at this id (stable key → no
// remount), so the terminal is live under the same tab.
export function endOptimisticSpawn(id: string): void {
  clearPending(id);
}

// Spawn failed: remove the placeholder (reconcile prunes the tab) + toast, unless
// the user already aborted (closed the pending tab) — then it's an expected removal.
export function failOptimisticSpawn(id: string, err: unknown): void {
  const wasAbortedNow = aborted.has(id);
  removePlaceholder(id);
  clearPending(id);
  aborted.delete(id);
  if (!wasAbortedNow) {
    addToast(`New terminal failed: ${err instanceof Error ? err.message : String(err)}`, "err");
  }
}

// User closed the placeholder before the spawn resolved: remove it now; the
// caller reaps the real PTY once the in-flight spawn lands (see doClose + doNewTab).
export function abortOptimisticSpawn(id: string): void {
  aborted.add(id);
  removePlaceholder(id);
  clearPending(id);
}

function removePlaceholder(id: string): void {
  if (rootStore.sessions[id]) setRootStore("sessions", id, undefined as unknown as Session);
}
function clearPending(id: string): void {
  setPendingIds((s) => {
    if (!s.has(id)) return s;
    const n = new Set(s);
    n.delete(id);
    return n;
  });
}

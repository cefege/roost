// Optimistic new-terminal spawn. The browser mints the session id, inserts a
// client-only placeholder shell session into the store so the reactive deck
// renders the tab + pane instantly, and marks it pending. The real spawn RPC
// carries the SAME id; the worker's `opened` event replaces the placeholder
// value in place (stable store key → the <For> row and its CellTerminal never
// remount, so WASM init started at click time is preserved). Pending clears on
// RPC-resolve, at which point CellTerminal fires its INITIAL viewport claim and
// paints the shell. On failure the placeholder is removed (reconcile prunes the
// tab) and a toast is shown.

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

// Mint the id the real spawn will reuse, insert a client-only placeholder shell
// session so the deck renders the tab instantly, mark it pending. `anchor` is the
// pane's current session — its worker_fp + cwd fix the folder bucket via
// folderKeyOf, so reconcile appends the placeholder into the focused pane.
export function beginOptimisticSpawn(anchor: Session): string {
  const id = crypto.randomUUID();
  const placeholder: Session = {
    id: asSessionId(id),
    worker_fp: anchor.worker_fp,
    channel: asChannelId(0), // sentinel; real channel arrives on `opened`
    kind: "shell",
    cwd: anchor.cwd, // folderKeyOf(placeholder) === folderKeyOf(anchor)
    spawn_cwd: anchor.cwd,
    workspace_id: anchor.workspace_id ?? null,
    status: "open",
    agent: null, // D-3 invariant: shell ⇒ agent null
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

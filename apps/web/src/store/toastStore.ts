// Toast notification store. Module-level signals; no rootStore dependency.
// addToast(msg, kind, opts) appends a toast that auto-dismisses on its own
// timer — ok after 3s, warn after 5s, err after 8s. Pass ttlMs:null for the
// rare must-not-vanish toast; the card's Copy button is what preserves error
// text, not the timer.
// Consumers: ToastStack.tsx / ToastCard.tsx inside the notification dock.

import { createSignal } from "solid-js";

export type ToastKind = "ok" | "warn" | "err";

export interface ToastAction {
  label: string;
  onClick: () => void;
}

export interface Toast {
  id: number;
  msg: string;
  kind: ToastKind;
  /** Multi-line follow-up (e.g. deploy tail). Renders below msg in a
   *  monospace block, full text selectable, with a Copy button. */
  details?: string;
  /** Auto-dismiss timer in ms, or `null` to stay until the user closes
   *  the toast. Defaults: ok=3000, warn=5000, err=8000. */
  ttlMs: number | null;
  /** Optional inline action button (e.g. "Jump to it" for attention toasts).
   *  Rendered to the left of Copy/✕. The action does NOT auto-dismiss the
   *  toast — the caller's onClick is responsible for dismissToast if desired. */
  action?: ToastAction;
}

export interface AddToastOptions {
  details?: string;
  ttlMs?: number | null;
  action?: ToastAction;
}

let nextId = 1;

const [toasts, setToasts] = createSignal<Toast[]>([]);

export { toasts };

const DEFAULT_TTL: Record<ToastKind, number | null> = {
  ok: 3000,
  warn: 5000,
  // Errors get the longest window but still expire: the card's Copy button is
  // the durable path to the text. ttlMs:null is the explicit opt-out.
  err: 8000,
};

/**
 * Append a toast and return a `dismiss` fn the caller can use to close
 * it early (e.g. flip an in-progress toast into a final result toast).
 */
export function addToast(
  msg: string,
  kind: ToastKind = "ok",
  opts: AddToastOptions = {},
): () => void {
  const id = nextId++;
  const ttlMs = opts.ttlMs !== undefined ? opts.ttlMs : DEFAULT_TTL[kind];
  setToasts((prev) => [...prev, { id, msg, kind, details: opts.details, ttlMs, action: opts.action }]);
  const dismiss = () => setToasts((prev) => prev.filter((t) => t.id !== id));
  if (ttlMs !== null) setTimeout(dismiss, ttlMs);
  return dismiss;
}

export function dismissToast(id: number): void {
  setToasts((prev) => prev.filter((t) => t.id !== id));
}

export function clearToastsForAccountBoundary(): void {
  setToasts([]);
}

// Ephemeral top-right attention popups. Module-level signal + auto-dismiss
// via setTimeout, modeled on toastStore.ts. Distinct from notifyStore's
// persistent 50-item log (the bell dropdown source): these are transient
// cards that AttentionToasts.tsx renders top-right and auto-dismiss.

import { createSignal } from "solid-js";
import type { AttentionNotification } from "./notifyStore.ts";

export interface AttentionToast {
  id: number;          // ephemeral popup id
  notifId: number;     // AttentionNotification.id — for markRead on click
  sessionId: string;
  title: string;       // session title (AttentionNotification.sessionTitle)
  kind: AttentionNotification["kind"]; // "done" | "blocked" | "offline"
  ttlMs: number | null;
}

let nextId = 1;
const [attentionToasts, setAttentionToasts] = createSignal<AttentionToast[]>([]);
export { attentionToasts };

// blocked (needs-input) persists until clicked/dismissed; done/offline auto-dismiss.
const TTL: Record<AttentionToast["kind"], number | null> = {
  blocked: null,
  done: 6000,
  offline: 6000,
};

const MAX_VISIBLE = 4;

export function pushAttentionToast(n: AttentionNotification): void {
  const id = nextId++;
  const ttlMs = TTL[n.kind];
  // Coalesce by session (mirrors the OS push `tag` behavior): one card per session.
  setAttentionToasts((prev) =>
    [{ id, notifId: n.id, sessionId: n.sessionId, title: n.sessionTitle, kind: n.kind, ttlMs },
     ...prev.filter((t) => t.sessionId !== n.sessionId)].slice(0, MAX_VISIBLE),
  );
  if (ttlMs !== null) setTimeout(() => dismissAttentionToast(id), ttlMs);
}

export function dismissAttentionToast(id: number): void {
  setAttentionToasts((prev) => prev.filter((t) => t.id !== id));
}

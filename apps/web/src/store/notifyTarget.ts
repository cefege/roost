// Which live surface a hovered notification points at. One module-level signal,
// written by ToastCard's pointer/focus handlers and read by the pane tab and the
// three sidebar row surfaces. Hover is pointer-rate state, so it is unlogged —
// same treatment as store/spotlight.ts.

import { createSignal } from "solid-js";
import { folderKeyOf } from "../lib/folderKey.ts";
import { rootStore } from "./root.ts";

interface NotifyTargetHold {
  readonly toastId: number;
  readonly sessionId: string;
}

const [notifyTargetHold, setNotifyTargetHold] = createSignal<NotifyTargetHold | null>(null);

export function notifyTargetSessionId(): string | null {
  return notifyTargetHold()?.sessionId ?? null;
}

/** The folder whose sidebar row stands in for the target when no pane tab does. */
export function notifyTargetFolderKey(): string | null {
  const sessionId = notifyTargetSessionId();
  if (!sessionId) return null;
  const session = rootStore.sessions[sessionId];
  return session ? folderKeyOf(session) : null;
}

export function holdNotifyTarget(toastId: number, sessionId: string): void {
  setNotifyTargetHold({ toastId, sessionId });
}

/** Ownership-guarded: a late leave from a superseded toast must not clear the
 *  hold its successor just took. */
export function releaseNotifyTarget(toastId: number): void {
  setNotifyTargetHold((current) => (current?.toastId === toastId ? null : current));
}

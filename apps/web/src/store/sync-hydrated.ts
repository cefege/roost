// Has the bootstrap's sessionsList snapshot been applied to the store yet?
//
// A leaf module on purpose. sync-bootstrap.ts owns the write, but the firehose
// (sync.ts) must READ it to hold deltas that arrive before the snapshot — and
// sync-bootstrap already imports sync.ts, so the flag cannot live there without
// an import cycle. Consumers: sync.ts (pre-hydration queue), and MainPane's
// dead-URL safety net + stuck-terminal watcher via sync-bootstrap's re-export,
// which use it to tell "still bootstrapping" from "genuinely gone".

import { createSignal } from "solid-js";

export type TerminalBootstrapStage =
  | "identity"
  | "authorization"
  | "sync"
  | "sessions"
  | "ready";

const [terminalBootstrapStage, setTerminalBootstrapStage] =
  createSignal<TerminalBootstrapStage>("identity");

const [sessionsHydrated, setSessionsHydrated] = createSignal(false);
/** A dashboard switch has no valid terminal snapshot until the next selected
 * scope finishes Sync hydration. */
export function resetSyncHydration(): void {
  setSessionsHydrated(false);
  setTerminalBootstrapStage("sync");
}


export {
  sessionsHydrated,
  setSessionsHydrated,
  terminalBootstrapStage,
  setTerminalBootstrapStage,
};

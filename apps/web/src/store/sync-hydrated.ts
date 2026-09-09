// Tracks whether the terminal and worker bootstrap snapshots are authoritative.
//
// This leaf avoids the sync-bootstrap ↔ sync.ts import cycle. Domain hydrators
// publish the flags; a credential boundary resets them before records clear.
// Route guards read the matching domain instead of inferring one from another.

import { createSignal } from "solid-js";

export type TerminalBootstrapStage =
  | "identity"
  | "sync"
  | "sessions"
  | "ready";

const [terminalBootstrapStage, setTerminalBootstrapStage] =
  createSignal<TerminalBootstrapStage>("identity");

const [sessionsHydrated, setSessionsHydrated] = createSignal(false);
const [workersHydrated, setWorkersHydrated] = createSignal(false);
/** A credential boundary has no valid terminal or worker snapshot until each
 * domain finishes hydration under the next accepted socket. */
export function resetSyncHydration(): void {
  setSessionsHydrated(false);
  setWorkersHydrated(false);
  setTerminalBootstrapStage("sync");
}

export {
  sessionsHydrated,
  setSessionsHydrated,
  workersHydrated,
  setWorkersHydrated,
  terminalBootstrapStage,
  setTerminalBootstrapStage,
};

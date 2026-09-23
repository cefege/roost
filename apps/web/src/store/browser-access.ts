// Owns every transition of rootStore.browser_access_state, the gate App.tsx's
// RootShell renders on. sync-bootstrap calls these from its device-rejection
// classification and from the protected sessions snapshot publish; the
// credential-boundary reset back to "checking" lives in root.ts.
// Depends on auth-boundary for teardown and on the runtimes the recovery edge restores.

import { diag, signal } from "@roost/shared/diag";
import { rootStore, setRootStore, type BrowserAccessState } from "./root.ts";
import { suspendAuthenticatedClientState } from "./auth-boundary.ts";
import { loadAgentConfig } from "../lib/agents.ts";
import { resumeContentSearchRuntimeAfterAuthBoundary } from "../lib/globalContentSearchRuntime.ts";

export type BrowserAccessSource =
  | "bootstrap_probe"
  | "sessions_hydration"
  | "sync_rejection"
  | "workers_refresh";

/** The coordinator classified this browser's device key as rejected. */
export function markBrowserDeviceRejected(source: BrowserAccessSource): void {
  const previous = rootStore.browser_access_state;
  if (previous === "unauthorized") return;
  // Only the loss edge tears down and signals; a persistently unknown browser
  // must not emit another relogin event on every visibility refresh.
  signal("auth.relogin_401", {});
  // Teardown resets the state to "checking", so the transition is written after it.
  suspendAuthenticatedClientState();
  transitionBrowserAccess(previous, "unauthorized", source);
}

/** The protected sessions snapshot published: this browser holds authority. */
export function markProtectedSnapshotPublished(): void {
  const previous = rootStore.browser_access_state;
  if (previous === "authorized") return;
  // The rejection teardown latched content search off and dropped agent
  // config; this recovery edge is the only one that restores them without a
  // full reload.
  if (previous === "unauthorized") {
    resumeContentSearchRuntimeAfterAuthBoundary();
    void loadAgentConfig();
  }
  transitionBrowserAccess(previous, "authorized", "sessions_hydration");
}

function transitionBrowserAccess(
  previous: BrowserAccessState,
  next: BrowserAccessState,
  source: BrowserAccessSource,
): void {
  setRootStore("browser_access_state", next);
  diag("auth.browser_access", { from: previous, to: next, source });
}

// Pure decisions behind the coordinator-move UI. apps/web runs no jsdom by
// design (Solid resolves to its SSR build under `bun test`), so components
// cannot be rendered in tests — these exports carry the logic worth defending
// and are covered by apps/web/tests/coordinatorMove.test.ts.

import { CoordinatorMovePhase } from "@roost/shared/proto/coordinator_pb";

export type CoordinatorRole = "yes" | "no" | "unknown";

/** Is this worker the machine currently hosting the coordinator?
 *
 *  "unknown" is a real answer, not a failure: `public_url` is optional on the
 *  coord config and `authCoordIdentity` returns `publicUrl ?? ""`, and
 *  `coord_identity` is null until that RPC settles. Collapsing those into "no"
 *  is what made the pane offer to move the coordinator onto the machine that
 *  already hosts it, with the "Coordinator" pill rendered nowhere.
 *
 *  Neither side arrives normalised: `public_url` can come straight from
 *  ROOST_COORDINATOR_PUBLIC_URL and `reachable_addr` from ROOST_REACHABLE_ADDR.
 *  WHATWG `URL` keeps a MagicDNS trailing dot ("https://mac.ts.net./" →
 *  hostname "mac.ts.net."), and a reachable addr may carry a :port, so both
 *  sides are stripped here rather than assumed clean upstream. */
export function coordinatorRole(
  coordIdentity: { public_url?: string | null } | null | undefined,
  worker: { reachable_addr?: string | null },
): CoordinatorRole {
  const url = coordIdentity?.public_url;
  if (!coordIdentity || !url) return "unknown";
  let host: string;
  try { host = new URL(url).hostname.toLowerCase().replace(/\.$/, ""); }
  catch { return "unknown"; }
  if (!host) return "unknown";
  const addr = worker.reachable_addr;
  if (!addr) return "no";
  const normalized = addr.toLowerCase().replace(/:\d+$/, "").replace(/\.$/, "");
  return normalized === host ? "yes" : "no";
}

export function isFailedMovePhase(phase: CoordinatorMovePhase): boolean {
  return phase === CoordinatorMovePhase.ROLLED_BACK || phase === CoordinatorMovePhase.FAILED;
}

/** Consecutive status-poll failures after which the dialog gives up polling
 *  and offers a manual Retry. 6 × 500ms = 3s. */
export const MOVE_POLL_FAILURE_LIMIT = 6;

/** The dialog must always be escapable. Before this, a source that died
 *  mid-move left a modal with no close button, no Escape and no backdrop
 *  dismiss — `failed()` covered only ROLLED_BACK/FAILED, which a coordinator
 *  that stopped answering never reaches, and COMMITTED stops the poll before
 *  `pollFailures` can ever reach the limit. */
export function moveDialogCanClose(state: {
  started: boolean;
  phase: CoordinatorMovePhase | null;
  manualFallback: boolean;
  pollFailures: number;
}): boolean {
  if (!state.started) return true;
  if (state.phase === CoordinatorMovePhase.COMMITTED) return true;
  if (state.manualFallback) return true;
  if (state.phase !== null && isFailedMovePhase(state.phase)) return true;
  return state.pollFailures >= MOVE_POLL_FAILURE_LIMIT;
}

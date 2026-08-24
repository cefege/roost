// Edge enforcement of the coordinator write gate: once the move gate hits
// "retired", every mutating request gets a 410. The allowlisted GET paths
// (health, move status, auth identity) stay readable so workers and monitors
// can still DISCOVER the relocation — adding an RPC here decides whether it
// survives its own retirement.
import type { CoordinatorWriteMode } from "../coord-move/write-gate.ts";

function isRetiredDiscoveryPath(path: string): boolean {
  return path === "/roost.v1.CoordinatorService/AuthCoordIdentity"
    || path === "/roost.v1.CoordinatorService/AuthMintCoordinatorRelocation"
    || path === "/roost.v1.CoordinatorService/CoordinatorMoveStatus"
    || path === "/roost.v1.CoordinatorService/MiscHealth";
}

export function coordinatorAvailabilityResponse(
  mode: CoordinatorWriteMode,
  method: string,
  path: string,
): Response | null {
  if (mode === "retired" && method !== "GET" && !isRetiredDiscoveryPath(path)) {
    return new Response("coordinator relocated", { status: 410 });
  }
  return null;
}

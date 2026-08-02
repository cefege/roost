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

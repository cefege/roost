// Routes typed direct-terminal controls only after worker-frame-dispatch has
// proved readiness and current connection generation. Each sink receives the
// exact WorkerHandle object, so a late frame cannot settle a replacement peer.
// These controls never enter the generic JSON pending-RPC path.

import type { CoordWorkerUp } from "@roost/protocol/proto/worker_transport_pb";
import type { WorkerServiceDeps } from "./worker-conn-types.ts";
import type { WorkerHandle } from "./worker-registry.ts";

export function dispatchDirectTerminalWorkerResult(
  deps: WorkerServiceDeps,
  source: WorkerHandle | null | undefined,
  frame: CoordWorkerUp["frame"],
): boolean {
  if (!source) return false;
  switch (frame.case) {
    case "localTerminalPeerAnswer":
      return deps.terminalPeerNegotiations?.acceptAnswer(source, frame.value) ?? false;
    case "localTerminalPeerError":
      return deps.terminalPeerNegotiations?.acceptError(source, frame.value) ?? false;
    case "terminalInputRouteResult":
      return deps.terminalInputRouteResults?.acceptInputRouteResult(source, frame.value) ?? false;
    case "terminalTransportProbeResult":
      return deps.terminalInputRouteResults?.acceptTransportProbeResult(source, frame.value) ?? false;
    default:
      return false;
  }
}

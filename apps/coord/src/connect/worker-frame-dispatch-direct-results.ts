// Classifies typed direct-control worker results before the general frame
// dispatcher applies its readiness and generation fence. Terminal, attachment
// peer, and attachment receipt controls keep their own protocol handlers; this
// bridge only preserves one bounded dispatch seam for their current worker.

import type { CoordWorkerUp } from "@roost/protocol/proto/worker_transport_pb";
import { dispatchDirectAttachmentWorkerResult } from "./worker-frame-dispatch-direct-attachment.ts";
import { dispatchDirectTerminalWorkerResult } from "./worker-frame-dispatch-direct-terminal.ts";
import type { WorkerServiceDeps } from "./worker-conn-types.ts";
import type { WorkerHandle } from "./worker-registry.ts";

export type DirectWorkerResultKind =
  | "direct_terminal_result"
  | "direct_attachment_result"
  | "attachment_direct_status";

export function directWorkerResultKind(
  frame: CoordWorkerUp["frame"],
): DirectWorkerResultKind | null {
  switch (frame.case) {
    case "localTerminalPeerAnswer":
    case "localTerminalPeerError":
    case "terminalInputRouteResult":
    case "terminalTransportProbeResult":
      return "direct_terminal_result";
    case "localAttachmentPeerAnswer":
    case "localAttachmentPeerError":
      return "direct_attachment_result";
    case "attachmentDirectStatus":
      return "attachment_direct_status";
    default:
      return null;
  }
}

export function dispatchTypedDirectWorkerResult(
  deps: WorkerServiceDeps,
  source: WorkerHandle | null | undefined,
  frame: CoordWorkerUp["frame"],
  kind: DirectWorkerResultKind,
): boolean {
  if (kind === "direct_terminal_result") {
    return dispatchDirectTerminalWorkerResult(deps, source, frame);
  }
  return dispatchDirectAttachmentWorkerResult(deps, source, frame);
}

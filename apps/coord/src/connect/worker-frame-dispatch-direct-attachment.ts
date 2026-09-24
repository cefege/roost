// Routes typed direct-attachment controls after worker-frame admission proves
// the current ready socket. The exact WorkerHandle fences answer correlation,
// so a stale connection cannot settle an attachment peer for its replacement.
// Attachment controls never pass through generic JSON pending RPCs.

import type { CoordWorkerUp } from "@roost/protocol/proto/worker_transport_pb";
import type { WorkerServiceDeps } from "./worker-conn-types.ts";
import type { WorkerHandle } from "./worker-registry.ts";

export function dispatchDirectAttachmentWorkerResult(
  deps: WorkerServiceDeps,
  source: WorkerHandle | null | undefined,
  frame: CoordWorkerUp["frame"],
): boolean {
  if (!source) return false;
  switch (frame.case) {
    case "localAttachmentPeerAnswer":
      return deps.attachmentPeerNegotiations?.acceptAnswer(source, frame.value) ?? false;
    case "localAttachmentPeerError":
      return deps.attachmentPeerNegotiations?.acceptError(source, frame.value) ?? false;
    case "attachmentDirectStatus":
      return deps.attachmentDirectStatusResults?.acceptStatus(source, frame.value) ?? false;
    default:
      return false;
  }
}

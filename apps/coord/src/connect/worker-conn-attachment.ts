// Keeps attachment-specific worker-connection controls separate from terminal
// negotiation lifecycle. The raw connection owner calls these helpers at the
// same exact-handle fences used for every worker replacement and revocation.
// Attachment grant authority remains in AttachmentGrantOwner, not this module.

import { ATTACHMENT_TRANSFER_PEER_WEBRTC_CAPABILITY } from "@roost/shared/attachment-transfer";
import type { WorkerServiceDeps } from "./worker-conn-types.ts";
import type { WorkerHandle } from "./worker-registry.ts";

export function acknowledgeAttachmentPeerCapability(
  deps: Pick<WorkerServiceDeps, "cfg" | "attachmentPeerNegotiations">,
  advertised: readonly string[],
  acknowledged: string[],
): boolean {
  const negotiated = deps.cfg?.terminalPeerEnabled === true
    && deps.attachmentPeerNegotiations !== undefined
    && advertised.includes(ATTACHMENT_TRANSFER_PEER_WEBRTC_CAPABILITY);
  if (negotiated) acknowledged.push(ATTACHMENT_TRANSFER_PEER_WEBRTC_CAPABILITY);
  return negotiated;
}

export function cancelAttachmentDirectWorkerResults(
  deps: Pick<WorkerServiceDeps, "attachmentPeerNegotiations" | "attachmentDirectStatusResults">,
  worker: WorkerHandle,
  reason: string,
): void {
  deps.attachmentPeerNegotiations?.cancelForWorkerHandle(worker, reason);
  deps.attachmentDirectStatusResults?.cancelForWorkerHandle(worker, reason);
}

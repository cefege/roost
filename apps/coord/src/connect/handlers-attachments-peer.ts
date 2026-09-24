// Bridges the authenticated attachment-peer RPC into its bounded negotiation owner.
// Grant, worker, request-shape, and response-correlation authority stay in that
// owner; this boundary supplies only the authenticated device and exact tab.
// The handler is spread through the coordinator's one service implementation.

import type { ServiceImpl } from "@connectrpc/connect";
import { CoordinatorService } from "@roost/protocol/proto/coordinator_pb";
import { requireAccountDevice, tabIdKey } from "./auth-interceptor.ts";
import type { ConnectDeps } from "./router.ts";

type AttachmentPeerMethods = "sessionsNegotiateAttachmentPeer";

export function makeAttachmentPeerHandlers(
  deps: ConnectDeps,
): Pick<ServiceImpl<typeof CoordinatorService>, AttachmentPeerMethods> {
  return {
    sessionsNegotiateAttachmentPeer(request, context) {
      return deps.attachmentPeerNegotiations.negotiate(
        requireAccountDevice(context.values),
        context.values.get(tabIdKey),
        request,
        context.signal,
      );
    },
  };
}

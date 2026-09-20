// Bridges the authenticated Connect RPC into TerminalPeerNegotiations. The
// owner performs every grant, route, SDP, capacity, and worker-generation
// admission check; this handler supplies only trusted caller and tab context.
// It is spread through the existing sessions service literal.

import type { ServiceImpl } from "@connectrpc/connect";
import { CoordinatorService } from "@roost/shared/proto/coordinator_pb";
import { requireAccountDevice, tabIdKey } from "./auth-interceptor.ts";
import type { ConnectDeps } from "./router.ts";

type TerminalPeerSessionMethods = "sessionsNegotiateLocalTerminalPeer";

export function makeSessionTerminalPeerHandlers(
  deps: ConnectDeps,
): Pick<ServiceImpl<typeof CoordinatorService>, TerminalPeerSessionMethods> {
  return {
    sessionsNegotiateLocalTerminalPeer(request, context) {
      return deps.terminalPeerNegotiations.negotiate(
        requireAccountDevice(context.values),
        context.values.get(tabIdKey),
        request,
        context.signal,
      );
    },
  };
}

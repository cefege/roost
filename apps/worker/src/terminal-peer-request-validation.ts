// Bounded identity validation for coordinator-authenticated terminal peer offers.
// TerminalPeerOwner calls this before native allocation; it owns no peer state
// and rejects opaque control identifiers rather than logging their contents.
// The coordinator already authorizes the worker and grant tuple separately.

import type { DLocalTerminalPeerOffer } from "@roost/shared/proto/worker_transport_pb";

const UUID_RE = /^[0-9a-f]{8}-[0-9a-f]{4}-[1-5][0-9a-f]{3}-[89ab][0-9a-f]{3}-[0-9a-f]{12}$/iu;
const MAX_OPAQUE_ID_BYTES = 128;

export function validTerminalPeerOfferIdentity(request: DLocalTerminalPeerOffer): boolean {
  return UUID_RE.test(request.peerId)
    && validOpaqueId(request.requestId)
    && validOpaqueId(request.connectionGeneration)
    && validOpaqueId(request.grantId)
    && validOpaqueId(request.deviceFingerprint)
    && validOpaqueId(request.tabId);
}

function validOpaqueId(value: string): boolean {
  if (value.length === 0 || value.length > MAX_OPAQUE_ID_BYTES) return false;
  for (let index = 0; index < value.length; index += 1) {
    const code = value.charCodeAt(index);
    if (code <= 0x1f || code === 0x7f) return false;
  }
  return true;
}

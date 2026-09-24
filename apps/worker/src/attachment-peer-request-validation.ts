// Bounded identity validation before attachment peer native allocation.
// The coordinator has already authenticated the request; this rejects malformed
// opaque fields without logging SDP, grant material, or attachment metadata.

import type { DLocalAttachmentPeerOffer } from "@roost/protocol/proto/worker_transport_pb";

const UUID_RE = /^[0-9a-f]{8}-[0-9a-f]{4}-[1-5][0-9a-f]{3}-[89ab][0-9a-f]{3}-[0-9a-f]{12}$/iu;
const MAX_OPAQUE_ID_BYTES = 128;

export function validAttachmentPeerOfferIdentity(request: DLocalAttachmentPeerOffer): boolean {
  return UUID_RE.test(request.peerId)
    && validOpaqueId(request.requestId)
    && validOpaqueId(request.connectionGeneration)
    && validOpaqueId(request.grantId)
    && validOpaqueId(request.deviceFingerprint)
    && validOpaqueId(request.tabId);
}

function validOpaqueId(value: string): boolean {
  if (value.length === 0 || Buffer.byteLength(value, "utf8") > MAX_OPAQUE_ID_BYTES) return false;
  for (let index = 0; index < value.length; index += 1) {
    const code = value.charCodeAt(index);
    if (code <= 0x1f || code === 0x7f) return false;
  }
  return true;
}

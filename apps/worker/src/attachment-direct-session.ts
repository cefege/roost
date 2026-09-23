// Per-port state for one admitted direct attachment upload.
// AttachmentDirectSockets owns mutation; carrier and grant modules supply only identities.
// The finite active lease remains separate from the short-lived hello grant.

import type { AttachmentUploadMetadata } from "./attachment-transfer-admission.ts";
import type { AttachmentPeerExpectedTuple } from "./attachment-peer-connection.ts";
import type { AttachmentTransferPort } from "./attachment-transfer-port.ts";
import type { AttachmentTransferLease } from "./attachment-transfer-lease.ts";

export interface AttachmentPortSession {
  readonly port: AttachmentTransferPort;
  readonly expectedPeer: AttachmentPeerExpectedTuple | null;
  metadata: AttachmentUploadMetadata | null;
  terminal: boolean;
  lease: AttachmentTransferLease | null;
  setupTimer: NodeJS.Timeout | undefined;
  writePending: boolean;
}

/** Coordinator-negotiated peers and hello-admitted sockets hold admitted-upload slots. */
export function holdsAdmittedSlot(session: AttachmentPortSession): boolean {
  return session.metadata !== null || session.expectedPeer !== null;
}

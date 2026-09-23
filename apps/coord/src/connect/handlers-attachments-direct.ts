// Owns authenticated attachment direct-control RPCs: one immutable grant mint
// and durable receipt status lookup. It authorizes the live session route, then
// delegates separate grant and typed worker-correlation state to composition
// owners. This handler never forwards attachment bytes through the coordinator.

import { Code, ConnectError, type ServiceImpl } from "@connectrpc/connect";
import { create } from "@bufbuild/protobuf";
import {
  AttachmentsDirectStatusResponseSchema,
  AttachmentsGrantDirectResponseSchema,
  CoordinatorService,
  type AttachmentsGrantDirectRequest,
} from "@roost/shared/proto/coordinator_pb";
import {
  ATTACHMENT_TRANSFER_GRANT_TTL_MS,
  ATTACHMENT_TRANSFER_PEER_WEBRTC_CAPABILITY,
} from "@roost/shared/attachment-transfer";
import { hasAtMostUtf8Bytes } from "@roost/shared/ui-state";
import type { KyselyDB } from "../db/connection.ts";
import { requireAccountDevice, tabIdKey } from "./auth-interceptor.ts";
import {
  attachmentGrantDenied,
  attachmentGrantInvalid,
  attachmentGrantUnavailable,
  type AttachmentGrantDescriptor,
} from "./attachment-grant-owner-state.ts";
import { captureOwnerKey } from "./terminal-capture-lease.ts";
import type { ConnectDeps } from "./router.ts";
import { currentRoutableWorker } from "./worker-send-target.ts";

const DIRECT_STATUS_IDENTIFIER_MAX_UTF8_BYTES = 128;
type AttachmentDirectMethods = "attachmentsGrantDirect" | "attachmentsDirectStatus";

export function makeAttachmentDirectHandlers(
  deps: ConnectDeps,
): Pick<ServiceImpl<typeof CoordinatorService>, AttachmentDirectMethods> {
  return {
    async attachmentsGrantDirect(request, context) {
      const caller = requireAccountDevice(context.values);
      const tabId = requireAttachmentGrantTab(request.tabId, context.values.get(tabIdKey));
      const descriptor = attachmentGrantDescriptor(request);
      try {
        const granted = await deps.attachmentGrants.grant({
          ownerKey: captureOwnerKey(caller),
          deviceFingerprint: caller.fingerprint,
          tabId,
          workerFp: request.workerFp,
          descriptor,
          authorize: () => authorizeAttachmentSession(deps.db, request.sessionId, request.workerFp),
        });
        const peerSupported = deps.cfg.terminalPeerEnabled
          && granted.lease.workerHandle.capabilities.has(ATTACHMENT_TRANSFER_PEER_WEBRTC_CAPABILITY);
        return create(AttachmentsGrantDirectResponseSchema, {
          grantId: granted.lease.grantId,
          secret: granted.secret,
          ttlMs: ATTACHMENT_TRANSFER_GRANT_TTL_MS,
          workerEpoch: granted.lease.workerEpoch,
          peerSupported,
          stunUrls: peerSupported ? deps.cfg.terminalPeerStunUrls : [],
        });
      } catch (error) {
        if (error instanceof ConnectError) throw error;
        throw attachmentGrantUnavailable("attachment grant worker is unavailable");
      }
    },

    async attachmentsDirectStatus(request, context) {
      requireAccountDevice(context.values);
      assertAttachmentStatusRequest(request.sessionId, request.uploadId);
      const workerFp = await attachmentSessionWorker(deps.db, request.sessionId);
      const worker = currentRoutableWorker(workerFp);
      if (!worker) throw attachmentGrantUnavailable("attachment status worker is unavailable");
      const status = await deps.attachmentDirectStatusResults.request(worker, request.sessionId, request.uploadId);
      return create(AttachmentsDirectStatusResponseSchema, { status });
    },
  };
}

function requireAttachmentGrantTab(requestTabId: string, authenticatedTabId: string | undefined): string {
  if (!authenticatedTabId) throw attachmentGrantInvalid("attachment grant tab is invalid");
  if (requestTabId !== authenticatedTabId) {
    throw attachmentGrantDenied("attachment grant tab does not match the authenticated document");
  }
  return requestTabId;
}

function attachmentGrantDescriptor(request: AttachmentsGrantDirectRequest): AttachmentGrantDescriptor {
  if (request.totalBytes > BigInt(Number.MAX_SAFE_INTEGER)) {
    throw attachmentGrantInvalid("attachment grant descriptor is invalid");
  }
  return {
    sessionId: request.sessionId,
    uploadId: request.uploadId,
    filename: request.filename,
    shortPath: request.shortPath,
    totalBytes: Number(request.totalBytes),
  };
}

async function authorizeAttachmentSession(
  db: KyselyDB,
  sessionId: string,
  expectedWorkerFp: string,
): Promise<void> {
  const workerFp = await attachmentSessionWorker(db, sessionId);
  if (workerFp !== expectedWorkerFp) {
    throw attachmentGrantDenied("attachment grant worker does not own the session");
  }
}

async function attachmentSessionWorker(db: KyselyDB, sessionId: string): Promise<string> {
  const session = await db.selectFrom("sessions as session")
    .innerJoin("workers as worker", "worker.fp", "session.worker_fp")
    .select(["session.worker_fp as workerFp", "session.status as status"])
    .where("session.id", "=", sessionId)
    .where("worker.deleted_at_ms", "is", null)
    .executeTakeFirst();
  if (!session || session.status !== "open") {
    throw new ConnectError("attachment session is unavailable", Code.NotFound);
  }
  return session.workerFp;
}

function assertAttachmentStatusRequest(sessionId: string, uploadId: string): void {
  if (
    sessionId.length === 0
    || !hasAtMostUtf8Bytes(sessionId, DIRECT_STATUS_IDENTIFIER_MAX_UTF8_BYTES)
    || uploadId.length === 0
    || !hasAtMostUtf8Bytes(uploadId, DIRECT_STATUS_IDENTIFIER_MAX_UTF8_BYTES)
    || /[\\/\x00-\x1f\x7f]/u.test(uploadId)
  ) throw attachmentGrantInvalid("attachment status request is invalid");
}

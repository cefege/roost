// Session grant RPC boundary for direct terminal transports. It validates the
// request against the authenticated Connect tab, then delegates lease lifetime
// and worker-ACK fencing to TerminalGrantOwner. The session-route authorizer is
// exported for peer signaling so grants and negotiation cannot drift apart.

import { Code, ConnectError, type ServiceImpl } from "@connectrpc/connect";
import { create } from "@bufbuild/protobuf";
import {
  CoordinatorService,
  SessionsGrantLocalTerminalResponseSchema,
} from "@roost/shared/proto/coordinator_pb";
import {
  TERMINAL_INPUT_ROUTE_CAPABILITY,
  TERMINAL_PEER_MAX_SESSIONS_PER_GRANT,
  TERMINAL_PEER_WEBRTC_CAPABILITY,
} from "@roost/shared/terminal-peer";
import { hasAtMostUtf8Bytes } from "@roost/shared/ui-state";
import type { KyselyDB } from "../db/connection.ts";
import { requireAccountDevice, tabIdKey } from "./auth-interceptor.ts";
import { captureFailure, captureOwnerKey } from "./terminal-capture-lease.ts";
import type { ConnectDeps } from "./router.ts";
import { LOCAL_TERMINAL_GRANT_TTL_MS } from "./terminal-grant-owner.ts";

const TERMINAL_GRANT_STRING_MAX_UTF8_BYTES = 128;

export type LocalTerminalGrantHandlers = Pick<
  ServiceImpl<typeof CoordinatorService>,
  "sessionsGrantLocalTerminal"
>;

export function makeSessionLocalTerminalGrantHandlers(
  deps: ConnectDeps,
): LocalTerminalGrantHandlers {
  return {
    async sessionsGrantLocalTerminal(request, context) {
      const caller = requireAccountDevice(context.values);
      const authenticatedTabId = context.values.get(tabIdKey);
      const { workerFp, tabId, sessionIds } = checkedGrantRequest(request, authenticatedTabId);

      let granted;
      try {
        granted = await deps.terminalGrants.grant({
          ownerKey: captureOwnerKey(caller),
          deviceFingerprint: caller.fingerprint,
          tabId,
          workerFp,
          sessionIds,
          authorize: (authorizedSessionIds) =>
            authorizeTerminalGrantSessions(deps.db, workerFp, authorizedSessionIds),
        });
      } catch (error) {
        if (isGrantClientFailure(error)) throw error;
        throw grantInstallFailure(error);
      }

      const { lease } = granted;
      const workerEpoch = lease.workerEpoch ?? "";
      const epochAware = workerEpoch !== "";
      const peerSupported = epochAware
        && deps.cfg.terminalPeerEnabled
        && lease.workerHandle.capabilities.has(TERMINAL_PEER_WEBRTC_CAPABILITY);
      const inputRouteSupported = epochAware
        && lease.workerHandle.capabilities.has(TERMINAL_INPUT_ROUTE_CAPABILITY);
      return create(SessionsGrantLocalTerminalResponseSchema, {
        grantId: lease.grantId,
        secret: granted.secret,
        ttlMs: LOCAL_TERMINAL_GRANT_TTL_MS,
        workerEpoch,
        peerSupported,
        stunUrls: peerSupported ? deps.cfg.terminalPeerStunUrls : [],
        inputRouteSupported,
      });
    },
  };
}

/** Confirms every requested session remains an open route on the selected live worker row. */
export async function authorizeTerminalGrantSessions(
  db: KyselyDB,
  workerFp: string,
  sessionIds: readonly string[],
): Promise<void> {
  requireBoundedString(workerFp, "worker_fp");
  if (sessionIds.length === 0 || sessionIds.length > TERMINAL_PEER_MAX_SESSIONS_PER_GRANT) {
    throw captureFailure("invalid_argument", "session_ids");
  }
  const uniqueSessionIds = new Set<string>();
  for (const sessionId of sessionIds) {
    requireBoundedString(sessionId, "session_ids");
    if (uniqueSessionIds.has(sessionId)) throw captureFailure("invalid_argument", "session_ids");
    uniqueSessionIds.add(sessionId);
  }
  const rows = await db.selectFrom("sessions as session")
    .innerJoin("workers as worker", "worker.fp", "session.worker_fp")
    .select([
      "session.id as id",
      "session.worker_fp as worker_fp",
      "session.status as status",
    ])
    .where("session.id", "in", [...sessionIds])
    .where("worker.deleted_at_ms", "is", null)
    .execute();
  const routes = new Map(rows.map((row) => [row.id, row]));
  for (const sessionId of sessionIds) {
    const route = routes.get(sessionId);
    if (!route || route.status !== "open") {
      throw captureFailure("session_unknown", "session_ids");
    }
    if (route.worker_fp !== workerFp) {
      throw captureFailure("permission_denied", "worker_fp");
    }
  }
}

function checkedGrantRequest(
  request: { workerFp: string; tabId: string; sessionIds: readonly string[] },
  authenticatedTabId: string | undefined,
): { workerFp: string; tabId: string; sessionIds: string[] } {
  requireBoundedString(request.workerFp, "worker_fp");
  requireBoundedString(request.tabId, "tab_id");
  if (authenticatedTabId === undefined) throw captureFailure("invalid_argument", "tab_id");
  requireBoundedString(authenticatedTabId, "tab_id");
  if (request.tabId !== authenticatedTabId) throw captureFailure("permission_denied", "tab_id");
  if (
    request.sessionIds.length === 0
    || request.sessionIds.length > TERMINAL_PEER_MAX_SESSIONS_PER_GRANT
  ) {
    throw captureFailure("invalid_argument", "session_ids");
  }
  const sessionIds: string[] = [];
  const seen = new Set<string>();
  for (const sessionId of request.sessionIds) {
    requireBoundedString(sessionId, "session_ids");
    if (seen.has(sessionId)) throw captureFailure("invalid_argument", "session_ids");
    seen.add(sessionId);
    sessionIds.push(sessionId);
  }
  return { workerFp: request.workerFp, tabId: request.tabId, sessionIds };
}

function requireBoundedString(value: string, field: string): void {
  if (value.length === 0 || !hasAtMostUtf8Bytes(value, TERMINAL_GRANT_STRING_MAX_UTF8_BYTES)) {
    throw captureFailure("invalid_argument", field);
  }
}
function isGrantClientFailure(error: unknown): error is ConnectError {
  return error instanceof ConnectError && (
    error.code === Code.InvalidArgument
    || error.code === Code.NotFound
    || error.code === Code.PermissionDenied
    || error.code === Code.ResourceExhausted
  );
}

function grantInstallFailure(error: unknown): ConnectError {
  if (error instanceof ConnectError) {
    if (error.code === Code.Unavailable) return captureFailure("worker_offline", "worker_fp");
    if (error.code === Code.DeadlineExceeded) return captureFailure("worker_timeout", "worker_fp");
  }
  return captureFailure("worker_failed", "worker_fp");
}

// Coordinator-side owner of per-session local-terminal grants: mints one grant,
// installs it on the session's worker, and keeps the principal+tab lease
// registry that a renewal replaces and a device revocation drops.
// Authorization mirrors the sibling session RPCs, and the minted secret is
// returned only after the worker acknowledges. Only its digest crosses a wire:
// the secret enters no log line, no audit row, and no diagnostic.

import { createHash, randomBytes, randomUUID } from "node:crypto";
import { Code, ConnectError } from "@connectrpc/connect";
import type { ServiceImpl } from "@connectrpc/connect";
import { create } from "@bufbuild/protobuf";
import {
  CoordinatorService,
  SessionsGrantLocalTerminalResponseSchema,
} from "@roost/shared/proto/coordinator_pb";
import { log } from "@roost/shared/log";
import { requireAccountDevice } from "./auth-interceptor.ts";
import { captureFailure, captureOwnerKey } from "./terminal-capture-lease.ts";
import { listRoutableFps } from "./worker-registry.ts";
import {
  sendLocalTerminalGrantRequest,
  sendLocalTerminalGrantRevoke,
} from "./worker-send-local-terminal.ts";
import type { ConnectDeps } from "./router.ts";

/** Long enough that an already-open local pane outlives a coordinator outage,
 *  short enough to bound a device whose revocation could not be delivered while
 *  the coordinator was unreachable. */
export const LOCAL_TERMINAL_GRANT_TTL_MS = 12 * 60 * 60_000;

interface LocalTerminalGrantLease {
  readonly grantId: string;
  /** Derived from the authenticated principal, never from a request body. */
  readonly ownerKey: string;
  readonly deviceFingerprint: string;
  readonly tabId: string;
  readonly workerFp: string;
  readonly sessionIds: readonly string[];
  readonly expiresAtMs: number;
  timer: NodeJS.Timeout | undefined;
}

/** What a lease exposes to tests and diagnostics. Nothing is redacted because
 *  nothing secret is retained: the coordinator keeps neither the secret nor its
 *  digest once the worker has acknowledged the install. */
export interface LocalTerminalGrantRecord {
  readonly grantId: string;
  readonly deviceFingerprint: string;
  readonly tabId: string;
  readonly workerFp: string;
  readonly sessionIds: readonly string[];
  readonly expiresAtMs: number;
}

// One lease per authenticated principal and browser tab: that pair IS the local
// page's identity, so a renewal replaces its predecessor instead of leaving a
// second live grant behind for the same pane.
const leases = new Map<string, LocalTerminalGrantLease>();

export type LocalTerminalGrantHandlers = Pick<
  ServiceImpl<typeof CoordinatorService>,
  "sessionsGrantLocalTerminal"
>;

export function makeSessionLocalTerminalGrantHandlers(
  deps: ConnectDeps,
): LocalTerminalGrantHandlers {
  return {
    async sessionsGrantLocalTerminal(req, ctx) {
      const caller = requireAccountDevice(ctx.values);
      if (req.workerFp === "") throw captureFailure("invalid_argument", "worker_fp");
      if (req.tabId === "") throw captureFailure("invalid_argument", "tab_id");
      // A repeated id would authorize the same session twice in the frame the
      // worker turns into its allow-set.
      const sessionIds = [...new Set(req.sessionIds)];
      if (sessionIds.length === 0) throw captureFailure("invalid_argument", "session_ids");
      await authorizeGrantedSessions(deps, req.workerFp, sessionIds);

      const grantId = randomUUID();
      const secret = randomBytes(32).toString("hex");
      try {
        const install = sendLocalTerminalGrantRequest(req.workerFp, {
          grantId,
          secretSha256: createHash("sha256").update(secret).digest("hex"),
          sessionIds,
          deviceFingerprint: caller.fingerprint,
          tabId: req.tabId,
          ttlMs: LOCAL_TERMINAL_GRANT_TTL_MS,
        });
        await install.promise;
      } catch (error) {
        log.warn("local-terminal-grants", "install_failed", {
          grant_id: grantId,
          worker_fp: req.workerFp,
          device_fp: caller.fingerprint,
          error: error instanceof ConnectError ? error.rawMessage : String(error),
        });
        throw grantInstallFailure(error);
      }

      // Recorded only now: a lease the worker never acknowledged would claim a
      // fast path that does not exist, and a failed renewal must leave the
      // predecessor the worker still holds in place.
      installLease({
        grantId,
        ownerKey: captureOwnerKey(caller),
        deviceFingerprint: caller.fingerprint,
        tabId: req.tabId,
        workerFp: req.workerFp,
        sessionIds,
      }, Date.now());
      log.info("local-terminal-grants", "granted", {
        grant_id: grantId,
        worker_fp: req.workerFp,
        device_fp: caller.fingerprint,
        tab_id: req.tabId,
        sessions: sessionIds.length,
        ttl_ms: LOCAL_TERMINAL_GRANT_TTL_MS,
      });
      return create(SessionsGrantLocalTerminalResponseSchema, {
        grantId,
        secret,
        ttlMs: LOCAL_TERMINAL_GRANT_TTL_MS,
      });
    },
  };
}

/** A revoked device loses every local fast path at once. The frame goes to
 *  every routable worker rather than only the ones this process holds leases
 *  for: the registry is memory-only, so after a coordinator restart the lease
 *  rows are gone while the workers' grants are not. Returns leases dropped. */
export function revokeLocalTerminalGrantsForFingerprint(deviceFingerprint: string): number {
  let dropped = 0;
  for (const [key, lease] of leases) {
    if (lease.deviceFingerprint !== deviceFingerprint) continue;
    clearTimeout(lease.timer);
    leases.delete(key);
    dropped += 1;
  }
  let notified = 0;
  for (const workerFp of listRoutableFps()) {
    if (sendLocalTerminalGrantRevoke(workerFp, deviceFingerprint)) notified += 1;
  }
  log.info("local-terminal-grants", "revoked", {
    device_fp: deviceFingerprint,
    leases_dropped: dropped,
    workers_notified: notified,
  });
  return dropped;
}

/** Server-time expiry sweep of coordinator bookkeeping only — the worker
 *  enforces the same TTL from the grant frame's ttl_ms. Carries the
 *  test/diagnostic marker because the focused tests drive it at an arbitrary
 *  instant instead of waiting out a twelve-hour grant. */
export function _sweepLocalTerminalGrants(nowMs: number): void {
  for (const [key, lease] of leases) {
    if (lease.expiresAtMs > nowMs) continue;
    clearTimeout(lease.timer);
    leases.delete(key);
    log.info("local-terminal-grants", "expired", {
      grant_id: lease.grantId,
      worker_fp: lease.workerFp,
      device_fp: lease.deviceFingerprint,
    });
  }
}

export function _localTerminalGrantLeases(): LocalTerminalGrantRecord[] {
  return [...leases.values()].map((lease) => ({
    grantId: lease.grantId,
    deviceFingerprint: lease.deviceFingerprint,
    tabId: lease.tabId,
    workerFp: lease.workerFp,
    sessionIds: lease.sessionIds,
    expiresAtMs: lease.expiresAtMs,
  }));
}

export function _resetLocalTerminalGrants(): void {
  for (const lease of leases.values()) clearTimeout(lease.timer);
  leases.clear();
}

/** Every granted session must be an OPEN session of the named worker, resolved
 *  exactly as the sibling session RPCs resolve a route (live worker row only):
 *  the fast path must never widen what this device can already reach. */
async function authorizeGrantedSessions(
  deps: ConnectDeps,
  workerFp: string,
  sessionIds: readonly string[],
): Promise<void> {
  const rows = await deps.db.selectFrom("sessions as session")
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

function installLease(
  lease: Omit<LocalTerminalGrantLease, "expiresAtMs" | "timer">,
  nowMs: number,
): void {
  const key = JSON.stringify([lease.ownerKey, lease.tabId]);
  clearTimeout(leases.get(key)?.timer);
  const timer = setTimeout(
    () => _sweepLocalTerminalGrants(Date.now()),
    LOCAL_TERMINAL_GRANT_TTL_MS + 1_000,
  );
  timer.unref?.();
  leases.set(key, {
    ...lease,
    expiresAtMs: nowMs + LOCAL_TERMINAL_GRANT_TTL_MS,
    timer,
  });
}

/** The worker's failure decides the vocabulary: a transport gap is retryable,
 *  a silent worker is a deadline, anything else is the worker's own refusal. */
function grantInstallFailure(error: unknown): ConnectError {
  if (error instanceof ConnectError) {
    if (error.code === Code.Unavailable) return captureFailure("worker_offline", "worker_fp");
    if (error.code === Code.DeadlineExceeded) return captureFailure("worker_timeout", "worker_fp");
  }
  return captureFailure("worker_failed", "worker_fp");
}

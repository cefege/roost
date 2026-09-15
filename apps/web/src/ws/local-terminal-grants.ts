// Coordinator-authorized grants for the local terminal fast path, and the
// cadence that keeps one alive. The coordinator checks that every requested
// session really lives on this worker, installs the grant's digest on it, and
// only then hands this page the secret the local socket presents. This module
// owns the grant, the set of sessions panes asked for, and renewal; the socket
// owner (local-terminal.ts) passes itself in and is told when to present a new
// one. Grants live in memory for exactly this document — never storage, never a
// diagnostic field.

import { diag } from "@roost/shared/diag";
import { getCurrentWebKeyInfo } from "../auth/web-key.ts";
import { getTabId } from "../auth/tab-id.ts";
import { coordClient } from "../connect.ts";
import { readLocalBootstrap } from "../lib/localBootstrap.ts";
import { rootStore } from "../store/root.ts";

export const LOCAL_TERMINAL_GRANT_RENEW_MS = 60 * 60_000;
const GRANT_RETRY_MS = 30_000;

export interface LocalTerminalGrant {
  grantId: string;
  secret: string;
  sessionIds: string[];
  tabId: string;
  deviceFingerprint: string;
}

/** What grant upkeep needs from the socket that presents them. */
export interface LocalTerminalGrantSocket {
  /** True while a socket presenting the held grant is dialed. */
  connected(): boolean;
  /** Present this grant on a new socket, replacing any live one. */
  present(grant: LocalTerminalGrant): void;
}

let grant: LocalTerminalGrant | null = null;
const wantedSessions = new Set<string>();
let grantRetryAtMs = 0;
let grantInFlight = false;
let renewTimer: ReturnType<typeof setInterval> | null = null;

export function currentLocalTerminalGrant(): LocalTerminalGrant | null {
  return grant;
}

/** A pane published a view for this session. Only the coordinator can decide
 * whether it is grantable, so this records the want and asks. */
export function noteLocalTerminalViewPublished(
  socket: LocalTerminalGrantSocket,
  sessionId: string,
): void {
  if (!readLocalBootstrap() || wantedSessions.has(sessionId)) return;
  wantedSessions.add(sessionId);
  void refreshLocalTerminalGrant(socket, "view_published");
}

/** One grant covers a set of sessions, so a newly wanted session needs a new
 * grant and a socket that presents it. A renewal keeps the same set and the
 * same socket: the worker simply holds a fresher credential for the next dial. */
export async function refreshLocalTerminalGrant(
  socket: LocalTerminalGrantSocket,
  reason: string,
): Promise<void> {
  const bootstrap = readLocalBootstrap();
  if (!bootstrap || grantInFlight) return;
  const workerFp = bootstrap.workerFingerprint;
  const eligible = grantableSessions(workerFp);
  if (eligible.length === 0) return;
  const held = grant;
  const covered = held !== null
    && eligible.every((sessionId) => held.sessionIds.includes(sessionId));
  if (covered && reason === "view_published") return;
  if (!covered && Date.now() < grantRetryAtMs) return;
  grantInFlight = true;
  try {
    const minted = await mintGrant(workerFp, eligible);
    if (!minted) {
      grantRetryAtMs = Date.now() + GRANT_RETRY_MS;
      return;
    }
    grant = minted;
    if (!covered || !socket.connected()) socket.present(minted);
  } finally {
    grantInFlight = false;
  }
}

/** Renewal runs only while a socket is up; the coordinator may be unreachable
 * for long stretches and the held grant stays valid until its TTL. */
export function armLocalTerminalGrantRenewal(socket: LocalTerminalGrantSocket): void {
  if (renewTimer !== null) return;
  renewTimer = setInterval(
    () => void refreshLocalTerminalGrant(socket, "renewal"),
    LOCAL_TERMINAL_GRANT_RENEW_MS,
  );
}

/** The worker refused this grant; only the coordinator can replace it. */
export function dropLocalTerminalGrant(): void {
  grant = null;
}

export function clearLocalTerminalGrantRetry(): void {
  grantRetryAtMs = 0;
}

export function resetLocalTerminalGrants(): void {
  grant = null;
  wantedSessions.clear();
  grantRetryAtMs = 0;
}

export function _releaseLocalTerminalGrantRenewalForTest(): void {
  if (renewTimer === null) return;
  clearInterval(renewTimer);
  renewTimer = null;
}

/** The subset of wanted sessions the coordinator can plausibly grant: open
 * sessions this page knows are routed to the local worker. Asking for anything
 * else is a guaranteed PermissionDenied that would strand the whole grant. */
function grantableSessions(workerFp: string): string[] {
  const granted: string[] = [];
  for (const sessionId of wantedSessions) {
    const session = rootStore.sessions[sessionId];
    if (!session || session.status !== "open" || session.worker_fp !== workerFp) continue;
    granted.push(sessionId);
  }
  return granted;
}

async function mintGrant(
  workerFp: string,
  sessionIds: string[],
): Promise<LocalTerminalGrant | null> {
  const tabId = getTabId();
  try {
    const deviceFingerprint = (await getCurrentWebKeyInfo()).fingerprint;
    const response = await coordClient.sessionsGrantLocalTerminal({
      sessionIds,
      workerFp,
      tabId,
    });
    if (!response.grantId || !response.secret) return null;
    diag("local_terminal.grant_minted", {
      worker_fp: workerFp,
      grant_id: response.grantId,
      sessions: sessionIds.length,
      ttl_ms: response.ttlMs,
    });
    return {
      grantId: response.grantId,
      secret: response.secret,
      sessionIds: [...sessionIds],
      tabId,
      deviceFingerprint,
    };
  } catch (error) {
    // A refused or unreachable coordinator is not an error here: those sessions
    // simply stay on Sync.
    diag("local_terminal.grant_refused", {
      worker_fp: workerFp,
      sessions: sessionIds.length,
      error: String(error),
    });
    return null;
  }
}

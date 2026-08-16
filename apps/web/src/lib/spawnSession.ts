// Spawn a new keeper PTY session on a worker.
//
// phase-24c-1: dispatches via tRPC `sessions.spawn` mutation. Coord
// forwards as `browser-command` to the worker over the WorkerHub WSS;
// worker SessionManager creates the keeper + emits the `opened`
// SessionEvent; coord projects + fans out via `sessions.events` SSE,
// so the new row lands in the SPA store within ~50ms. The `attached`
// reply carries (session_id, channel_id), letting the caller navigate
// directly to /w/<id>/t/<channel>.
//
// Replaces the previous worker-direct WSS round-trip — no browser
// dials worker for spawn anymore. Reachability problems with a
// specific worker can no longer wedge the click flow.

import { ConnectError, Code } from "@connectrpc/connect";
import { coordClient } from "../connect.ts";
import { rootStore } from "../store/root.ts";
import { estimateWtermSize } from "./wtermSizeEstimate.ts";
import { log } from "@roost/shared/log";
import type { WorkerFp, Session } from "@roost/shared/wire";
import { sendTerminalInput } from "../ws/sync-outbound.ts";
import { resolveAgent, autoLaunchEnabled } from "./agents.ts";

export interface SpawnInitialViewport {
  cols: number;
  rows: number;
  clientSeq: number;
}

export interface SpawnShellResult {
  sessionId: string;
  channelId: number;
  initialViewportPreclaimed: boolean;
  effectiveClientSeq: number;
}

export interface SpawnShellRequest {
  workerFp: WorkerFp;
  kind: "shell";
  folder: string;
  cols?: number;
  rows?: number;
  sessionId?: string;
  preclaimInitialViewport?: boolean;
  initialViewportClientSeq?: bigint;
}

/** Pure request builder shared with tests. A measured mount is the only path
 * that asks the server to preclaim; estimates merely choose the PTY start size. */
export function buildSpawnShellRequest(
  workerFp: WorkerFp,
  folder: string,
  sessionId?: string,
  initialViewport?: SpawnInitialViewport,
): SpawnShellRequest {
  const size = initialViewport ?? estimateWtermSize() ?? undefined;
  return {
    workerFp,
    kind: "shell",
    folder,
    cols: size?.cols,
    rows: size?.rows,
    ...(sessionId ? { sessionId } : {}),
    ...(initialViewport
      ? {
        preclaimInitialViewport: true,
        initialViewportClientSeq: BigInt(initialViewport.clientSeq),
      }
      : {}),
  };
}

// A session's agent command is auto-launched at most once. maybeAutoLaunchAgent
// is reachable from several racy new-tab paths (swipe setTimeout + button clicks,
// split, browse, context menu); without this a second call re-queues the command
// and the agent gets typed twice into the same PTY. sendInput has no dedup.
const autoLaunchedSessionIds = new Set<string>();

// Spawn-retry window. After a coord restart the worker↔coord WS is down for
// ~10-15s while the worker re-dials; coord rejects a spawn in that window with
// FailedPrecondition "worker <fp> not connected". Retrying across the window
// turns the post-restart blip into a brief invisible delay instead of a scary
// error toast. Only surface the failure if the worker stays down past this.
const SPAWN_RETRY_MS = 12_000;

// Exported for spawnSession.test.ts. True only for the transient post-restart
// window — NOT a worker that's genuinely down (that path keeps its own error).
export function isWorkerReconnecting(err: unknown): boolean {
  return err instanceof ConnectError
    && err.code === Code.FailedPrecondition
    && /not connected/i.test(err.message);
}

// Retry `call` while it fails with the worker-reconnecting precondition, up to
// SPAWN_RETRY_MS, with capped exponential backoff. Any other error (or running
// past the deadline) propagates. Generic over the call so it's unit-testable
// without the coordClient singleton.
export async function withSpawnRetry<T>(call: () => Promise<T>): Promise<T> {
  const deadline = Date.now() + SPAWN_RETRY_MS;
  let delay = 200;
  for (let attempt = 1; ; attempt++) {
    try {
      return await call();
    } catch (err) {
      if (!isWorkerReconnecting(err) || Date.now() >= deadline) throw err;
      log.info("spawnSession", "worker reconnecting — retrying spawn", {
        attempt, delay_ms: delay,
      });
      await new Promise((r) => setTimeout(r, delay));
      delay = Math.min(Math.round(delay * 1.6), 1_000);
    }
  }
}

export async function spawnShellDetailed(
  workerFp: WorkerFp,
  folder = "~",
  sessionId?: string,
  initialViewport?: SpawnInitialViewport,
): Promise<SpawnShellResult> {
  const result = await withSpawnRetry(() =>
    coordClient.sessionsSpawn(
      buildSpawnShellRequest(workerFp, folder, sessionId, initialViewport),
    )
  );
  return {
    sessionId: result.sessionId,
    channelId: result.channelId,
    initialViewportPreclaimed: result.initialViewportPreclaimed,
    effectiveClientSeq: Number(result.effectiveClientSeq),
  };
}

export async function spawnShell(
  workerFp: WorkerFp,
  folder = "~",
  sessionId?: string,
): Promise<string> {
  return (await spawnShellDetailed(workerFp, folder, sessionId)).sessionId;
}

// Spawn into an existing workspace's folder and link the session to it.
export async function spawnInWorkspaceDetailed(
  workerFp: WorkerFp,
  workspaceId: string,
  folderPath: string,
  sessionId?: string,
  initialViewport?: SpawnInitialViewport,
): Promise<SpawnShellResult> {
  const result = await spawnShellDetailed(
    workerFp,
    folderPath,
    sessionId,
    initialViewport,
  );
  try {
    await coordClient.sessionsAssignWorkspace({
      sessionId: result.sessionId,
      workspaceId,
    });
  } catch { /* projection still groups by workspace_id once the assign event lands */ }
  return result;
}

export async function spawnInWorkspace(
  workerFp: WorkerFp,
  workspaceId: string,
  folderPath: string,
  sessionId?: string,
): Promise<string> {
  return (
    await spawnInWorkspaceDetailed(workerFp, workspaceId, folderPath, sessionId)
  ).sessionId;
}



/**
 * Wait for an opened session to land in the SPA store via the
 * sessions.events SSE projection.
 */
export async function waitForSession(sessionId: string, timeoutMs = 2_000): Promise<Session | null> {
  const deadline = Date.now() + timeoutMs;
  while (Date.now() < deadline) {
    const s = rootStore.sessions[sessionId];
    if (s) return s;
    await new Promise((r) => setTimeout(r, 25));
  }
  return null;
}

/** If auto-launch is enabled, send the configured agent command into the new PTY. Fires at most once per session. */
export function maybeAutoLaunchAgent(sessionId: string): void {
  if (!autoLaunchEnabled()) return;
  if (autoLaunchedSessionIds.has(sessionId)) return;
  const cmd = resolveAgent().command + "\r";
  const admission = sendTerminalInput(sessionId, new TextEncoder().encode(cmd));
  if (admission.accepted) autoLaunchedSessionIds.add(sessionId);
}

/** Launch the selected default agent NOW, bypassing the auto-launch toggle
 *  (quick chats always launch — that's the point). At most once per session. */
export function forceLaunchAgent(sessionId: string): void {
  if (autoLaunchedSessionIds.has(sessionId)) return;
  const cmd = resolveAgent().command + "\r";
  const admission = sendTerminalInput(sessionId, new TextEncoder().encode(cmd));
  if (admission.accepted) autoLaunchedSessionIds.add(sessionId);
}

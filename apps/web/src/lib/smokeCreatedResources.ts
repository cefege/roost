// Smoke flows create real sessions and workspaces that must not leak across browser specs.
// This module tracks only resources owned by the current tab and survives page reloads.
// The smoke backdoor composes these methods with coordinator mutations and root-store state.
// Cleanup stays scoped to recorded identifiers so live user resources are never touched.

import { coordClient } from "../connect.ts";
import { rootStore } from "../store/root.ts";
import { workerPathBasename } from "./nativePath.ts";
import type { SmokeApi } from "./smokeTypes.ts";

type SmokeCreatedResourceMethods = Pick<
  SmokeApi,
  "cleanupCreated" | "kill" | "spawnShell" | "trackCreatedSession" | "createWorkspace"
>;

export function createSmokeCreatedResourceMethods(): SmokeCreatedResourceMethods {
  const spawned = new Set<string>();
  const workspaces = new Set<string>();
  try {
    const raw = sessionStorage.getItem(CREATED_KEY);
    const carried = raw ? JSON.parse(raw) : null;
    if (carried && typeof carried === "object" && "sessions" in carried && "workspaces" in carried) {
      const { sessions, workspaces: workspaceIds } = carried as {
        sessions: unknown;
        workspaces: unknown;
      };
      if (Array.isArray(sessions)) {
        for (const sessionId of sessions) spawned.add(String(sessionId));
      }
      if (Array.isArray(workspaceIds)) {
        for (const workspaceId of workspaceIds) workspaces.add(String(workspaceId));
      }
    }
  } catch {
    // Privacy mode or unparseable state must not prevent smoke setup.
  }

  const persistCreated = () => {
    try {
      sessionStorage.setItem(CREATED_KEY, JSON.stringify({
        sessions: [...spawned],
        workspaces: [...workspaces],
      }));
    } catch {
      // Cleanup remains usable when storage is unavailable in privacy mode.
    }
  };

  return {
    async cleanupCreated() {
      const killedSessions: string[] = [];
      const deletedWorkspaces: string[] = [];
      const errors: string[] = [];
      for (const sessionId of spawned) {
        try {
          await coordClient.sessionsKill({ sessionId });
          killedSessions.push(sessionId);
        } catch (error) {
          errors.push(`kill ${sessionId}: ${String(error)}`);
        }
      }
      spawned.clear();
      for (const workspaceId of workspaces) {
        for (let attempt = 0; attempt < 2; attempt++) {
          try {
            const { workspaces: current } = await coordClient.workspacesList({});
            const workspace = current.find((item) => item.id === workspaceId);
            if (!workspace) break;
            await coordClient.workspacesDelete({ id: workspace.id, ifVersion: workspace.version });
            deletedWorkspaces.push(workspaceId);
            break;
          } catch (error) {
            if (attempt === 1) {
              errors.push(`delete workspace ${workspaceId}: ${String(error)}`);
            }
          }
        }
      }
      workspaces.clear();
      persistCreated();
      return { killedSessions, deletedWorkspaces, errors };
    },
    async kill(sessionId) {
      const response = await coordClient.sessionsKill({ sessionId });
      return { accepted: response.accepted };
    },
    async spawnShell(workerFp, folder, sessionId) {
      const response = await coordClient.sessionsSpawn({
        workerFp,
        kind: "shell",
        folder,
        sessionId,
      });
      spawned.add(response.sessionId);
      persistCreated();
      return { session_id: response.sessionId, channel_id: response.channelId };
    },
    trackCreatedSession(sessionId) {
      spawned.add(sessionId);
      persistCreated();
    },
    async createWorkspace(workerFp, folder, sessionId) {
      const existing = new Set(
        Object.values(rootStore.workspaces)
          .filter((workspace: { worker_fp?: string; name?: string }) => (
            workspace.worker_fp === workerFp
          ))
          .map((workspace: { name?: string }) => workspace.name ?? ""),
      );
      const base = workerPathBasename(workerFp, folder) || "~";
      let name = base;
      let suffix = 2;
      while (existing.has(name)) {
        name = `${base} ${suffix++}`;
      }
      const response = await coordClient.workspacesCreate({
        workerFp,
        name,
        folderPath: folder,
        attachSessionIds: [sessionId],
      });
      workspaces.add(response.workspace!.id);
      persistCreated();
      const session = rootStore.sessions[sessionId] as { channel?: number } | undefined;
      return { id: response.workspace!.id, channel: session?.channel ?? 0 };
    },
  };
}

const CREATED_KEY = "roostSmoke.created.v1";

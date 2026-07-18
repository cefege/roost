// Workspace RPC handlers: list/create/update/delete/set-sessions. Workspaces
// are optimistic-concurrency rows (version CAS) projected to the SPA via
// workspaceBus deltas. set-sessions also garbage-collects workspaces that
// lose their last session. Spread into router.ts's single router.service()
// literal. Split out of router.ts (400-line cap).

import type { ServiceImpl } from "@connectrpc/connect";
import { Code, ConnectError } from "@connectrpc/connect";
import { create } from "@bufbuild/protobuf";
import { randomUUID } from "node:crypto";
import { sql } from "kysely";
import {
  CoordinatorService,
  WorkspacesListResponseSchema, WorkspacesCreateResponseSchema,
  WorkspacesUpdateResponseSchema, WorkspacesDeleteResponseSchema,
  WorkspacesSetSessionsResponseSchema,
} from "@roost/shared/proto/coordinator_pb";
import { WorkspaceSchema } from "@roost/shared/proto/wire_pb";
import type { KyselyDB } from "../db/connection.ts";
import { workspaceBus } from "../buses.ts";
import { requireAuth } from "./auth-interceptor.ts";
import { asSessionId } from "@roost/shared/wire";
import type { SessionId } from "@roost/shared/wire";
import type { ConnectDeps } from "./router.ts";

// A workspace's session ids from the junction table. The one query every
// handler here needs after a mutation (for the proto response + the bus wire).
async function fetchSessionIds(db: KyselyDB, workspaceId: string): Promise<SessionId[]> {
  return (await db.selectFrom("workspace_sessions").select("session_id")
    .where("workspace_id", "=", workspaceId).execute()).map(r => asSessionId(r.session_id as string));
}

// snake_case wire Workspace for workspaceBus created/updated deltas. The SPA
// projector consumes this shape; keep it byte-identical to the Zod Workspace.
function wsRowToWire(row: any, sessionIds: string[]) {
  return {
    id: row.id, worker_fp: row.worker_fp,
    name: row.name, folder_path: row.folder_path,
    color: row.color ?? null, position: row.position,
    version: row.version, created_at_ms: row.created_at_ms,
    updated_at_ms: row.updated_at_ms, session_ids: sessionIds,
  };
}

// DB-coupled row→proto adapter (loads the workspace's session ids). Kept
// here as workspaces is its only caller.
async function workspaceRowToProto(db: KyselyDB, row: any) {
  return create(WorkspaceSchema, {
    id: row.id,
    workerFp: row.worker_fp,
    name: row.name,
    folderPath: row.folder_path,
    color: row.color ?? undefined,
    position: row.position,
    version: BigInt(row.version),
    createdAtMs: BigInt(row.created_at_ms),
    updatedAtMs: BigInt(row.updated_at_ms),
    sessionIds: await fetchSessionIds(db, row.id),
  });
}

type WorkspaceMethods =
  | "workspacesList" | "workspacesCreate" | "workspacesUpdate"
  | "workspacesDelete" | "workspacesSetSessions";

export function makeWorkspaceHandlers(
  deps: ConnectDeps,
): Pick<ServiceImpl<typeof CoordinatorService>, WorkspaceMethods> {
  return {
    async workspacesList(_req, ctx) {
      requireAuth(ctx.values);
      const rows = await deps.db.selectFrom("workspaces").selectAll().orderBy("position").execute();
      const workspaces = await Promise.all(rows.map(r => workspaceRowToProto(deps.db, r)));
      return create(WorkspacesListResponseSchema, { workspaces });
    },

    async workspacesCreate(req, ctx) {
      requireAuth(ctx.values);
      const id = randomUUID();
      const now = Date.now();
      let folderPath = req.folderPath;
      if (req.attachSessionIds.length > 0) {
        const sess = await deps.db.selectFrom("sessions").select(["cwd"])
          .where("id", "=", req.attachSessionIds[0]!).executeTakeFirst();
        if (sess?.cwd) folderPath = sess.cwd;
      }
      // 2026-06-15: server-side dedupe by (worker_fp, folder_path).
      // The SPA already does a client-side check before calling this
      // RPC, but two browsers (or a race) can still POST two creates
      // concurrently. Catch it here too so the DB never accumulates
      // duplicate empty workspaces. Returns the EXISTING row if one
      // already lives at that path.
      const existing = await deps.db.selectFrom("workspaces").selectAll()
        .where("worker_fp", "=", req.workerFp)
        .where("folder_path", "=", folderPath)
        .executeTakeFirst();
      if (existing) {
        const w = await workspaceRowToProto(deps.db, existing);
        return create(WorkspacesCreateResponseSchema, { workspace: w });
      }
      const result = await deps.db.transaction().execute(async (trx) => {
        const position = await trx.selectFrom("workspaces").select(trx.fn.countAll<number>().as("cnt"))
          .executeTakeFirst().then(r => Number(r?.cnt ?? 0));
        await trx.insertInto("workspaces").values({
          id, worker_fp: req.workerFp, name: req.name,
          folder_path: folderPath, color: req.color ?? null,
          position, version: 0, created_at_ms: now, updated_at_ms: now,
        }).execute();
        if (req.attachSessionIds.length > 0) {
          await trx.deleteFrom("workspace_sessions").where("session_id", "in", req.attachSessionIds).execute();
          await trx.insertInto("workspace_sessions").values(req.attachSessionIds.map(sid => ({
            workspace_id: id, session_id: sid, added_at_ms: now,
          }))).execute();
        }
        const row = await trx.selectFrom("workspaces").selectAll().where("id", "=", id).executeTakeFirstOrThrow();
        return row;
      });
      const w = await workspaceRowToProto(deps.db, result);
      workspaceBus.publish({ kind: "created", workspace: wsRowToWire(result, await fetchSessionIds(deps.db, result.id)) as any });
      return create(WorkspacesCreateResponseSchema, { workspace: w });
    },

    async workspacesUpdate(req, ctx) {
      requireAuth(ctx.values);
      const now = Date.now();
      const result = await deps.db.updateTable("workspaces").set({
        ...(req.name !== undefined && { name: req.name }),
        ...(req.folderPath !== undefined && { folder_path: req.folderPath }),
        ...(req.color !== undefined && { color: req.color }),
        ...(req.position !== undefined && { position: req.position }),
        updated_at_ms: now,
        version: sql`version + 1`,
      })
        .where("id", "=", req.id)
        .where("version", "=", Number(req.ifVersion))
        .returningAll().executeTakeFirst();
      if (!result) throw new ConnectError("version mismatch", Code.FailedPrecondition);
      const w = await workspaceRowToProto(deps.db, result);
      workspaceBus.publish({ kind: "updated", workspace: wsRowToWire(result, await fetchSessionIds(deps.db, result.id)) as any });
      return create(WorkspacesUpdateResponseSchema, { workspace: w });
    },

    async workspacesDelete(req, ctx) {
      requireAuth(ctx.values);
      const result = await deps.db.deleteFrom("workspaces")
        .where("id", "=", req.id).where("version", "=", Number(req.ifVersion))
        .returningAll().executeTakeFirst();
      if (!result) throw new ConnectError("version mismatch or not found", Code.FailedPrecondition);
      workspaceBus.publish({ kind: "deleted", id: result.id as any });
      return create(WorkspacesDeleteResponseSchema, { ok: true });
    },

    async workspacesSetSessions(req, ctx) {
      requireAuth(ctx.values);
      const now = Date.now();
      const { result, sourceRows, orphanIds } = await deps.db.transaction().execute(async (trx) => {
        const result = await trx.updateTable("workspaces").set({
          updated_at_ms: now, version: sql`version + 1`,
        }).where("id", "=", req.id).where("version", "=", Number(req.ifVersion))
          .returningAll().executeTakeFirst();
        if (!result) throw new ConnectError("version mismatch", Code.FailedPrecondition);

        const sourceWorkspaceIds = req.sessionIds.length > 0
          ? (await trx.selectFrom("workspace_sessions").select("workspace_id")
              .where("session_id", "in", req.sessionIds)
              .where("workspace_id", "!=", req.id).execute()).map(r => r.workspace_id as string)
          : [];
        const uniqueSources = [...new Set(sourceWorkspaceIds)];

        await trx.deleteFrom("workspace_sessions").where("workspace_id", "=", req.id).execute();
        if (req.sessionIds.length > 0) {
          await trx.deleteFrom("workspace_sessions").where("session_id", "in", req.sessionIds)
            .where("workspace_id", "!=", req.id).execute();
          await trx.insertInto("workspace_sessions").values(req.sessionIds.map(sid => ({
            workspace_id: req.id, session_id: sid, added_at_ms: now,
          }))).execute();
        }

        const sourceRows = uniqueSources.length > 0
          ? await trx.selectFrom("workspaces").selectAll().where("id", "in", uniqueSources).execute()
          : [];

        const affectedIds = [req.id, ...uniqueSources];
        const remaining = await trx.selectFrom("workspace_sessions").select(["workspace_id"])
          .where("workspace_id", "in", affectedIds).execute();
        const haveSessions = new Set(remaining.map(r => r.workspace_id as string));
        const orphanIds = affectedIds.filter(id => !haveSessions.has(id));
        if (orphanIds.length > 0) {
          await trx.deleteFrom("workspaces").where("id", "in", orphanIds).execute();
        }
        return { result, sourceRows, orphanIds };
      });

      const orphanSet = new Set(orphanIds);
      if (orphanSet.has(result.id)) {
        workspaceBus.publish({ kind: "deleted", id: result.id as any });
      } else {
        const sids = await fetchSessionIds(deps.db, result.id);
        workspaceBus.publish({ kind: "sessions-set", id: result.id as any, session_ids: sids, version: result.version });
      }
      for (const srcRow of sourceRows) {
        if (orphanSet.has(srcRow.id)) {
          workspaceBus.publish({ kind: "deleted", id: srcRow.id as any });
        } else {
          const sids = await fetchSessionIds(deps.db, srcRow.id);
          workspaceBus.publish({ kind: "sessions-set", id: srcRow.id as any, session_ids: sids, version: srcRow.version });
        }
      }

      if (orphanSet.has(result.id)) {
        return create(WorkspacesSetSessionsResponseSchema, {
          workspace: create(WorkspaceSchema, {
            id: result.id, workerFp: result.worker_fp, name: result.name,
            folderPath: result.folder_path, color: result.color ?? undefined,
            position: result.position, version: BigInt(result.version),
            createdAtMs: BigInt(result.created_at_ms), updatedAtMs: BigInt(result.updated_at_ms),
            sessionIds: [],
          }),
        });
      }
      const w = await workspaceRowToProto(deps.db, result);
      return create(WorkspacesSetSessionsResponseSchema, { workspace: w });
    },
  };
}

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
import { sameWorkerFolder } from "@roost/shared/native-path";
import type { KyselyDB } from "../db/connection.ts";
import type { WorkspacesTable } from "../db/schema.ts";
import { workspaceBus } from "../buses.ts";
import { requireDashboardActor } from "./auth-interceptor.ts";
import { asSessionId, asWorkspaceId } from "@roost/shared/wire";
import type { SessionId, Workspace } from "@roost/shared/wire";
import type { ConnectDeps } from "./router.ts";

// A workspace's session ids from the junction table. The one query every
// handler here needs after a mutation (for the proto response + the bus wire).
async function fetchSessionIds(
  db: KyselyDB,
  dashboardId: string,
  workspaceId: string,
): Promise<SessionId[]> {
  return (await db.selectFrom("workspace_sessions").select("session_id")
    .where("workspace_id", "=", workspaceId)
    .where("dashboard_id", "=", dashboardId)
    .execute()).map(r => asSessionId(r.session_id as string));
}

// snake_case wire Workspace for workspaceBus created/updated deltas. The SPA
// projector consumes this shape; keep it byte-identical to the Zod Workspace.
function wsRowToWire(row: WorkspacesTable, sessionIds: SessionId[]): Workspace {
  const wireWorkspace = {
    id: row.id, worker_fp: row.worker_fp,
    name: row.name, folder_path: row.folder_path,
    color: row.color ?? null, position: row.position,
    version: row.version, created_at_ms: row.created_at_ms,
    updated_at_ms: row.updated_at_ms, session_ids: sessionIds,
  };
  // Kysely exposes persisted IDs as strings while Workspace carries Zod brands.
  return wireWorkspace as unknown as Workspace;
}

// DB-coupled row→proto adapter (loads the workspace's session ids). Kept
// here as workspaces is its only caller.
async function workspaceRowToProto(db: KyselyDB, dashboardId: string, row: WorkspacesTable) {
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
    sessionIds: await fetchSessionIds(db, dashboardId, row.id),
  });
}

type WorkspaceMethods =
  | "workspacesList" | "workspacesCreate" | "workspacesUpdate"
  | "workspacesDelete" | "workspacesSetSessions";

export type WorkspaceHandlers = Pick<ServiceImpl<typeof CoordinatorService>, WorkspaceMethods>;

export function makeWorkspaceHandlers(deps: ConnectDeps): WorkspaceHandlers {
  return {
    async workspacesList(_req, ctx) {
      const actor = requireDashboardActor(ctx.values);
      const rows = await deps.db.selectFrom("workspaces").selectAll()
        .where("dashboard_id", "=", actor.dashboardId)
        .orderBy("position").execute();
      const workspaces = await Promise.all(
        rows.map(r => workspaceRowToProto(deps.db, actor.dashboardId, r)),
      );
      return create(WorkspacesListResponseSchema, { workspaces });
    },

    async workspacesCreate(req, ctx) {
      const actor = requireDashboardActor(ctx.values);
      const id = randomUUID();
      const now = Date.now();
      const result = await deps.db.transaction().execute(async (trx) => {
        const worker = await trx.selectFrom("workers").select("os")
          .where("fp", "=", req.workerFp)
          .where("dashboard_id", "=", actor.dashboardId)
          .where("deleted_at_ms", "is", null)
          .executeTakeFirst();
        if (!worker) throw new ConnectError("worker not found", Code.NotFound);

        const attachedSessions = req.attachSessionIds.length > 0
          ? await trx.selectFrom("sessions").select(["id", "cwd"])
              .where("id", "in", req.attachSessionIds)
              .where("dashboard_id", "=", actor.dashboardId)
              .execute()
          : [];
        if (attachedSessions.length !== new Set(req.attachSessionIds).size) {
          throw new ConnectError("session not found", Code.NotFound);
        }

        let folderPath = req.folderPath;
        const firstAttachedSession = attachedSessions.find(
          (session) => session.id === req.attachSessionIds[0],
        );
        if (firstAttachedSession?.cwd) folderPath = firstAttachedSession.cwd;

        // Dedupe by PATH IDENTITY, not string equality: on darwin `/tmp` and
        // `/private/tmp` are one directory, and a row can hold either form —
        // sessions report the realpath (worker canonicalSessionCwd) while a row
        // written before that, or from a typed path, holds what the user saw.
        // Raw equality created a second workspace for the same folder, and the
        // SPA then showed neither row's name (folderKey.ts resolves by identity).
        // The SPA already checks client-side; this also catches two concurrent
        // creates — the scan runs INSIDE the insert transaction so a racing
        // create serializes behind SQLite's write lock instead of slipping past
        // an already-committed SELECT. Returns the EXISTING row when one lives
        // at that path.
        const rows = await trx.selectFrom("workspaces").selectAll()
          .where("worker_fp", "=", req.workerFp)
          .where("dashboard_id", "=", actor.dashboardId)
          .execute();
        const existing = rows.find((r) => sameWorkerFolder(worker.os, r.folder_path, folderPath));
        if (existing) return { kind: "existing" as const, row: existing };
        const position = await trx.selectFrom("workspaces").select(trx.fn.countAll<number>().as("cnt"))
          .where("dashboard_id", "=", actor.dashboardId)
          .executeTakeFirst().then(r => Number(r?.cnt ?? 0));
        await trx.insertInto("workspaces").values({
          id, dashboard_id: actor.dashboardId, worker_fp: req.workerFp, name: req.name,
          folder_path: folderPath, color: req.color ?? null,
          position, version: 0, created_at_ms: now, updated_at_ms: now,
        }).execute();
        if (req.attachSessionIds.length > 0) {
          await trx.deleteFrom("workspace_sessions")
            .where("session_id", "in", req.attachSessionIds)
            .where("dashboard_id", "=", actor.dashboardId)
            .execute();
          await trx.insertInto("workspace_sessions").values(req.attachSessionIds.map(sid => ({
            workspace_id: id, dashboard_id: actor.dashboardId, session_id: sid, added_at_ms: now,
          }))).execute();
        }
        const row = await trx.selectFrom("workspaces").selectAll()
          .where("id", "=", id)
          .where("dashboard_id", "=", actor.dashboardId)
          .executeTakeFirstOrThrow();
        return { kind: "created" as const, row };
      });
      if (result.kind === "existing") {
        const w = await workspaceRowToProto(deps.db, actor.dashboardId, result.row);
        return create(WorkspacesCreateResponseSchema, { workspace: w });
      }
      const w = await workspaceRowToProto(deps.db, actor.dashboardId, result.row);
      workspaceBus.publish({
        kind: "created",
        _dashboard_id: actor.dashboardId,
        workspace: wsRowToWire(
          result.row,
          await fetchSessionIds(deps.db, actor.dashboardId, result.row.id),
        ),
      });
      return create(WorkspacesCreateResponseSchema, { workspace: w });
    },

    async workspacesUpdate(req, ctx) {
      const actor = requireDashboardActor(ctx.values);
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
        .where("dashboard_id", "=", actor.dashboardId)
        .where("version", "=", Number(req.ifVersion))
        .returningAll().executeTakeFirst();
      if (!result) throw new ConnectError("version mismatch", Code.FailedPrecondition);
      const w = await workspaceRowToProto(deps.db, actor.dashboardId, result);
      workspaceBus.publish({
        kind: "updated",
        _dashboard_id: actor.dashboardId,
        workspace: wsRowToWire(
          result,
          await fetchSessionIds(deps.db, actor.dashboardId, result.id),
        ),
      });
      return create(WorkspacesUpdateResponseSchema, { workspace: w });
    },

    async workspacesDelete(req, ctx) {
      const actor = requireDashboardActor(ctx.values);
      const result = await deps.db.deleteFrom("workspaces")
        .where("id", "=", req.id)
        .where("dashboard_id", "=", actor.dashboardId)
        .where("version", "=", Number(req.ifVersion))
        .returningAll().executeTakeFirst();
      if (!result) throw new ConnectError("version mismatch or not found", Code.FailedPrecondition);
      workspaceBus.publish({
        kind: "deleted",
        id: asWorkspaceId(result.id),
        _dashboard_id: actor.dashboardId,
      });
      return create(WorkspacesDeleteResponseSchema, { ok: true });
    },

    async workspacesSetSessions(req, ctx) {
      const actor = requireDashboardActor(ctx.values);
      const now = Date.now();
      const { result, sourceRows, orphanIds } = await deps.db.transaction().execute(async (trx) => {
        const workspace = await trx.selectFrom("workspaces").select("id")
          .where("id", "=", req.id)
          .where("dashboard_id", "=", actor.dashboardId)
          .where("version", "=", Number(req.ifVersion))
          .executeTakeFirst();
        if (!workspace) throw new ConnectError("version mismatch", Code.FailedPrecondition);

        if (req.sessionIds.length > 0) {
          const sessionRows = await trx.selectFrom("sessions").select("id")
            .where("id", "in", req.sessionIds)
            .where("dashboard_id", "=", actor.dashboardId)
            .execute();
          if (sessionRows.length !== new Set(req.sessionIds).size) {
            throw new ConnectError("session not found", Code.NotFound);
          }
        }

        const result = await trx.updateTable("workspaces").set({
          updated_at_ms: now, version: sql`version + 1`,
        })
          .where("id", "=", req.id)
          .where("dashboard_id", "=", actor.dashboardId)
          .where("version", "=", Number(req.ifVersion))
          .returningAll().executeTakeFirst();
        if (!result) throw new ConnectError("version mismatch", Code.FailedPrecondition);

        const sourceWorkspaceIds = req.sessionIds.length > 0
          ? (await trx.selectFrom("workspace_sessions").select("workspace_id")
              .where("session_id", "in", req.sessionIds)
              .where("dashboard_id", "=", actor.dashboardId)
              .where("workspace_id", "!=", req.id).execute()).map(r => r.workspace_id as string)
          : [];
        const uniqueSources = [...new Set(sourceWorkspaceIds)];

        await trx.deleteFrom("workspace_sessions")
          .where("workspace_id", "=", req.id)
          .where("dashboard_id", "=", actor.dashboardId)
          .execute();
        if (req.sessionIds.length > 0) {
          await trx.deleteFrom("workspace_sessions")
            .where("session_id", "in", req.sessionIds)
            .where("dashboard_id", "=", actor.dashboardId)
            .where("workspace_id", "!=", req.id)
            .execute();
          await trx.insertInto("workspace_sessions").values(req.sessionIds.map(sid => ({
            workspace_id: req.id, dashboard_id: actor.dashboardId, session_id: sid, added_at_ms: now,
          }))).execute();
        }

        const sourceRows = uniqueSources.length > 0
          ? await trx.selectFrom("workspaces").selectAll()
              .where("id", "in", uniqueSources)
              .where("dashboard_id", "=", actor.dashboardId)
              .execute()
          : [];

        const affectedIds = [req.id, ...uniqueSources];
        const remaining = await trx.selectFrom("workspace_sessions").select(["workspace_id"])
          .where("workspace_id", "in", affectedIds)
          .where("dashboard_id", "=", actor.dashboardId)
          .execute();
        const haveSessions = new Set(remaining.map(r => r.workspace_id as string));
        const orphanIds = affectedIds.filter(id => !haveSessions.has(id));
        if (orphanIds.length > 0) {
          await trx.deleteFrom("workspaces")
            .where("id", "in", orphanIds)
            .where("dashboard_id", "=", actor.dashboardId)
            .execute();
        }
        return { result, sourceRows, orphanIds };
      });

      const orphanSet = new Set(orphanIds);
      if (orphanSet.has(result.id)) {
        workspaceBus.publish({
          kind: "deleted",
          id: asWorkspaceId(result.id),
          _dashboard_id: actor.dashboardId,
        });
      } else {
        const sids = await fetchSessionIds(deps.db, actor.dashboardId, result.id);
        workspaceBus.publish({
          kind: "sessions-set",
          _dashboard_id: actor.dashboardId,
          id: asWorkspaceId(result.id),
          session_ids: sids,
          version: result.version,
        });
      }
      for (const srcRow of sourceRows) {
        if (orphanSet.has(srcRow.id)) {
          workspaceBus.publish({
            kind: "deleted",
            id: asWorkspaceId(srcRow.id),
            _dashboard_id: actor.dashboardId,
          });
        } else {
          const sids = await fetchSessionIds(deps.db, actor.dashboardId, srcRow.id);
          workspaceBus.publish({
            kind: "sessions-set",
            _dashboard_id: actor.dashboardId,
            id: asWorkspaceId(srcRow.id),
            session_ids: sids,
            version: srcRow.version,
          });
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
      const w = await workspaceRowToProto(deps.db, actor.dashboardId, result);
      return create(WorkspacesSetSessionsResponseSchema, { workspace: w });
    },
  };
}

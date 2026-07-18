// Task-queue RPC handlers: list/enqueue/next-pending/set-state/cancel. Tasks
// are a simple claimable work queue; every mutation publishes a taskBus delta
// (created or state) so other browsers' QueueViews update without a refresh.
// Spread into router.ts's single router.service() literal. Split out of
// router.ts (400-line cap).

import type { ServiceImpl } from "@connectrpc/connect";
import { Code, ConnectError } from "@connectrpc/connect";
import { create } from "@bufbuild/protobuf";
import { randomUUID } from "node:crypto";
import {
  CoordinatorService,
  TasksListResponseSchema, TasksEnqueueResponseSchema,
  TasksNextPendingResponseSchema, TasksSetStateResponseSchema, TasksCancelResponseSchema,
} from "@roost/shared/proto/coordinator_pb";
import { taskRowToProto } from "@roost/shared/wire/row-proto";
import { taskBus } from "../buses.ts";
import { requireAuth } from "./auth-interceptor.ts";
import type { ConnectDeps } from "./router.ts";

// Emit a `state` delta on the taskBus. Used by next-pending/set-state/cancel.
// taskBus carries the proto Task directly (TaskBusMsg) so no JSON
// parse/stringify roundtrip fires on the hot path.
function publishTaskState(row: Parameters<typeof taskRowToProto>[0]): void {
  taskBus.publish({ kind: "state", task: taskRowToProto(row) });
}

type TaskMethods =
  | "tasksList" | "tasksEnqueue" | "tasksNextPending" | "tasksSetState" | "tasksCancel";

export function makeTaskHandlers(
  deps: ConnectDeps,
): Pick<ServiceImpl<typeof CoordinatorService>, TaskMethods> {
  return {
    async tasksList(req, ctx) {
      requireAuth(ctx.values);
      let q = deps.db.selectFrom("tasks").selectAll().orderBy("enqueued_at_ms");
      if (req.state) q = q.where("state", "=", req.state as any);
      const rows = await q.execute();
      return create(TasksListResponseSchema, { tasks: rows.map(taskRowToProto) });
    },

    async tasksEnqueue(req, ctx) {
      requireAuth(ctx.values);
      // Reject malformed payload at the wire boundary — symmetric to
      // mcpCreate. Without this, a webhook caller can persist garbage
      // (the proto type is `string`, no parse validation) and every
      // downstream consumer (taskBus, SPA) sees task.payload=null
      // from _safeJsonParse's fallback, violating the Task Zod
      // schema that declares payload as a non-nullable object.
      try { JSON.parse(req.payloadJson); }
      catch (e) {
        throw new ConnectError(`invalid payloadJson: ${(e as Error).message}`, Code.InvalidArgument);
      }
      const id = randomUUID();
      const now = Date.now();
      await deps.db.insertInto("tasks").values({
        id, state: "pending", payload_json: req.payloadJson,
        enqueued_at_ms: now, claimed_at_ms: null, claimed_by: null,
        finished_at_ms: null, result_json: null,
        completion_check: req.completionCheck ?? null,
        completion_check_last_attempt_ms: null,
        claim_ttl_ms: Number(req.claimTtlMs ?? BigInt(15 * 60 * 1000)),
      }).execute();
      const row = await deps.db.selectFrom("tasks").selectAll().where("id", "=", id).executeTakeFirstOrThrow();
      const task = taskRowToProto(row);
      taskBus.publish({ kind: "created", task });
      return create(TasksEnqueueResponseSchema, { task });
    },

    async tasksNextPending(_req, ctx) {
      const caller = requireAuth(ctx.values);
      const now = Date.now();
      const result = await deps.db.updateTable("tasks").set({
        state: "claimed", claimed_at_ms: now, claimed_by: caller.fingerprint,
      }).where("id", "=",
        deps.db.selectFrom("tasks").select("id").where("state", "=", "pending")
          .orderBy("enqueued_at_ms").limit(1)
      ).returningAll().executeTakeFirst();
      if (!result) return create(TasksNextPendingResponseSchema, {});
      publishTaskState(result);
      return create(TasksNextPendingResponseSchema, { task: taskRowToProto(result) });
    },

    async tasksSetState(req, ctx) {
      const caller = requireAuth(ctx.values);
      const now = Date.now();
      const terminal = ["done", "failed", "cancelled"].includes(req.state);
      const existing = await deps.db.selectFrom("tasks").selectAll().where("id", "=", req.id).executeTakeFirst();
      if (!existing) throw new ConnectError("task not found", Code.NotFound);
      if (existing.claimed_by && existing.claimed_by !== caller.fingerprint) {
        throw new ConnectError("task claimed by different worker", Code.PermissionDenied);
      }
      const result = await deps.db.updateTable("tasks").set({
        state: req.state as any,
        ...(terminal && { finished_at_ms: now }),
        ...(req.resultJson !== undefined && { result_json: req.resultJson }),
      }).where("id", "=", req.id).returningAll().executeTakeFirstOrThrow();
      publishTaskState(result);
      return create(TasksSetStateResponseSchema, { task: taskRowToProto(result) });
    },

    async tasksCancel(req, ctx) {
      requireAuth(ctx.values);
      const result = await deps.db.updateTable("tasks").set({
        state: "cancelled", finished_at_ms: Date.now(),
      }).where("id", "=", req.id)
        .where("state", "not in", ["done", "failed", "cancelled"])
        .returningAll().executeTakeFirst();
      if (!result) throw new ConnectError("task not found or already terminal", Code.NotFound);
      publishTaskState(result);
      return create(TasksCancelResponseSchema, { task: taskRowToProto(result) });
    },
  };
}

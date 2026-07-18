// Task queue (worker pull-claim model). Tasks live in coord SQL; coord
// hands them to workers via /tasks.nextPending tRPC query. Webhook-
// minted tokens can enqueue via the .enqueue mutation.

import { z } from "zod";
import { TaskId, WorkerFp } from "./brand.ts";

export const TaskState = z.enum([
  "pending",
  "claimed",
  "running",
  "done",
  "failed",
  "cancelled",
]);
export type TaskState = z.infer<typeof TaskState>;

export const Task = z.object({
  id: TaskId,
  state: TaskState,
  payload: z.record(z.string(), z.unknown()),      // free-form JSON the worker interprets
  enqueued_at_ms: z.number().int().positive(),
  claimed_at_ms: z.number().int().positive().nullable(),
  claimed_by: WorkerFp.nullable(),
  finished_at_ms: z.number().int().positive().nullable(),
  result: z.record(z.string(), z.unknown()).nullable(),
  // R8.1 completion-promise: a shell-checkable command the worker can
  // run to know if the task's effect has landed (e.g. PR merged).
  // When set, the worker re-checks until the command exits 0 or
  // timeout. Repeats are bounded by claim_ttl_ms.
  completion_check: z.string().nullable(),
  completion_check_last_attempt_ms: z.number().int().positive().nullable(),
  claim_ttl_ms: z.number().int().positive(),
});
export type Task = z.infer<typeof Task>;

export const TaskDelta = z.discriminatedUnion("kind", [
  z.object({ kind: z.literal("created"), task: Task }),
  z.object({ kind: z.literal("state"), task: Task }),
]);
export type TaskDelta = z.infer<typeof TaskDelta>;

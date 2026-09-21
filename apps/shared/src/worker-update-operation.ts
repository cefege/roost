// Authoritative coordinator-owned worker update operation and bounded report.
// Coordinator job persistence, CLI status, and the SPA share this contract;
// deployment stdout and environment data never enter it.

import { z } from "zod";
import {
  KeeperContractV1Schema,
  KeeperRuntimeObservationV1Schema,
} from "./keeper-update.ts";
import { WorkerFp } from "./wire/brand.ts";

export const WORKER_UPDATE_MAX_EVENTS = 256;
export const WORKER_UPDATE_MAX_TEXT_LENGTH = 2_048;
export const WORKER_UPDATE_MAX_HOST_LENGTH = 253;
export const WORKER_UPDATE_HOST_RE = /^[A-Za-z0-9.-]+$/;
export const WORKER_UPDATE_GIT_SHA_RE = /^(?:[0-9a-f]{40}|[0-9a-f]{64})$/;
const WORKER_UPDATE_JOB_ID_RE = /^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$/;
const SINGLE_LINE_TEXT_RE = /^[^\r\n]*$/;
const SafeTimestampSchema = z.number().int().nonnegative().max(Number.MAX_SAFE_INTEGER);
const BoundedTextSchema = z.string()
  .max(WORKER_UPDATE_MAX_TEXT_LENGTH)
  .regex(SINGLE_LINE_TEXT_RE, "text must be a single line");
const NonemptyBoundedTextSchema = BoundedTextSchema.min(1);

export const WorkerUpdateSourceSchema = z.enum(["push", "manual", "catchup"]);
export type WorkerUpdateSource = z.infer<typeof WorkerUpdateSourceSchema>;

export const WorkerUpdateStatusSchema = z.enum([
  "queued",
  "running",
  "verifying",
  "waiting",
  "blocked",
  "succeeded",
  "failed",
]);
export type WorkerUpdateStatus = z.infer<typeof WorkerUpdateStatusSchema>;

export const WorkerUpdatePhaseSchema = z.enum([
  "preflight",
  "recovery",
  "staging",
  "activation",
  "confirmation",
  "settled",
]);
export type WorkerUpdatePhase = z.infer<typeof WorkerUpdatePhaseSchema>;

export const WorkerUpdateReasonCodeSchema = z.enum([
  "offline",
  "busy",
  "source_unavailable",
  "runtime_unavailable",
  "keeper_incompatible",
  "keeper_unproven",
  "journal_conflict",
  "coordinator_restarting",
  "confirmation_pending",
  "confirmation_timeout",
  "deploy_failed",
  "report_unavailable",
  "unsupported_platform",
]);
export type WorkerUpdateReasonCode = z.infer<typeof WorkerUpdateReasonCodeSchema>;

export const WorkerUpdateOperationSchema = z.object({
  jobId: z.string().regex(WORKER_UPDATE_JOB_ID_RE),
  workerFp: WorkerFp,
  host: z.string().min(1).max(WORKER_UPDATE_MAX_HOST_LENGTH).regex(WORKER_UPDATE_HOST_RE),
  revision: z.number().int().positive().max(Number.MAX_SAFE_INTEGER),
  targetGitSha: z.string().regex(WORKER_UPDATE_GIT_SHA_RE),
  source: WorkerUpdateSourceSchema,
  status: WorkerUpdateStatusSchema,
  phase: WorkerUpdatePhaseSchema,
  reasonCode: WorkerUpdateReasonCodeSchema.nullable(),
  message: BoundedTextSchema.nullable(),
  createdAtMs: SafeTimestampSchema,
  updatedAtMs: SafeTimestampSchema,
  startedAtMs: SafeTimestampSchema.nullable(),
  completedAtMs: SafeTimestampSchema.nullable(),
  nextAttemptAtMs: SafeTimestampSchema.nullable(),
  exitCode: z.number().int().nullable(),
}).strict().readonly().superRefine((operation, context) => {
  if (operation.updatedAtMs < operation.createdAtMs) {
    context.addIssue({
      code: z.ZodIssueCode.custom,
      path: ["updatedAtMs"],
      message: "updatedAtMs precedes createdAtMs",
    });
  }
  if (operation.startedAtMs !== null && operation.startedAtMs < operation.createdAtMs) {
    context.addIssue({
      code: z.ZodIssueCode.custom,
      path: ["startedAtMs"],
      message: "startedAtMs precedes createdAtMs",
    });
  }
  const completionFloor = operation.startedAtMs ?? operation.createdAtMs;
  if (operation.completedAtMs !== null && operation.completedAtMs < completionFloor) {
    context.addIssue({
      code: z.ZodIssueCode.custom,
      path: ["completedAtMs"],
      message: "completedAtMs precedes operation start",
    });
  }
});
export type WorkerUpdateOperation = z.infer<typeof WorkerUpdateOperationSchema>;

export const WorkerUpdateProgressEventSchema = z.object({
  atMs: SafeTimestampSchema,
  phase: WorkerUpdatePhaseSchema,
  message: NonemptyBoundedTextSchema,
}).strict().readonly();
export type WorkerUpdateProgressEvent = z.infer<
  typeof WorkerUpdateProgressEventSchema
>;

export const WorkerUpdateJournalEvidenceSchema = z.object({
  path: z.string().min(1).max(4_096).regex(SINGLE_LINE_TEXT_RE),
  phase: z.string().min(1).max(128).regex(SINGLE_LINE_TEXT_RE).nullable(),
  ownerId: z.string().min(1).max(256).regex(SINGLE_LINE_TEXT_RE).nullable(),
  rolloutId: z.string().min(1).max(256).regex(SINGLE_LINE_TEXT_RE).nullable(),
  priorSha: z.string().regex(WORKER_UPDATE_GIT_SHA_RE).nullable(),
  targetSha: z.string().regex(WORKER_UPDATE_GIT_SHA_RE).nullable(),
}).strict().readonly();
export type WorkerUpdateJournalEvidence = z.infer<
  typeof WorkerUpdateJournalEvidenceSchema
>;

export const WorkerUpdateFailureSchema = z.object({
  code: NonemptyBoundedTextSchema,
  phase: WorkerUpdatePhaseSchema,
  message: NonemptyBoundedTextSchema,
  journal: WorkerUpdateJournalEvidenceSchema.nullable(),
  expectedKeeper: KeeperRuntimeObservationV1Schema.nullable(),
  observedKeeper: KeeperRuntimeObservationV1Schema.nullable(),
  targetContract: KeeperContractV1Schema.nullable(),
}).strict().readonly();
export type WorkerUpdateFailure = z.infer<typeof WorkerUpdateFailureSchema>;

export const WorkerUpdateReportSchema = z.object({
  schemaVersion: z.literal(1),
  operation: WorkerUpdateOperationSchema,
  observedGitSha: z.string().regex(WORKER_UPDATE_GIT_SHA_RE).nullable(),
  coordinatorOrigin: z.string().min(1).max(2_048).url()
    .regex(SINGLE_LINE_TEXT_RE)
    .refine((value) => {
      const url = new URL(value);
      return value === url.origin && !url.username && !url.password;
    }, "coordinatorOrigin must be a credential-free origin"),
  failure: WorkerUpdateFailureSchema.nullable(),
  events: z.array(WorkerUpdateProgressEventSchema).max(WORKER_UPDATE_MAX_EVENTS).readonly(),
}).strict().readonly();
export type WorkerUpdateReport = z.infer<typeof WorkerUpdateReportSchema>;

export interface WorkerUpdateStartRequest {
  readonly workerFp: string;
  readonly host: string;
  readonly expectedGitSha: string;
  readonly source: WorkerUpdateSource;
  readonly sourceRoot: string;
  readonly sourceMode: "coordinator-pinned";
}

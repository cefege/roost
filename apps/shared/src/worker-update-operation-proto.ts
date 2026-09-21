// Converts authoritative worker-update operations and reports to protobuf.
// Enum and bigint checks reject unknown or lossy values before they can replace
// a newer coordinator-owned summary in the browser.

import { create } from "@bufbuild/protobuf";
import {
  WorkerUpdateFailureSchema as WorkerUpdateFailureProtoSchema,
  WorkerUpdateJournalEvidenceSchema as WorkerUpdateJournalEvidenceProtoSchema,
  WorkerUpdateOperationSchema as WorkerUpdateOperationProtoSchema,
  WorkerUpdatePhase as ProtoPhase,
  WorkerUpdateProgressEventSchema as WorkerUpdateProgressEventProtoSchema,
  WorkerUpdateReportSchema as WorkerUpdateReportProtoSchema,
  WorkerUpdateSource as ProtoSource,
  WorkerUpdateStatus as ProtoStatus,
  type WorkerUpdateFailure as WorkerUpdateFailureProto,
  type WorkerUpdateOperation as WorkerUpdateOperationProto,
  type WorkerUpdateReport as WorkerUpdateReportProto,
} from "./gen/roost/v1/wire_pb.ts";
import {
  keeperContractFromProto,
  keeperContractToProto,
  keeperRuntimeObservationFromProto,
  keeperRuntimeObservationToProto,
} from "./keeper-update-proto.ts";
import {
  WorkerUpdateOperationSchema,
  WorkerUpdateReportSchema,
  type WorkerUpdateFailure,
  type WorkerUpdateOperation,
  type WorkerUpdatePhase,
  type WorkerUpdateReport,
  type WorkerUpdateSource,
  type WorkerUpdateStatus,
} from "./worker-update-operation.ts";

const sourceToProto: Record<WorkerUpdateSource, ProtoSource> = {
  push: ProtoSource.PUSH,
  manual: ProtoSource.MANUAL,
  catchup: ProtoSource.CATCHUP,
};
const sourceFromProto = new Map<ProtoSource, WorkerUpdateSource>([
  [ProtoSource.PUSH, "push"],
  [ProtoSource.MANUAL, "manual"],
  [ProtoSource.CATCHUP, "catchup"],
]);
const statusToProto: Record<WorkerUpdateStatus, ProtoStatus> = {
  queued: ProtoStatus.QUEUED,
  running: ProtoStatus.RUNNING,
  verifying: ProtoStatus.VERIFYING,
  waiting: ProtoStatus.WAITING,
  blocked: ProtoStatus.BLOCKED,
  succeeded: ProtoStatus.SUCCEEDED,
  failed: ProtoStatus.FAILED,
};
const statusFromProto = new Map<ProtoStatus, WorkerUpdateStatus>([
  [ProtoStatus.QUEUED, "queued"],
  [ProtoStatus.RUNNING, "running"],
  [ProtoStatus.VERIFYING, "verifying"],
  [ProtoStatus.WAITING, "waiting"],
  [ProtoStatus.BLOCKED, "blocked"],
  [ProtoStatus.SUCCEEDED, "succeeded"],
  [ProtoStatus.FAILED, "failed"],
]);
const phaseToProto: Record<WorkerUpdatePhase, ProtoPhase> = {
  preflight: ProtoPhase.PREFLIGHT,
  recovery: ProtoPhase.RECOVERY,
  staging: ProtoPhase.STAGING,
  activation: ProtoPhase.ACTIVATION,
  confirmation: ProtoPhase.CONFIRMATION,
  settled: ProtoPhase.SETTLED,
};
const phaseFromProto = new Map<ProtoPhase, WorkerUpdatePhase>([
  [ProtoPhase.PREFLIGHT, "preflight"],
  [ProtoPhase.RECOVERY, "recovery"],
  [ProtoPhase.STAGING, "staging"],
  [ProtoPhase.ACTIVATION, "activation"],
  [ProtoPhase.CONFIRMATION, "confirmation"],
  [ProtoPhase.SETTLED, "settled"],
]);

export function workerUpdateOperationToProto(
  operation: WorkerUpdateOperation,
): WorkerUpdateOperationProto {
  const checked = WorkerUpdateOperationSchema.parse(operation);
  return create(WorkerUpdateOperationProtoSchema, {
    jobId: checked.jobId,
    workerFp: checked.workerFp,
    host: checked.host,
    revision: BigInt(checked.revision),
    targetGitSha: checked.targetGitSha,
    source: sourceToProto[checked.source],
    status: statusToProto[checked.status],
    phase: phaseToProto[checked.phase],
    reasonCode: checked.reasonCode ?? undefined,
    message: checked.message ?? undefined,
    createdAtMs: BigInt(checked.createdAtMs),
    updatedAtMs: BigInt(checked.updatedAtMs),
    startedAtMs: optionalBigInt(checked.startedAtMs),
    completedAtMs: optionalBigInt(checked.completedAtMs),
    nextAttemptAtMs: optionalBigInt(checked.nextAttemptAtMs),
    exitCode: checked.exitCode ?? undefined,
  });
}

export function workerUpdateOperationFromProto(
  operation: WorkerUpdateOperationProto,
): WorkerUpdateOperation {
  return WorkerUpdateOperationSchema.parse({
    jobId: operation.jobId,
    workerFp: operation.workerFp,
    host: operation.host,
    revision: safeNumber(operation.revision, "revision"),
    targetGitSha: operation.targetGitSha,
    source: requiredEnum(sourceFromProto, operation.source, "source"),
    status: requiredEnum(statusFromProto, operation.status, "status"),
    phase: requiredEnum(phaseFromProto, operation.phase, "phase"),
    reasonCode: operation.reasonCode ?? null,
    message: operation.message ?? null,
    createdAtMs: safeNumber(operation.createdAtMs, "createdAtMs"),
    updatedAtMs: safeNumber(operation.updatedAtMs, "updatedAtMs"),
    startedAtMs: optionalNumber(operation.startedAtMs, "startedAtMs"),
    completedAtMs: optionalNumber(operation.completedAtMs, "completedAtMs"),
    nextAttemptAtMs: optionalNumber(operation.nextAttemptAtMs, "nextAttemptAtMs"),
    exitCode: operation.exitCode ?? null,
  });
}

export function workerUpdateReportToProto(report: WorkerUpdateReport): WorkerUpdateReportProto {
  const checked = WorkerUpdateReportSchema.parse(report);
  return create(WorkerUpdateReportProtoSchema, {
    schemaVersion: checked.schemaVersion,
    operation: workerUpdateOperationToProto(checked.operation),
    observedGitSha: checked.observedGitSha ?? undefined,
    coordinatorOrigin: checked.coordinatorOrigin,
    failure: checked.failure ? failureToProto(checked.failure) : undefined,
    events: checked.events.map(event => create(WorkerUpdateProgressEventProtoSchema, {
      atMs: BigInt(event.atMs),
      phase: phaseToProto[event.phase],
      message: event.message,
    })),
  });
}

export function workerUpdateReportFromProto(report: WorkerUpdateReportProto): WorkerUpdateReport {
  if (!report.operation) throw new Error("worker update report has no operation");
  return WorkerUpdateReportSchema.parse({
    schemaVersion: report.schemaVersion,
    operation: workerUpdateOperationFromProto(report.operation),
    observedGitSha: report.observedGitSha ?? null,
    coordinatorOrigin: report.coordinatorOrigin,
    failure: report.failure ? failureFromProto(report.failure) : null,
    events: report.events.map(event => ({
      atMs: safeNumber(event.atMs, "events.atMs"),
      phase: requiredEnum(phaseFromProto, event.phase, "events.phase"),
      message: event.message,
    })),
  });
}

function failureToProto(failure: WorkerUpdateFailure): WorkerUpdateFailureProto {
  return create(WorkerUpdateFailureProtoSchema, {
    code: failure.code,
    phase: phaseToProto[failure.phase],
    message: failure.message,
    journal: failure.journal ? create(WorkerUpdateJournalEvidenceProtoSchema, {
      path: failure.journal.path,
      phase: failure.journal.phase ?? undefined,
      ownerId: failure.journal.ownerId ?? undefined,
      rolloutId: failure.journal.rolloutId ?? undefined,
      priorSha: failure.journal.priorSha ?? undefined,
      targetSha: failure.journal.targetSha ?? undefined,
    }) : undefined,
    expectedKeeper: failure.expectedKeeper
      ? keeperRuntimeObservationToProto(failure.expectedKeeper)
      : undefined,
    observedKeeper: failure.observedKeeper
      ? keeperRuntimeObservationToProto(failure.observedKeeper)
      : undefined,
    targetContract: failure.targetContract
      ? keeperContractToProto(failure.targetContract)
      : undefined,
  });
}

function failureFromProto(failure: WorkerUpdateFailureProto): WorkerUpdateFailure {
  return {
    code: failure.code,
    phase: requiredEnum(phaseFromProto, failure.phase, "failure.phase"),
    message: failure.message,
    journal: failure.journal ? {
      path: failure.journal.path,
      phase: failure.journal.phase ?? null,
      ownerId: failure.journal.ownerId ?? null,
      rolloutId: failure.journal.rolloutId ?? null,
      priorSha: failure.journal.priorSha ?? null,
      targetSha: failure.journal.targetSha ?? null,
    } : null,
    expectedKeeper: failure.expectedKeeper
      ? keeperRuntimeObservationFromProto(failure.expectedKeeper)
      : null,
    observedKeeper: failure.observedKeeper
      ? keeperRuntimeObservationFromProto(failure.observedKeeper)
      : null,
    targetContract: failure.targetContract
      ? keeperContractFromProto(failure.targetContract)
      : null,
  };
}

function requiredEnum<T>(values: ReadonlyMap<number, T>, value: number, field: string): T {
  const mapped = values.get(value);
  if (mapped === undefined) throw new Error(`worker update ${field} is unspecified or unknown`);
  return mapped;
}

function optionalBigInt(value: number | null): bigint | undefined {
  return value === null ? undefined : BigInt(value);
}

function optionalNumber(value: bigint | undefined, field: string): number | null {
  return value === undefined ? null : safeNumber(value, field);
}

function safeNumber(value: bigint, field: string): number {
  if (value > BigInt(Number.MAX_SAFE_INTEGER)) {
    throw new Error(`worker update ${field} exceeds a safe integer`);
  }
  return Number(value);
}

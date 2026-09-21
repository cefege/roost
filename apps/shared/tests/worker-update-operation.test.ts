// Pins strict bounded worker-update operations and reports. These records cross
// coordinator restarts and browser tabs, so malformed or unbounded evidence
// must fail at the shared boundary before it can replace a newer summary.

import { describe, expect, test } from "bun:test";
import {
  WORKER_UPDATE_MAX_EVENTS,
  type WorkerUpdateOperation,
  WORKER_UPDATE_MAX_TEXT_LENGTH,
  WorkerUpdateOperationSchema,
  WorkerUpdateReportSchema,
} from "../src/worker-update-operation.ts";

const operation = {
  jobId: "aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa",
  workerFp: "b".repeat(64) as WorkerUpdateOperation["workerFp"],
  host: "worker.example.test",
  revision: 1,
  targetGitSha: "c".repeat(40),
  source: "manual",
  status: "queued",
  phase: "preflight",
  reasonCode: null,
  message: null,
  createdAtMs: 1,
  updatedAtMs: 1,
  startedAtMs: null,
  completedAtMs: null,
  nextAttemptAtMs: null,
  exitCode: null,
} as const;

describe("worker update operation contract", () => {
  test("accepts one strict authoritative operation", () => {
    expect(WorkerUpdateOperationSchema.parse(operation)).toEqual(operation);
    expect(WorkerUpdateOperationSchema.safeParse({ ...operation, extra: true }).success).toBe(false);
  });

  test("rejects unsafe identity, chronology, and multiline text", () => {
    expect(WorkerUpdateOperationSchema.safeParse({
      ...operation,
      workerFp: "B".repeat(64),
    }).success).toBe(false);
    expect(WorkerUpdateOperationSchema.safeParse({
      ...operation,
      updatedAtMs: 0,
    }).success).toBe(false);
    expect(WorkerUpdateOperationSchema.safeParse({
      ...operation,
      message: "line one\nline two",
    }).success).toBe(false);
  });

  test("bounds persistent report events and messages", () => {
    const report = {
      schemaVersion: 1,
      operation,
      observedGitSha: null,
      coordinatorOrigin: "https://coord.example.test",
      failure: null,
      events: Array.from({ length: WORKER_UPDATE_MAX_EVENTS }, (_, index) => ({
        atMs: index + 1,
        phase: "preflight",
        message: "queued",
      })),
    } as const;
    expect(WorkerUpdateReportSchema.safeParse(report).success).toBe(true);
    expect(WorkerUpdateReportSchema.safeParse({
      ...report,
      events: [...report.events, report.events[0]],
    }).success).toBe(false);
    expect(WorkerUpdateReportSchema.safeParse({
      ...report,
      events: [{ atMs: 1, phase: "preflight", message: "x".repeat(WORKER_UPDATE_MAX_TEXT_LENGTH + 1) }],
    }).success).toBe(false);
    expect(WorkerUpdateReportSchema.safeParse({
      ...report,
      coordinatorOrigin: "https://user:secret@coord.example.test",
    }).success).toBe(false);
    expect(WorkerUpdateReportSchema.safeParse({
      ...report,
      coordinatorOrigin: "https://coord.example.test/?token=secret",
    }).success).toBe(false);
  });
});

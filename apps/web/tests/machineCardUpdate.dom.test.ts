// Machine update UI state comes solely from Worker.update_operation plus the
// current worker projection. Browser request guards never become deployment
// state, and durable report text is one structured value for Details and copy.

import { beforeEach, describe, expect, mock, test } from "bun:test";
import type * as SolidApi from "solid-js";
import type { Worker } from "@roost/shared/wire";
import type {
  WorkerUpdateOperation,
  WorkerUpdateReport,
} from "@roost/shared/worker-update-operation";
import { workerUpdateReportToProto } from "@roost/shared/worker-update-operation-proto";
import { WorkerUpdateSource } from "@roost/shared/proto/wire_pb";

const COORD_SHA = "a".repeat(40);
const WORKER_SHA = "b".repeat(40);
const STALE_SHA = "c".repeat(40);
const FP = "d".repeat(64);
const JOB_ID = "00000000-0000-4000-8000-000000000001";

// The client renderer is selected by resolved test runtime URL.
const solidClientUrl = new URL("./solid.js", import.meta.resolve("solid-js"));
const Solid = await import(solidClientUrl.href) as typeof SolidApi;
mock.module("solid-js", () => Solid);

const ReactShim = { Fragment: Symbol("Fragment"), createElement: () => null };
(globalThis as typeof globalThis & { React: unknown }).React = ReactShim;
mock.module("react/jsx-dev-runtime", () => ({
  Fragment: ReactShim.Fragment,
  jsxDEV: () => null,
}));
mock.module("../src/components/Settings/md/primitives.tsx", () => ({ Button: () => null }));
mock.module("../src/store/sync-bootstrap.ts", () => ({
  refreshCoordAndWorkers: async () => true,
}));

type DeployStartRequest = {
  host: string;
  expectedGitSha?: string;
  source?: WorkerUpdateSource;
};
type DeployStartResponse = { ok: boolean; jobId: string; error: string };

const startRequests: DeployStartRequest[] = [];
let startResponse: (request: DeployStartRequest) => Promise<DeployStartResponse> = async () => ({
  ok: true,
  jobId: JOB_ID,
  error: "",
});
let outputFrames: Array<Record<string, unknown>> = [];
mock.module("../src/connect.ts", () => ({
  coordClient: {
    workersDeployStart: (request: DeployStartRequest) => {
      startRequests.push(request);
      return startResponse(request);
    },
    workersDeployOutput: () => (async function* () {
      for (const frame of outputFrames) yield frame;
    })(),
  },
}));

// These modules bind mocked RPC and UI dependencies during evaluation.
const {
  _resetMachineUpdateStarts,
  machineUpdateStartPending,
  machineUpdateReportRefreshRevision,
  noteMachineUpdateStatusRefreshed,
  readMachineUpdateReport,
  startMachineUpdateDeploy,
} = await import("../src/components/Settings/machine-update-deploy.ts");
const {
  deriveMachineUpdatePresentation,
  formatMachineUpdateReport,
} = await import("../src/components/Settings/MachineUpdateDetails.tsx");

function makeOperation(
  status: WorkerUpdateOperation["status"],
  targetGitSha = COORD_SHA,
  overrides: Partial<WorkerUpdateOperation> = {},
): WorkerUpdateOperation {
  return {
    jobId: JOB_ID,
    workerFp: FP as WorkerUpdateOperation["workerFp"],
    host: "workshop.example",
    revision: 1,
    targetGitSha,
    source: "manual",
    status,
    phase: status === "verifying" ? "confirmation" : "activation",
    reasonCode: null,
    message: null,
    createdAtMs: 10,
    updatedAtMs: 10,
    startedAtMs: 10,
    completedAtMs: status === "queued" || status === "running" || status === "verifying"
      ? null
      : 11,
    nextAttemptAtMs: null,
    exitCode: null,
    ...overrides,
  };
}

function makeWorker(): Worker {
  return {
    fp: FP as Worker["fp"],
    label: "Workshop",
    os: "linux",
    git_sha: WORKER_SHA,
    host_identity: null,
    host_metrics: null,
    registered_at_ms: 1,
    last_seen_ms: 2,
    reachable_addr: "workshop.example",
    keeper_runtime: null,
    terminal_core_capacity: null,
    update_operation: null,
  };
}

function makeReport(operation: WorkerUpdateOperation): WorkerUpdateReport {
  return {
    schemaVersion: 1,
    operation,
    observedGitSha: WORKER_SHA,
    coordinatorOrigin: "https://coord.example",
    failure: {
      code: "keeper_incompatible",
      phase: "preflight",
      message: "Keeper ABI differs from the target.",
      journal: {
        path: "/var/lib/roost/deploy-jobs/report.json",
        phase: "activation",
        ownerId: "job-owner",
        rolloutId: null,
        priorSha: WORKER_SHA,
        targetSha: COORD_SHA,
      },
      expectedKeeper: null,
      observedKeeper: null,
      targetContract: null,
    },
    events: [{
      atMs: 10,
      phase: "preflight",
      message: "Checking worker update admission.",
    }],
  };
}

beforeEach(() => {
  _resetMachineUpdateStarts();
  startRequests.length = 0;
  outputFrames = [];
  startResponse = async () => ({ ok: true, jobId: JOB_ID, error: "" });
});

describe("MachineUpdateDetails projection", () => {
  test("unsettled operations override matching worker versions in every tab", () => {
    const operation = makeOperation("running");
    const firstTab = deriveMachineUpdatePresentation({
      workerGitSha: COORD_SHA,
      coordinatorGitSha: COORD_SHA,
      online: true,
      operation,
    });
    const reloadedTab = deriveMachineUpdatePresentation({
      workerGitSha: COORD_SHA,
      coordinatorGitSha: COORD_SHA,
      online: true,
      operation,
    });

    expect(firstTab).toEqual(reloadedTab);
    expect(firstTab.label).toContain("Updating");
    expect(firstTab.action).toBe("update");
    expect(firstTab.actionDisabled).toBe(true);
  });

  test("maps offline, blocked, failed, and stale operations to their safe actions", () => {
    const offline = deriveMachineUpdatePresentation({
      workerGitSha: WORKER_SHA,
      coordinatorGitSha: COORD_SHA,
      online: false,
      operation: null,
    });
    const blocked = deriveMachineUpdatePresentation({
      workerGitSha: WORKER_SHA,
      coordinatorGitSha: COORD_SHA,
      online: true,
      operation: makeOperation("blocked", COORD_SHA, { reasonCode: "keeper_incompatible" }),
    });
    const failed = deriveMachineUpdatePresentation({
      workerGitSha: WORKER_SHA,
      coordinatorGitSha: COORD_SHA,
      online: true,
      operation: makeOperation("failed"),
    });
    const staleFailure = deriveMachineUpdatePresentation({
      workerGitSha: WORKER_SHA,
      coordinatorGitSha: COORD_SHA,
      online: true,
      operation: makeOperation("failed", STALE_SHA),
    });

    expect(offline).toMatchObject({
      label: "Update pending — offline",
      detail: "Updates automatically when this machine returns.",
      action: null,
    });
    expect(blocked).toMatchObject({
      label: "Update blocked",
      action: "retry",
      detail: expect.stringContaining("incompatible"),
    });
    expect(failed).toMatchObject({ label: "Update failed", action: "retry" });
    expect(staleFailure).toMatchObject({ label: "Update available", action: "update" });
  });
});

describe("machine update request and report ownership", () => {
  test("the browser guards only its unresolved start request and leaves coalescing to the coordinator", async () => {
    const response = Promise.withResolvers<DeployStartResponse>();
    startResponse = async () => response.promise;

    const first = startMachineUpdateDeploy(FP, COORD_SHA);
    const duplicate = startMachineUpdateDeploy(FP, COORD_SHA);

    expect(startRequests).toEqual([{
      host: FP,
      expectedGitSha: COORD_SHA,
      source: WorkerUpdateSource.MANUAL,
    }]);
    expect(machineUpdateStartPending(FP)).toBe(true);

    response.resolve({ ok: true, jobId: JOB_ID, error: "" });
    await expect(first).resolves.toBeNull();
    await expect(duplicate).resolves.toBeNull();
    expect(machineUpdateStartPending(FP)).toBe(false);
  });

  test("status refresh invalidates an unavailable report read without changing job state", () => {
    const before = machineUpdateReportRefreshRevision();
    noteMachineUpdateStatusRefreshed();
    expect(machineUpdateReportRefreshRevision()).toBe(before + 1);
  });

  test("reopens a durable report without treating a missing report as a deploy failure", async () => {
    const operation = makeOperation("failed");
    const report = makeReport(operation);
    outputFrames = [
      { kind: "operation", operation: workerUpdateReportToProto(report).operation },
      { kind: "report", report: workerUpdateReportToProto(report) },
    ];

    await expect(readMachineUpdateReport(JOB_ID)).resolves.toEqual({
      kind: "available",
      report,
    });

    outputFrames = [{ kind: "done", exit: 8, error: "not a report" }];
    await expect(readMachineUpdateReport(JOB_ID)).resolves.toEqual({ kind: "unavailable" });
  });

  test("formats one structured report for the selectable Details region and copy action", () => {
    const report = makeReport(makeOperation("failed"));
    const payload = JSON.parse(formatMachineUpdateReport(makeWorker(), report));

    expect(payload).toMatchObject({
      schemaVersion: 1,
      machine: {
        label: "Workshop",
        fingerprint: FP,
        host: "workshop.example",
        reachableAddress: "workshop.example",
      },
      coordinatorOrigin: "https://coord.example",
      operation: {
        jobId: JOB_ID,
        source: "manual",
        targetGitSha: COORD_SHA,
        status: "failed",
      },
      observedGitSha: WORKER_SHA,
      failure: {
        code: "keeper_incompatible",
        journal: { path: "/var/lib/roost/deploy-jobs/report.json" },
      },
    });
  });
});

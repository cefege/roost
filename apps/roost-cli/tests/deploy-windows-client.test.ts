import { describe, expect, test } from "bun:test";
import { deployWindowsWorkerViaCoordinator } from "../src/deploy.ts";
import { DeployFailure } from "../src/deploy-exec.ts";

type DeployClient = Parameters<typeof deployWindowsWorkerViaCoordinator>[0];
type DeployStartRequest = Parameters<DeployClient["workersDeployStart"]>[0];

function clientFor(options: {
  os?: "darwin" | "linux" | "win32";
  start?: { ok: boolean; jobId: string; error: string };
  frames?: Array<{ kind: string; text?: string; exit?: number; error?: string }>;
  streams?: Array<Array<{ kind: string; text?: string; exit?: number; error?: string }>>;
  calls?: string[];
  requests?: DeployStartRequest[];
  workers?: Array<{
    fp: string;
    label: string;
    os: "darwin" | "linux" | "win32";
    reachableAddr?: string;
  }>;
  inventoryError?: Error;
} = {}): DeployClient {
  const calls = options.calls ?? [];
  let streamIndex = 0;
  return {
    workersList: async () => {
      if (options.inventoryError) throw options.inventoryError;
      return {
        workers: options.workers ?? [{
          $typeName: "roost.v1.Worker",
          fp: "a".repeat(64),
          label: "Build PC",
          os: options.os ?? "win32",
          reachableAddr: "build-pc.tail.example",
        }],
        routableFps: ["a".repeat(64)],
      };
    },
    workersDeployStart: async (request: DeployStartRequest) => {
      calls.push(`start:${request.host}`);
      options.requests?.push(request);
      return { $typeName: "roost.v1.WorkersDeployStartResponse", ...(options.start ?? { ok: true, jobId: "job-1", error: "" }) };
    },
    workersDeployOutput: ({ jobId }: { jobId: string }) => (async function* () {
      calls.push(`stream:${jobId}`);
      const frames = options.streams?.[streamIndex++] ?? options.frames ?? [
        { kind: "line", text: "service-configs-switched" },
        { kind: "done", exit: 0 },
      ];
      for (const frame of frames) {
        yield { $typeName: "roost.v1.WorkersDeployOutputFrame", text: "", exit: 0, error: "", ...frame };
      }
    })(),
  } as unknown as DeployClient;
}

describe("Windows deploy CLI channel", () => {
  test("detects a registered Windows host and streams its coordinator update to completion", async () => {
    const calls: string[] = [];
    const lines: string[] = [];

    expect(await deployWindowsWorkerViaCoordinator(
      clientFor({ calls }),
      "BUILD-PC.TAIL.EXAMPLE.",
      (line) => lines.push(line),
    )).toBe(true);

    expect(calls).toEqual([`start:${"a".repeat(64)}`, "stream:job-1"]);
    expect(lines).toContain("service-configs-switched");
    expect(lines.at(-1)).toBe("✓ signed Windows update complete for Build PC");
  });

  test("pins the fleet build and preflight manifest digest in START admission", async () => {
    const requests: DeployStartRequest[] = [];
    const expectedGitSha = "b".repeat(40);
    const expectedManifestSha256 = "c".repeat(64);
    expect(await deployWindowsWorkerViaCoordinator(
      clientFor({ requests }),
      "Build PC",
      () => {},
      { expectedGitSha, expectedManifestSha256 },
    )).toBeTrue();
    expect(requests).toHaveLength(1);
    expect(requests[0]).toMatchObject({
      host: "a".repeat(64),
      expectedGitSha,
      expectedManifestSha256,
    });
  });

  test("reconnects by durable job id when the coordinator restarts mid-update", async () => {
    const calls: string[] = [];
    const lines: string[] = [];
    const streams = [
      [{ kind: "line", text: "prepared" }, { kind: "done", exit: -1, error: "unknown jobId" }],
      [
        { kind: "line", text: "prepared" },
        { kind: "line", text: "resumed signed Windows update from durable worker journal" },
        { kind: "done", exit: 0 },
      ],
    ];

    expect(await deployWindowsWorkerViaCoordinator(
      clientFor({ calls, streams }),
      "Build PC",
      (line) => lines.push(line),
      { sleep: async () => undefined },
    )).toBe(true);

    expect(calls).toEqual([`start:${"a".repeat(64)}`, "stream:job-1", "stream:job-1"]);
    expect(lines.filter((line) => line === "prepared")).toHaveLength(1);
    expect(lines).toContain("resumed signed Windows update from durable worker journal");
    expect(lines.at(-1)).toBe("✓ signed Windows update complete for Build PC");
  });

  test("leaves non-Windows targets to the existing POSIX deploy path", async () => {
    const calls: string[] = [];
    expect(await deployWindowsWorkerViaCoordinator(
      clientFor({ os: "linux", calls }),
      "Build PC",
    )).toBe(false);
    expect(calls).toEqual([]);
  });

  test("falls back to direct SSH when coordinator inventory is unavailable before a match", async () => {
    const calls: string[] = [];
    expect(await deployWindowsWorkerViaCoordinator(
      clientFor({ calls, inventoryError: new Error("coordinator unavailable") }),
      "linux-worker.tail.example",
    )).toBe(false);
    expect(calls).toEqual([]);
  });

  test("rejects an empty normalized target before matching missing worker identities", async () => {
    const calls: string[] = [];
    await expect(deployWindowsWorkerViaCoordinator(
      clientFor({ calls }),
      " . ",
    )).rejects.toEqual(new DeployFailure(2, "deploy target must not be empty"));
    expect(calls).toEqual([]);
  });

  test("rejects duplicate labels and reachable names instead of choosing an unordered match", async () => {
    const calls: string[] = [];
    const workers = [
      {
        fp: "a".repeat(64),
        label: "Shared worker",
        os: "win32" as const,
        reachableAddr: "shared.tail.example",
      },
      {
        fp: "b".repeat(64),
        label: "Shared worker",
        os: "win32" as const,
        reachableAddr: "shared.tail.example",
      },
    ];

    await expect(deployWindowsWorkerViaCoordinator(
      clientFor({ calls, workers }),
      "Shared worker",
    )).rejects.toEqual(new DeployFailure(
      2,
      'ambiguous deploy target "Shared worker" matches multiple registered workers; use the worker fingerprint',
    ));
    await expect(deployWindowsWorkerViaCoordinator(
      clientFor({ calls, workers }),
      "shared.tail.example",
    )).rejects.toEqual(new DeployFailure(
      2,
      'ambiguous deploy target "shared.tail.example" matches multiple registered workers; use the worker fingerprint',
    ));
    expect(calls).toEqual([]);
  });

  test("fails closed when the update cannot start or its stream lacks success", async () => {
    await expect(deployWindowsWorkerViaCoordinator(clientFor({
      start: { ok: false, jobId: "", error: "publisher pin missing" },
    }), "Build PC", () => undefined)).rejects.toEqual(new DeployFailure(2, "publisher pin missing"));

    await expect(deployWindowsWorkerViaCoordinator(clientFor({
      frames: [{ kind: "done", exit: 7, error: "health proof failed" }],
    }), "Build PC", () => undefined)).rejects.toEqual(new DeployFailure(2, "health proof failed"));

    await expect(deployWindowsWorkerViaCoordinator(
      clientFor({ frames: [] }),
      "Build PC",
      () => undefined,
      { deadlineMs: 0 },
    )).rejects.toEqual(new DeployFailure(2, "signed Windows update stream ended without a terminal result"));
  });
});

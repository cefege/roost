// Local worker verification tests pin rollback after failed activation and
// exact service-definition release identity. The shared deployment suite keeps
// remote lock and platform probes separate from this localhost contract.

import { describe, expect, test } from "bun:test";
import { _activateLocalWorker } from "../src/deploy-local-activation.ts";
import { localWorkerReleaseMatches } from "../src/local-worker-deploy-journal.ts";

describe("local worker deployment verification", () => {
  test("rolls back and removes its stage when release proof fails", async () => {
    let rollbacks = 0;
    let cleanups = 0;
    const running = {
      exit: 0,
      stdout: "MainPID=42\nActiveState=active\nSubState=running\n",
      stderr: "",
    };
    await expect(_activateLocalWorker({
      install: async () => ({ exit: 0, stdout: "installed", stderr: "" }),
      restart: async () => ({ exit: 0, stdout: "", stderr: "" }),
      verify: async () => running,
      rollback: async () => {
        rollbacks += 1;
        return null;
      },
      cleanupStage: async () => {
        cleanups += 1;
      },
    })).rejects.toThrow("prior worker service restored");
    expect(rollbacks).toBe(1);
    expect(cleanups).toBe(1);
  });

  test("binds local release proof to worktree and git identity", () => {
    const definition = [
      "[Service]",
      'WorkingDirectory="/srv/releases/worker/sha-1"',
      'Environment="GIT_SHA=sha-1"',
    ].join("\n");
    expect(localWorkerReleaseMatches(
      definition,
      "linux",
      "/srv/releases/worker/sha-1",
      "sha-1",
    )).toBe(true);
    expect(localWorkerReleaseMatches(
      definition,
      "linux",
      "/srv/releases/worker/sha-2",
      "sha-1",
    )).toBe(false);
  });
});

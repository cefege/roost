// Local worker verification tests pin rollback after failed activation and
// exact service-definition release identity. The shared deployment suite keeps
// remote lock and platform probes separate from this localhost contract.

import { describe, expect, test } from "bun:test";
import type { JournaledKeeperUpdateV1 } from "@roost/shared/keeper-update";
import { _activateLocalWorker } from "../src/deploy-local-activation.ts";
import { localWorkerReleaseMatches } from "../src/local-worker-deploy-journal.ts";
import {
  LOCAL_KEEPER_UPDATE,
  WORKER_FINGERPRINT,
} from "./deploy-local-journal-fixture.ts";

const PRESERVE_KEEPER_UPDATE = {
  admission: {
    ...LOCAL_KEEPER_UPDATE.admission,
    classification: "worker-only-safe",
    target_contract_digest:
      LOCAL_KEEPER_UPDATE.admission.source_contract_digest,
    expected_binding_digest: "e".repeat(64),
    required_action: "preserve",
  },
  source_contract: LOCAL_KEEPER_UPDATE.source_contract,
  target_contract: {
    ...LOCAL_KEEPER_UPDATE.source_contract,
    build_sha: LOCAL_KEEPER_UPDATE.target_contract.build_sha,
  },
} as const satisfies JournaledKeeperUpdateV1;

describe("local worker deployment verification", () => {
  test("rolls back and removes its stage when release proof fails", async () => {
    const events: string[] = [];
    const running = {
      exit: 0,
      stdout: "MainPID=42\nActiveState=active\nSubState=running\n",
      stderr: "",
    };
    await expect(_activateLocalWorker({
      stop: async () => {
        events.push("stop");
        return { exit: 0, stdout: "", stderr: "" };
      },
      install: async () => {
        events.push("install");
        return { exit: 0, stdout: "installed", stderr: "" };
      },
      keeperUpdate: LOCAL_KEEPER_UPDATE,
      workerFingerprint: WORKER_FINGERPRINT,
      applyKeeperUpdate: async (_workerFingerprint, update, direction) => { expect(update).toEqual(LOCAL_KEEPER_UPDATE);
      events.push(`keeper:${direction}`); },
      restart: async () => {
        events.push("restart");
        return { exit: 0, stdout: "", stderr: "" };
      },
      verify: async () => {
        events.push("verify");
        return running;
      },
      rollback: async () => {
        events.push("rollback");
        return null;
      },
      cleanupStage: async () => {
        events.push("cleanup");
      },
    })).rejects.toThrow("prior worker service restored");
    expect(events).toEqual([
      "keeper:target",
      "stop",
      "install",
      "restart",
      "verify",
      "rollback",
      "cleanup",
    ]);
  });

  test("preserve executes before stop and never takes a replacement-only branch", async () => {
    const events: string[] = [];
    await expect(_activateLocalWorker({
      stop: async () => {
        events.push("stop");
        return { exit: 0, stdout: "", stderr: "" };
      },
      install: async () => {
        events.push("install");
        return { exit: 0, stdout: "", stderr: "" };
      },
      keeperUpdate: PRESERVE_KEEPER_UPDATE,
      workerFingerprint: WORKER_FINGERPRINT,
      applyKeeperUpdate: async (_workerFingerprint, update, direction) => {
        expect(update).toEqual(PRESERVE_KEEPER_UPDATE);
        events.push(`keeper:${direction}:${update.admission.required_action}`);
      },
      restart: async () => {
        events.push("restart");
        return { exit: 0, stdout: "", stderr: "" };
      },
      verify: async () => {
        events.push("verify");
        return {
          exit: 0,
          stdout: "MainPID=43\nActiveState=active\nSubState=running\nRoostReleaseMatch=yes\n",
          stderr: "",
        };
      },
      rollback: async () => {
        events.push("unexpected-rollback");
        return null;
      },
      cleanupStage: async () => {
        events.push("unexpected-cleanup");
      },
    })).resolves.toBeDefined();
    expect(events).toEqual([
      "keeper:target:preserve",
      "stop",
      "install",
      "restart",
      "verify",
    ]);
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

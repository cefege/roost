// Builds canonical Linux worker deployment journals and recovery remotes.
// Recovery tests use these fixtures to observe operation ordering without
// touching a host worker service or keeper.
import { expect } from "bun:test";
import {
  KEEPER_EMPTY_BINDING_DIGEST,
  type JournaledKeeperUpdateV1,
  type KeeperContractV1,
} from "@roost/shared/keeper-update";
import {
  serializeLinuxKeeperUpdate,
  type LinuxDeployJournal,
} from "../src/linux-deploy-journal.ts";
import type { LinuxDeployRecoveryRemote } from "../src/deploy-linux-recovery.ts";

export const HOME = "/home/worker";
export const SHA = "a".repeat(40);
export const PRIOR_SHA = "c".repeat(40);
export const ROLLOUT_ID = "11111111-1111-4111-8111-111111111111";
export const WORKER_FINGERPRINT = "4".repeat(64);
export const SOURCE_KEEPER_DIGEST = "1".repeat(64);
const TARGET_KEEPER_DIGEST = "2".repeat(64);
export const KEEPER_UPDATE: JournaledKeeperUpdateV1 = {
  admission: {
    classification: "keeper-restart-required",
    source_contract_digest: SOURCE_KEEPER_DIGEST,
    target_contract_digest: TARGET_KEEPER_DIGEST,
    expected_keeper_pid: 700,
    expected_keeper_epoch: "22222222-2222-4222-8222-222222222222",
    expected_binding_digest: KEEPER_EMPTY_BINDING_DIGEST,
    required_action: "replace-empty",
  },
  source_contract: keeperContract(SOURCE_KEEPER_DIGEST, PRIOR_SHA),
  target_contract: keeperContract(TARGET_KEEPER_DIGEST, SHA),
};
export const TARGET = `${HOME}/.local/share/roost/releases/worker/${SHA}-11111111-1111-4111-8111-111111111111`;
export const PRIOR_UNIT = [
  "[Service]",
  `WorkingDirectory=${HOME}/.local/share/roost/releases/worker/prior`,
  `Environment="GIT_SHA=${PRIOR_SHA}"`,
  "",
].join("\n");

export function keeperContract(
  implementationDigest: string,
  buildSha: string,
): KeeperContractV1 {
  return {
    protocol_version: 1,
    supported_features: ["history-v1"],
    required_features: [],
    implementation_digest: implementationDigest,
    bun_abi: "bun-linux-x64-v1",
    platform: "linux",
    arch: "x64",
    build_sha: buildSha,
  };
}

export function encode(value: string): string {
  return Buffer.from(value, "utf8").toString("base64");
}

export function journalSnapshot(options: {
  phase: string;
  target?: string;
  sha?: string;
  priorUnit?: string | null;
  lifecycle?: string;
  priorPid?: number;
  schema?: "3" | "4";
  rolloutId?: string | null;
  workerFingerprint?: string | null;
  keeperUpdate?: JournaledKeeperUpdateV1 | null;
}): string {
  const priorUnit = options.priorUnit === undefined ? PRIOR_UNIT : options.priorUnit;
  const lifecycle = options.lifecycle ?? "stopped";
  const keeperUpdate = options.keeperUpdate === undefined
    ? priorUnit === null ? null : KEEPER_UPDATE
    : options.keeperUpdate;
  const workerFingerprint = options.workerFingerprint === undefined
    ? keeperUpdate ? WORKER_FINGERPRINT : null
    : options.workerFingerprint;
  const fields: Record<string, string> = {
    schema: options.schema ?? "4",
    phase: options.phase,
    "target-sha": options.sha ?? SHA,
    "target-release": options.target ?? TARGET,
    "prior-unit-state": priorUnit === null ? "absent" : "present",
    "prior-unit-mode": priorUnit === null ? "" : "644",
    "prior-lifecycle": lifecycle,
    "prior-enablement": priorUnit === null ? "absent" : "enabled",
    "prior-pid": String(options.priorPid ?? (lifecycle === "running" ? 42 : 0)),
    "prior-unit": priorUnit ?? "",
  };
  if ((options.schema ?? "4") === "4") {
    fields["rollout-id"] = options.rolloutId ?? "";
    fields["worker-fingerprint"] = workerFingerprint ?? "";
    fields["keeper-update"] = serializeLinuxKeeperUpdate(keeperUpdate);
  } else {
    fields["rollout-id"] = options.rolloutId ?? "";
  }
  return [
    "journal",
    ...Object.entries(fields).map(([name, value]) => `${name}=${encode(value)}`),
    "",
  ].join("\n");
}

export function fakeRemote(
  journal: LinuxDeployJournal,
  targetHealthy: boolean,
): { calls: string[]; remote: LinuxDeployRecoveryRemote } {
  const calls: string[] = [];
  return {
    calls,
    remote: {
      home: HOME,
      loadJournal: async () => {
        calls.push("load");
        return journal;
      },
      proveTarget: async () => {
        calls.push("prove-target");
        return {
          healthy: targetHealthy,
          proof: { exit: targetHealthy ? 0 : 1, stdout: "", stderr: "" },
        };
      },
      checkpointRollback: async () => { calls.push("checkpoint-rollback"); },
      checkpointCommit: async () => { calls.push("checkpoint-commit"); },
      stopWorker: async () => {
        calls.push("stop-worker");
      },
      startWorker: async () => {
        calls.push("start-worker");
      },
      applyKeeperUpdate: async (_workerFingerprint, keeperUpdate, direction, actionReleasePath) => {
        expect(actionReleasePath).toBe(journal.targetReleasePath);
        calls.push(`keeper-${keeperUpdate.admission.required_action}-${direction}`);
      },
      proveKeeperUpdate: async (
        _workerFingerprint,
        _keeperUpdate,
        direction,
        expectedWorkerSha,
      ) => {
        calls.push(`prove-keeper-${direction}-${expectedWorkerSha}`);
      },
      restorePrior: async (loaded) => {
        calls.push(
          `restore-${loaded.priorUnit === null ? "absent" : "present"}-${loaded.priorLifecycle}`,
        );
      },
      settlePrior: async () => { calls.push("settle-prior"); },
      provePriorWorker: async (_loaded, expectedSha) => {
        calls.push(`prove-prior-worker-${expectedSha}`);
      },
      provePrior: async () => {
        calls.push("prove-prior");
      },
      cleanupPrior: async () => {
        calls.push("cleanup-prior");
      },
      removeTarget: async (loaded) => {
        calls.push(`remove-${loaded.targetReleasePath}`);
      },
      clearJournal: async () => {
        calls.push("clear");
      },
      now: () => 100,
    },
  };
}

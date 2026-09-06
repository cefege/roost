// Builds canonical macOS worker journals and injected recovery remotes.
// Recovery tests use these fixtures to verify lifecycle and keeper ordering
// without touching launchd or a host worker installation.
import {
  type MacosDeployJournalV2,
  type MacosDeployRecoveryRemote,
  type MacosDeployTargetProof,
} from "../src/deploy-macos-journal.ts";
import {
  MACOS_KEEPER_UPDATE,
  MACOS_WORKER_FINGERPRINT,
} from "./deploy-macos-keeper-update-fixture.ts";

export const SHA = "a".repeat(40);
export const PRIOR_SHA = "c".repeat(40);
export const ROLLOUT_ID = "11111111-1111-4111-8111-111111111111";
export const RELEASE_ROOT = "/Users/worker/RoostWorkerV2-releases";
export const RELEASE_ID = `${SHA}-00000000-0000-4000-8000-000000000001`;
export const RELEASE_PATH = `${RELEASE_ROOT}/${RELEASE_ID}`;
const PRIOR_PLIST = Buffer.from(
  `<plist><dict><key>EnvironmentVariables</key><dict>` +
    `<key>GIT_SHA</key><string>${PRIOR_SHA}</string>` +
    `</dict></dict></plist>\n`,
).toString("base64");

export function journal(overrides: Partial<MacosDeployJournalV2> = {}): MacosDeployJournalV2 {
  return {
    schemaVersion: 2,
    phase: "activating",
    targetGitSha: SHA,
    targetReleasePath: RELEASE_PATH,
    rolloutId: null,
    workerFingerprint: MACOS_WORKER_FINGERPRINT,
    keeperUpdate: MACOS_KEEPER_UPDATE,
    priorPlistBase64: PRIOR_PLIST,
    priorPlistMode: 0o600,
    priorLifecycle: "unloaded",
    priorPid: null,
    priorDisabled: false,
    createdAt: "2026-08-16T00:00:00.000Z",
    updatedAt: "2026-08-16T00:00:01.000Z",
    ...overrides,
  };
}

export function fakeRemote(
  durable: MacosDeployJournalV2,
  targetProof: MacosDeployTargetProof = {
    definitionMatches: false,
    running: false,
    result: { exit: 1, stdout: "state = exited\n", stderr: "" },
  },
  failProof = false,
): { remote: MacosDeployRecoveryRemote; calls: string[] } {
  const calls: string[] = [];
  const remote: MacosDeployRecoveryRemote = {
    async load() {
      calls.push("load");
      return durable;
    },
    async proveTarget() {
      calls.push("prove-target");
      return targetProof;
    },
    async checkpointActivated(saved) {
      calls.push("checkpoint-activated");
      return { ...saved, phase: "activated" };
    },
    async checkpointRollback() {
      calls.push("checkpoint-rollback");
    },
    async checkpointCommit() {
      calls.push("checkpoint-commit");
    },
    async bootout() {
      calls.push("bootout");
    },
    async applyKeeperUpdate(_workerFingerprint, update, direction) {
      calls.push(`keeper:${direction}:${update.admission.required_action}`);
    },
    async proveKeeperUpdate(_workerFingerprint, _update, direction, expectedWorkerSha) {
      calls.push(`prove-keeper:${direction}:${expectedWorkerSha}`);
    },
    async restorePriorDefinition(saved) {
      calls.push(`restore-prior:${saved.priorPlistBase64 === null ? "absent" : "bytes"}`);
    },
    async setDisabled(_saved, disabled) {
      calls.push(`disabled:${disabled}`);
    },
    async bootstrap() {
      calls.push("bootstrap");
    },
    async kickstart() {
      calls.push("kickstart");
    },
    async stop() {
      calls.push("stop");
    },
    async provePrior() {
      calls.push("prove-prior");
      if (failProof) throw new Error("prior lifecycle mismatch");
    },
    async removeTarget() {
      calls.push("remove-target");
    },
    async cleanupPriorRelease() {
      calls.push("cleanup-prior");
    },
    async clear() {
      calls.push("clear");
    },
    now: () => 100,
  };
  return { remote, calls };
}
// Canonical service snapshots and keeper proof for local journal tests.
// Source and target differ so recovery must replay the persisted replace-empty
// action rather than infer a replacement from current runtime state.

import {
  KEEPER_EMPTY_BINDING_DIGEST,
  type JournaledKeeperUpdateV1,
  type KeeperContractV1,
} from "@roost/shared/keeper-update";
import type {
  LocalWorkerDeployConfinement,
  LocalWorkerDeployJournal,
  LocalWorkerServiceSnapshot,
} from "../src/local-worker-deploy-journal.ts";

export const TARGET_SHA = "a".repeat(40);
export const PRIOR_SHA = "b".repeat(40);
export const ROLLOUT_ID = "11111111-1111-4111-8111-111111111111";
export const WORKER_FINGERPRINT = "f".repeat(64);
export const SOURCE_ROOT = "/srv/roost/source";
export const RELEASE_ROOT = "/srv/roost/service/releases/worker";
export const STAGED_RELEASE =
  `${RELEASE_ROOT}/${TARGET_SHA}-11111111-1111-4111-8111-111111111111`;
export const PRIOR_RELEASE = `${RELEASE_ROOT}/${PRIOR_SHA}-prior`;
export const LOCAL_CONFINEMENT: LocalWorkerDeployConfinement = {
  os: "linux",
  sourceRoot: SOURCE_ROOT,
  releaseRoot: RELEASE_ROOT,
};

function serviceSnapshot(definition: string, mode = 0o600): LocalWorkerServiceSnapshot {
  return { definitionBase64: Buffer.from(definition).toString("base64"), mode };
}

export const PRIOR_SERVICE = serviceSnapshot([
  "[Service]",
  `WorkingDirectory=\"${PRIOR_RELEASE}\"`,
  `Environment=\"GIT_SHA=${PRIOR_SHA}\"`,
].join("\n"), 0o640);
export const TARGET_SERVICE = serviceSnapshot([
  "[Service]",
  `WorkingDirectory=\"${STAGED_RELEASE}\"`,
  `Environment=\"GIT_SHA=${TARGET_SHA}\"`,
].join("\n"), 0o640);

const SOURCE_DIGEST = "c".repeat(64);
const TARGET_DIGEST = "d".repeat(64);

const SOURCE_CONTRACT: KeeperContractV1 = {
  protocol_version: 1,
  supported_features: ["history-v1"],
  required_features: ["history-v1"],
  implementation_digest: SOURCE_DIGEST,
  bun_abi: "bun-1.2",
  platform: "linux",
  arch: "x64",
  build_sha: "b".repeat(40),
};

export const LOCAL_KEEPER_UPDATE: JournaledKeeperUpdateV1 = {
  admission: {
    classification: "keeper-restart-required",
    source_contract_digest: SOURCE_DIGEST,
    target_contract_digest: TARGET_DIGEST,
    expected_keeper_pid: 4242,
    expected_keeper_epoch: "22222222-2222-4222-8222-222222222222",
    expected_binding_digest: KEEPER_EMPTY_BINDING_DIGEST,
    required_action: "replace-empty",
  },
  source_contract: SOURCE_CONTRACT,
  target_contract: {
    ...SOURCE_CONTRACT,
    implementation_digest: TARGET_DIGEST,
    build_sha: "a".repeat(40),
  },
};

export function localWorkerJournal(
  overrides: Partial<LocalWorkerDeployJournal> = {},
): LocalWorkerDeployJournal {
  return {
    schemaVersion: 4,
    phase: "prepared",
    os: "linux",
    sourceRoot: SOURCE_ROOT,
    releaseRoot: RELEASE_ROOT,
    stagedReleasePath: STAGED_RELEASE,
    targetSha: TARGET_SHA,
    rolloutId: null,
    workerFingerprint: WORKER_FINGERPRINT,
    keeperUpdate: LOCAL_KEEPER_UPDATE,
    priorService: PRIOR_SERVICE,
    priorLifecycle: "running",
    priorStartupPolicy: "enabled",
    priorWorkingDirectory: PRIOR_RELEASE,
    priorGitSha: PRIOR_SHA,
    targetService: null,
    ...overrides,
  };
}

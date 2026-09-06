// Canonical keeper-update envelope for macOS journal parsing and replay tests.
// Source and target differ only by build provenance, so preserve is the required
// action; individual tests override fields to exercise fail-closed validation.

import type { JournaledKeeperUpdateV1 } from "@roost/shared/keeper-update";

const IMPLEMENTATION_DIGEST = "d".repeat(64);
export const MACOS_WORKER_FINGERPRINT = "f".repeat(64);
export const MACOS_SOURCE_SHA = "c".repeat(40);
const CONTRACT_BASE = {
  protocol_version: 1,
  supported_features: ["history-v1"],
  required_features: ["history-v1"],
  implementation_digest: IMPLEMENTATION_DIGEST,
  bun_abi: "1.2.20",
  platform: "darwin" as const,
  arch: "arm64",
};

export const MACOS_KEEPER_UPDATE: JournaledKeeperUpdateV1 = {
  admission: {
    classification: "worker-only-safe",
    source_contract_digest: IMPLEMENTATION_DIGEST,
    target_contract_digest: IMPLEMENTATION_DIGEST,
    expected_keeper_pid: 2718,
    expected_keeper_epoch: "22222222-2222-4222-8222-222222222222",
    expected_binding_digest: "e".repeat(64),
    required_action: "preserve",
  },
  source_contract: { ...CONTRACT_BASE, build_sha: MACOS_SOURCE_SHA },
  target_contract: { ...CONTRACT_BASE, build_sha: "a".repeat(40) },
};

// Pure keeper update admission contract tests.
// These cases pin the four fail-closed outcomes, build provenance exception,
// runtime consistency checks, and immutable journal envelope validation.

import { createHash } from "node:crypto";
import { describe, expect, test } from "bun:test";
import { create, fromBinary, toBinary } from "@bufbuild/protobuf";
import {
  CoordWorkerDownSchema,
  DKeeperUpdatePrepareSchema,
} from "../src/gen/roost/v1/worker_transport_pb.ts";
import {
  JournaledKeeperUpdateV1Schema,
  KEEPER_EMPTY_BINDING_DIGEST,
  KeeperCoordinatorOpenSessionIdsSchema,
  classifyKeeperUpdate,
  keeperBindingDigestInput,
  keeperUpdateAdmission,
  keeperUpdateOutcomeMatchesAction,
  type KeeperContractV1,
  type KeeperRuntimeObservationV1,
} from "../src/keeper-update.ts";

const SESSION_ID = "00000000-0000-4000-8000-000000000001";
const SOURCE_DIGEST = "1".repeat(64);
const TARGET_DIGEST = "2".repeat(64);

function contract(overrides: Partial<KeeperContractV1> = {}): KeeperContractV1 {
  return {
    protocol_version: 2,
    supported_features: ["keeper-contract-v1"],
    required_features: ["keeper-contract-v1"],
    implementation_digest: SOURCE_DIGEST,
    bun_abi: "1.2.3",
    platform: "linux",
    arch: "x64",
    build_sha: "a".repeat(40),
    ...overrides,
  };
}

function observation(
  runningContract: KeeperContractV1 = contract(),
  live = false,
): KeeperRuntimeObservationV1 {
  const digestInput = live
    ? keeperBindingDigestInput([{ channel_id: 7, pid: 99 }])
    : keeperBindingDigestInput([]);
  return {
    schema_version: 1,
    running_contract: runningContract,
    keeper_pid: 41,
    keeper_epoch: "00000000-0000-4000-8000-000000000002",
    channel_count: live ? 1 : 0,
    binding_digest: createHash("sha256").update(digestInput).digest("hex"),
    reconciled_at_ms: 100,
  };
}

describe("classifyKeeperUpdate", () => {
  test("ignores build provenance for worker-only preservation", () => {
    const target = contract({ build_sha: "b".repeat(40) });
    expect(classifyKeeperUpdate(target, observation(), new Set())).toBe(
      "worker-only-safe",
    );
  });

  test("requires empty replacement for a changed keeper artifact", () => {
    const target = contract({ implementation_digest: TARGET_DIGEST });
    expect(classifyKeeperUpdate(target, observation(), new Set())).toBe(
      "keeper-restart-required",
    );
  });

  test("blocks changed keeper artifacts while sessions are live", () => {
    const target = contract({ implementation_digest: TARGET_DIGEST });
    expect(classifyKeeperUpdate(
      target,
      observation(contract(), true),
      new Set([SESSION_ID]),
    )).toBe("incompatible-with-live-sessions");
  });

  test("treats missing and internally inconsistent proof as unproven", () => {
    expect(classifyKeeperUpdate(contract(), null, new Set())).toBe("unproven");
    expect(classifyKeeperUpdate(
      contract(),
      { ...observation(), channel_count: 1 },
      new Set([SESSION_ID]),
    )).toBe("unproven");
    expect(classifyKeeperUpdate(
      contract(),
      observation(),
      new Set([SESSION_ID]),
    )).toBe("unproven");
  });
});

test("journal envelope rejects a classification inconsistent with full contracts", () => {
  const source = contract();
  const target = contract({ bun_abi: "2.0.0" });
  const admitted = keeperUpdateAdmission(target, observation(source), new Set());
  expect(admitted?.classification).toBe("keeper-restart-required");
  expect(() => JournaledKeeperUpdateV1Schema.parse({
    admission: { ...admitted!, classification: "worker-only-safe", required_action: "preserve" },
    source_contract: source,
    target_contract: target,
  })).toThrow();
  expect(observation().binding_digest).toBe(KEEPER_EMPTY_BINDING_DIGEST);
});

test("coordinator session proof accepts only canonical UUID order", () => {
  const later = "00000000-0000-4000-8000-000000000002";
  expect(KeeperCoordinatorOpenSessionIdsSchema.parse([SESSION_ID, later])).toEqual([
    SESSION_ID,
    later,
  ]);
  expect(() => KeeperCoordinatorOpenSessionIdsSchema.parse([later, SESSION_ID])).toThrow();
  expect(() => KeeperCoordinatorOpenSessionIdsSchema.parse([SESSION_ID, SESSION_ID])).toThrow();
});

test("keeper outcomes are accepted only for their recorded action", () => {
  expect(keeperUpdateOutcomeMatchesAction("preserve", "preserved")).toBe(true);
  expect(keeperUpdateOutcomeMatchesAction("preserve", "shutdown")).toBe(false);
  expect(keeperUpdateOutcomeMatchesAction("replace-empty", "shutdown")).toBe(true);
  expect(keeperUpdateOutcomeMatchesAction("replace-empty", "preserved")).toBe(false);
  expect(keeperUpdateOutcomeMatchesAction("maintenance", "already-absent")).toBe(true);
  expect(keeperUpdateOutcomeMatchesAction("maintenance", "already-converged")).toBe(false);
});

test("keeper preparation session IDs survive the binary worker transport", () => {
  const later = "00000000-0000-4000-8000-000000000002";
  const frame = create(CoordWorkerDownSchema, {
    frame: {
      case: "keeperUpdatePrepare",
      value: create(DKeeperUpdatePrepareSchema, {
        requestId: "keeper-action-request",
        journaledUpdateJson: "{}",
        direction: "target",
        coordinatorOpenSessionIds: [SESSION_ID, later],
      }),
    },
  });
  const decoded = fromBinary(
    CoordWorkerDownSchema,
    toBinary(CoordWorkerDownSchema, frame),
  );
  expect(decoded.frame.case).toBe("keeperUpdatePrepare");
  if (decoded.frame.case !== "keeperUpdatePrepare") {
    throw new Error("keeper update frame did not survive transport");
  }
  expect(decoded.frame.value.coordinatorOpenSessionIds).toEqual([
    SESSION_ID,
    later,
  ]);
});

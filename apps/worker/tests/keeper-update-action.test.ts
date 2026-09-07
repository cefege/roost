// Keeper update action tests pin the authenticated live-worker boundary.
// Preserve proves keeper identity plus the current session/channel mapping;
// replacement accepts only an exact empty proof and waits for shutdown.

import { createHash } from "node:crypto";
import { describe, expect, test } from "bun:test";
import {
  KEEPER_EMPTY_BINDING_DIGEST,
  keeperBindingDigestInput,
  type JournaledKeeperUpdateV1,
  type KeeperContractV1,
} from "@roost/shared/keeper-update";
import {
  applyJournaledKeeperUpdateAction,
  type JournaledKeeperUpdateActionV1,
} from "../src/keeper/update-admission.ts";
import type { KeeperProbeResult } from "../src/keeper/keeper-probe.ts";

const SESSION_ID = "10000000-0000-4000-8000-000000000001";
const KEEPER_EPOCH = "20000000-0000-4000-8000-000000000001";
const KEEPER_PID = 4242;
const SOURCE_DIGEST = "1".repeat(64);
const TARGET_DIGEST = "2".repeat(64);
const BINDINGS = [{ channel_id: 7, pid: 5252 }] as const;
const ACTIVE_BINDING_DIGEST = createHash("sha256")
  .update(keeperBindingDigestInput(BINDINGS))
  .digest("hex");

function contract(
  implementationDigest: string,
  buildSha: string,
): KeeperContractV1 {
  return {
    protocol_version: 1,
    supported_features: ["keeper-contract-v1"],
    required_features: ["keeper-contract-v1"],
    implementation_digest: implementationDigest,
    bun_abi: "test",
    platform: "linux",
    arch: "x64",
    build_sha: buildSha,
  };
}

function update(requiredAction: "preserve" | "replace-empty"): JournaledKeeperUpdateV1 {
  const sourceContract = contract(SOURCE_DIGEST, "a".repeat(40));
  const targetContract = requiredAction === "preserve"
    ? contract(SOURCE_DIGEST, "b".repeat(40))
    : contract(TARGET_DIGEST, "b".repeat(40));
  return {
    admission: {
      classification: requiredAction === "preserve"
        ? "worker-only-safe"
        : "keeper-restart-required",
      source_contract_digest: SOURCE_DIGEST,
      target_contract_digest: targetContract.implementation_digest!,
      expected_keeper_pid: KEEPER_PID,
      expected_keeper_epoch: KEEPER_EPOCH,
      expected_binding_digest: requiredAction === "preserve"
        ? ACTIVE_BINDING_DIGEST
        : KEEPER_EMPTY_BINDING_DIGEST,
      required_action: requiredAction,
    },
    source_contract: sourceContract,
    target_contract: targetContract,
  };
}

function probe(
  runningContract: KeeperContractV1,
  bindings: readonly { channel_id: number; pid: number }[],
): KeeperProbeResult {
  return {
    reachable: true,
    authenticated: true,
    protocolCompatible: true,
    exactTarget: false,
    contract: runningContract,
    keeperPid: KEEPER_PID,
    processEpoch: KEEPER_EPOCH,
    bindings,
    spawningChannels: [],
    features: [],
  };
}

function action(
  requiredAction: "preserve" | "replace-empty",
  coordinatorSessionIds: readonly string[],
): JournaledKeeperUpdateActionV1 {
  return {
    schema_version: 1,
    update: update(requiredAction),
    direction: "target",
    coordinator_open_session_ids: coordinatorSessionIds,
    worker_open_channel_ids: coordinatorSessionIds.length === 0 ? [] : [7],
  };
}

describe("journaled keeper update action", () => {
  test("preserve proves the exact live identity and never invokes shutdown", async () => {
    let shutdownCalls = 0;
    const result = await applyJournaledKeeperUpdateAction(
      action("preserve", [SESSION_ID]),
      {
        probe: async () => probe(contract(SOURCE_DIGEST, "a".repeat(40)), BINDINGS),
        shutdownEmpty: async () => {
          shutdownCalls += 1;
          return true;
        },
      },
    );
    expect(result).toEqual({
      outcome: "preserved",
      keeper_pid: KEEPER_PID,
      keeper_epoch: KEEPER_EPOCH,
      binding_digest: ACTIVE_BINDING_DIGEST,
    });
    expect(shutdownCalls).toBe(0);
  });

  test("preserve rejects changed epoch proof without invoking shutdown", async () => {
    let shutdownCalls = 0;
    const changedEpochProbe = {
      ...probe(contract(SOURCE_DIGEST, "a".repeat(40)), BINDINGS),
      processEpoch: "30000000-0000-4000-8000-000000000001",
    };
    await expect(applyJournaledKeeperUpdateAction(
      action("preserve", [SESSION_ID]),
      {
        probe: async () => changedEpochProbe,
        shutdownEmpty: async () => {
          shutdownCalls += 1;
          return true;
        },
      },
    )).rejects.toThrow("preserve identity no longer matches");
    expect(shutdownCalls).toBe(0);
  });

  test("replace-empty refuses a recorded live session without shutdown", async () => {
    let shutdownCalls = 0;
    await expect(applyJournaledKeeperUpdateAction(
      action("replace-empty", [SESSION_ID]),
      {
        probe: async () => probe(contract(SOURCE_DIGEST, "a".repeat(40)), []),
        shutdownEmpty: async () => {
          shutdownCalls += 1;
          return true;
        },
      },
    )).rejects.toThrow("blocked by live sessions");
    expect(shutdownCalls).toBe(0);
  });

  test("replace-empty shuts down only the exact empty admitted keeper", async () => {
    let reachable = true;
    let shutdownCalls = 0;
    const result = await applyJournaledKeeperUpdateAction(
      action("replace-empty", []),
      {
        probe: async () => reachable
          ? probe(contract(SOURCE_DIGEST, "a".repeat(40)), [])
          : {
              reachable: false,
              authenticated: false,
              protocolCompatible: false,
              exactTarget: false,
              features: [],
            },
        shutdownEmpty: async (_endpoint, expectation) => {
          expect(expectation).toEqual({
            keeperPid: KEEPER_PID,
            processEpoch: KEEPER_EPOCH,
            bindingDigest: KEEPER_EMPTY_BINDING_DIGEST,
          });
          shutdownCalls += 1;
          reachable = false;
          return true;
        },
        sleep: async () => {},
        now: () => 0,
      },
    );
    expect(result.outcome).toBe("shutdown");
    expect(shutdownCalls).toBe(1);
  });

  test("source replacement accepts restored implementation with different provenance", async () => {
    let shutdownCalls = 0;
    const recorded = action("replace-empty", []);
    const result = await applyJournaledKeeperUpdateAction(
      { ...recorded, direction: "source" },
      {
        probe: async () => probe(contract(SOURCE_DIGEST, "b".repeat(40)), []),
        shutdownEmpty: async () => {
          shutdownCalls += 1;
          return true;
        },
      },
    );
    expect(result.outcome).toBe("already-converged");
    expect(shutdownCalls).toBe(0);
  });

  test("source preserve replay accepts exactly mapped binding drift", async () => {
    let shutdownCalls = 0;
    const recorded = action("preserve", [SESSION_ID]);
    const result = await applyJournaledKeeperUpdateAction(
      { ...recorded, direction: "source", worker_open_channel_ids: [9] },
      {
        probe: async () => ({
          ...probe(
            contract(SOURCE_DIGEST, "b".repeat(40)),
            [{ channel_id: 9, pid: 6262 }],
          ),
          keeperPid: 7171,
          processEpoch: "40000000-0000-4000-8000-000000000004",
        }),
        shutdownEmpty: async () => {
          shutdownCalls += 1;
          return true;
        },
      },
    );
    expect(result).toEqual({
      outcome: "preserved",
      keeper_pid: 7171,
      keeper_epoch: "40000000-0000-4000-8000-000000000004",
      binding_digest: createHash("sha256").update(keeperBindingDigestInput([
        { channel_id: 9, pid: 6262 },
      ])).digest("hex"),
    });
    expect(shutdownCalls).toBe(0);
  });

  test("preserve rejects a keeper channel absent from the worker session map", async () => {
    await expect(applyJournaledKeeperUpdateAction(
      action("preserve", [SESSION_ID]),
      {
        probe: async () => probe(
          contract(SOURCE_DIGEST, "a".repeat(40)),
          [{ channel_id: 9, pid: 6262 }],
        ),
      },
    )).rejects.toThrow("worker sessions and keeper channels changed");
  });

  test("rejects a journaled action that tries to carry force-live", async () => {
    let probeCalls = 0;
    await expect(applyJournaledKeeperUpdateAction(
      { ...action("replace-empty", []), force_live: true } as JournaledKeeperUpdateActionV1,
      {
        probe: async () => {
          probeCalls += 1;
          return probe(contract(SOURCE_DIGEST, "a".repeat(40)), []);
        },
      },
    )).rejects.toThrow();
    expect(probeCalls).toBe(0);
  });
});

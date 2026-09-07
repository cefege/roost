// Proves the worker half of keeper-runtime reporting against a real keeper:
// observeKeeperRuntime must produce an observation that the shared update
// admission contract accepts, otherwise every deploy stays blocked because
// workers.keeper_runtime_json can only ever be NULL or unusable.

import { afterAll, expect, test } from "bun:test";
import { createHash } from "node:crypto";
import { rmSync } from "node:fs";
import { homedir, tmpdir } from "node:os";
import { join } from "node:path";
import {
  keeperBindingDigestInput,
  keeperUpdateAdmission,
} from "@roost/shared/keeper-update";
import { keeperRuntimeObservationFromProto, keeperRuntimeObservationToProto } from "@roost/shared/keeper-update-proto";
import { observeKeeperRuntime } from "../src/heartbeat.ts";
import { probeKeeperCompatible } from "../src/keeper/keeper-probe.ts";
import {
  MultiplexedKeeperPool,
  type MuxChannelCallbacks,
} from "../src/keeper/multiplexed-client.ts";
import { muxLocalEndpoint } from "../src/keeper/keeper-pool-config.ts";
import { KEEPER_TARGET_CONTRACT } from "../src/keeper/keeper-stamp.ts";
import { keeperTestShellSpec } from "./keeper-test-fixtures.ts";

const SOCK_DIR = join(tmpdir(), `roost-test-keeper-runtime-report-${process.pid}`);
process.env.ROOST_WORKER_DATA_DIR = SOCK_DIR;
process.env.ROOST_KEEPER_QUIET = "1";

const RECONCILED_AT_MS = 1_700_000_000_000;
const OPEN_SESSION_ID = "00000000-0000-4000-8000-000000000001";

const pool = new MultiplexedKeeperPool();
const callbacks: MuxChannelCallbacks = {
  onOutput: () => {},
  onExit: () => {},
  onError: () => {},
};

afterAll(() => {
  const keeperPid = pool._keeperProc?.pid;
  pool.dispose();
  if (keeperPid) {
    try { process.kill(keeperPid, "SIGKILL"); } catch { /* already dead */ }
  }
  rmSync(SOCK_DIR, { recursive: true, force: true });
});

test("an absent keeper endpoint reports no runtime instead of a partial one", async () => {
  expect(await observeKeeperRuntime(RECONCILED_AT_MS)).toBeNull();
});

test("a real keeper's runtime observation admits a keeper-preserving update", async () => {
  const channelId = 811;
  const shellPid = await pool.spawn({
    channelId,
    shellSpec: keeperTestShellSpec({
      executable: "/bin/sh",
      argv: ["-c", "exec sleep 60"],
      cwd: homedir(),
    }),
    cols: 80,
    rows: 24,
    callbacks,
  });
  const probe = await probeKeeperCompatible(muxLocalEndpoint());
  const observation = await observeKeeperRuntime(RECONCILED_AT_MS);
  if (!observation) {
    throw new Error("a live keeper produced no runtime observation");
  }
  const { keeperPid, processEpoch } = probe;
  const targetDigest = KEEPER_TARGET_CONTRACT.implementation_digest;
  if (keeperPid === undefined || processEpoch === undefined || targetDigest === null) {
    throw new Error("the live keeper did not prove its identity or digest");
  }
  expect(observation).toEqual({
    schema_version: 1,
    running_contract: KEEPER_TARGET_CONTRACT,
    keeper_pid: keeperPid,
    keeper_epoch: processEpoch,
    channel_count: 1,
    binding_digest: createHash("sha256")
      .update(keeperBindingDigestInput([{ channel_id: channelId, pid: shellPid }], []))
      .digest("hex"),
    reconciled_at_ms: RECONCILED_AT_MS,
  });

  // The heartbeat ships the proto form; admission reads the Zod form back off
  // the coordinator row, so the two encodings must be lossless in both
  // directions or a truthful worker still looks unproven.
  expect(keeperRuntimeObservationFromProto(
    keeperRuntimeObservationToProto(observation),
  )).toEqual(observation);

  expect(keeperUpdateAdmission(
    KEEPER_TARGET_CONTRACT,
    observation,
    new Set([OPEN_SESSION_ID]),
  )).toEqual({
    classification: "worker-only-safe",
    source_contract_digest: targetDigest,
    target_contract_digest: targetDigest,
    expected_keeper_pid: keeperPid,
    expected_keeper_epoch: processEpoch,
    expected_binding_digest: observation.binding_digest,
    required_action: "preserve",
  });
}, 20_000);

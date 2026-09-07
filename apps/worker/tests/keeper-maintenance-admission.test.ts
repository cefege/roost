// Keeper maintenance admission. An empty keeper is retired through the
// identity-fenced shutdown, a keeper holding live channels is refused unless the
// operator authorizes the destruction, an unproven identity is refused either
// way, and an exit that takes real work still counts as a completed shutdown.

import { describe, expect, test } from "bun:test";
import { KEEPER_EMPTY_BINDING_DIGEST } from "@roost/shared/keeper-update";
import { shutdownKeeperForMaintenance } from "../src/keeper/update-admission.ts";
import type { KeeperProbeResult } from "../src/keeper/keeper-probe.ts";

const KEEPER_PID = 4242;
const KEEPER_EPOCH = "20000000-0000-4000-8000-000000000001";
const LIVE_BINDING = { channel_id: 7, pid: 5252 } as const;
const CONTRACT = {
  protocol_version: 1,
  supported_features: ["keeper-contract-v1"],
  required_features: ["keeper-contract-v1"],
  implementation_digest: "1".repeat(64),
  bun_abi: "test",
  platform: "linux" as const,
  arch: "x64",
  build_sha: "a".repeat(40),
};

function runningProbe(
  bindings: readonly { channel_id: number; pid: number }[],
): KeeperProbeResult {
  return {
    reachable: true,
    authenticated: true,
    protocolCompatible: true,
    exactTarget: true,
    contract: CONTRACT,
    keeperPid: KEEPER_PID,
    processEpoch: KEEPER_EPOCH,
    bindings,
    spawningChannels: [],
    features: [],
  };
}

const ABSENT_PROBE: KeeperProbeResult = {
  reachable: false,
  authenticated: false,
  protocolCompatible: false,
  exactTarget: false,
  features: [],
};

/** Exit confirmation polls a clock and sleeps; driving both keeps a 30 s budget
 * out of the wall clock while still proving the budget is honored. */
function virtualClock() {
  let elapsedMs = 0;
  return {
    now: () => elapsedMs,
    sleep: async (milliseconds: number) => { elapsedMs += milliseconds; },
    elapsedMs: () => elapsedMs,
  };
}

describe("keeper maintenance admission", () => {
  test("retires an empty keeper through the identity-fenced shutdown", async () => {
    let exited = false;
    let forcedCalls = 0;
    let expectation: unknown;
    const outcome = await shutdownKeeperForMaintenance(
      { forceLive: false },
      {
        probe: async () => exited ? ABSENT_PROBE : runningProbe([]),
        shutdownEmpty: async (_endpoint, expected) => {
          expectation = expected;
          exited = true;
          return true;
        },
        shutdownForced: async () => {
          forcedCalls += 1;
          return true;
        },
        sleep: async () => {},
        now: () => 0,
      },
    );
    expect(outcome).toBe("shutdown");
    expect(expectation).toEqual({
      keeperPid: KEEPER_PID,
      processEpoch: KEEPER_EPOCH,
      bindingDigest: KEEPER_EMPTY_BINDING_DIGEST,
    });
    expect(forcedCalls).toBe(0);
  });

  test("refuses a keeper holding live channels without force-live", async () => {
    let shutdownCalls = 0;
    await expect(shutdownKeeperForMaintenance(
      { forceLive: false },
      {
        probe: async () => runningProbe([LIVE_BINDING]),
        shutdownEmpty: async () => { shutdownCalls += 1; return true; },
        shutdownForced: async () => { shutdownCalls += 1; return true; },
      },
    )).rejects.toThrow("live channels");
    expect(shutdownCalls).toBe(0);
  });

  test("destroys a keeper holding live channels only when force-live is authorized", async () => {
    let exited = false;
    let emptyCalls = 0;
    let forcedCalls = 0;
    const capturedErrors: string[] = [];
    const originalError = console.error;
    console.error = (...values: unknown[]) => {
      capturedErrors.push(values.map(String).join(" "));
    };
    let outcome: string;
    try {
      outcome = await shutdownKeeperForMaintenance(
        { forceLive: true },
        {
          probe: async () => exited ? ABSENT_PROBE : runningProbe([LIVE_BINDING]),
          shutdownEmpty: async () => { emptyCalls += 1; return true; },
          shutdownForced: async () => {
            forcedCalls += 1;
            exited = true;
            return true;
          },
          sleep: async () => {},
          now: () => 0,
        },
      );
    } finally {
      console.error = originalError;
    }
    expect(outcome).toBe("shutdown");
    expect(forcedCalls).toBe(1);
    expect(emptyCalls).toBe(0);
    const destruction = capturedErrors
      .map(line => JSON.parse(line) as Record<string, unknown>)
      .find(entry => entry.msg === "maintenance_forced_live_keeper_shutdown");
    expect(destruction).toMatchObject({
      level: "warn",
      keeper_pid: KEEPER_PID,
      keeper_epoch: KEEPER_EPOCH,
      channel_bindings: [{ channel_id: 7, pid: 5252 }],
      spawning_channels: [],
    });
  });

  test("refuses force-live when the keeper identity is unproven", async () => {
    let shutdownCalls = 0;
    await expect(shutdownKeeperForMaintenance(
      { forceLive: true },
      {
        probe: async () => ({ ...ABSENT_PROBE, reachable: true }),
        shutdownEmpty: async () => { shutdownCalls += 1; return true; },
        shutdownForced: async () => { shutdownCalls += 1; return true; },
      },
    )).rejects.toThrow("keeper identity is unproven");
    expect(shutdownCalls).toBe(0);
  });

  test("reports an absent keeper without attempting a shutdown", async () => {
    let shutdownCalls = 0;
    const outcome = await shutdownKeeperForMaintenance(
      { forceLive: false },
      {
        probe: async () => ABSENT_PROBE,
        shutdownEmpty: async () => { shutdownCalls += 1; return true; },
        shutdownForced: async () => { shutdownCalls += 1; return true; },
      },
    );
    expect(outcome).toBe("already-absent");
    expect(shutdownCalls).toBe(0);
  });

  test("accepts an exit that takes longer than two seconds", async () => {
    const clock = virtualClock();
    let exitAtMs = Number.POSITIVE_INFINITY;
    const outcome = await shutdownKeeperForMaintenance(
      { forceLive: false },
      {
        probe: async () => clock.now() >= exitAtMs ? ABSENT_PROBE : runningProbe([]),
        shutdownEmpty: async () => {
          exitAtMs = clock.now() + 3_000;
          return true;
        },
        sleep: clock.sleep,
        now: clock.now,
      },
    );
    expect(outcome).toBe("shutdown");
    expect(clock.elapsedMs()).toBeGreaterThanOrEqual(3_000);
  });

  test("fails only after the full exit-confirmation budget", async () => {
    const clock = virtualClock();
    await expect(shutdownKeeperForMaintenance(
      { forceLive: false },
      {
        probe: async () => runningProbe([]),
        shutdownEmpty: async () => true,
        sleep: clock.sleep,
        now: clock.now,
      },
    )).rejects.toThrow("did not exit");
    expect(clock.elapsedMs()).toBeGreaterThanOrEqual(30_000);
  });
});

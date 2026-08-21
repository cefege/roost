import { expect, test } from "bun:test";
import { CoordinatorMovePhase } from "@roost/shared/proto/coordinator_pb";
import { createCoordRelocationRecovery } from "../src/coord-relocation-recovery.ts";
import type { WorkerCoordRelocation } from "../src/coord-relocation.ts";
import type { CoordLink } from "../src/transport/coord-link-types.ts";

const journal = {
  version: 1 as const,
  handoff_id: "00000000-0000-4000-8000-000000000000",
  source_url: "https://source.example.ts.net",
  target_url: "https://target.example.ts.net",
  state: "STAGED" as const,
  updated_at_ms: 0,
};

function link(relocations: string[]): CoordLink {
  return {
    send: () => false,
    sendBinary: () => "dropped",
    sendCellGrid: () => "dropped",
    sendCellGridChunk: () => "dropped",
    sendAgentStatus: () => false,
    state: () => ({ kind: "reconnecting", nextDialAtMs: 0, backoffMs: 100 }),
    relocate: (url) => { relocations.push(url); },
    unackedEventCount: () => 0,
    dispose: () => {},
  };
}

test("relocation recovery activates a staged target only after the source has been unavailable", async () => {
  let now = 0;
  const relocations: string[] = [];
  const activations: unknown[] = [];
  const endpoints: string[] = [];
  const recovery = createCoordRelocationRecovery({
    relocation: {
      load: () => journal,
      activate: (request: { handoff_id: string; source_url: string; target_url: string }) => { activations.push(request); },
    } as unknown as WorkerCoordRelocation,
    link: link(relocations),
    statusAt: async (url) => {
      if (url === journal.source_url) throw new Error("source unavailable");
      return { phase: CoordinatorMovePhase.WAITING_FOR_WORKERS };
    },
    setCoordinatorEndpoint: (url) => { endpoints.push(url); },
    reannounce: () => {},
    abortTarget: async () => {},
    now: () => now,
  });

  await recovery();
  expect(activations).toHaveLength(0);
  now = 15_000;
  await recovery();

  expect(activations).toEqual([{ handoff_id: journal.handoff_id, source_url: journal.source_url, target_url: journal.target_url }]);
  expect(endpoints).toEqual([journal.target_url]);
  expect(relocations).toEqual([journal.target_url]);
});

test("relocation recovery starts its outage timer after the last reachable source status", async () => {
  let now = 0;
  let sourceReachable = true;
  const relocations: string[] = [];
  let activated = false;
  const recovery = createCoordRelocationRecovery({
    relocation: {
      load: () => journal,
      activate: () => { activated = true; },
    } as unknown as WorkerCoordRelocation,
    link: link(relocations),
    statusAt: async (url) => {
      if (url === journal.source_url && !sourceReachable) throw new Error("source unavailable");
      return { phase: CoordinatorMovePhase.WAITING_FOR_WORKERS };
    },
    setCoordinatorEndpoint: () => {},
    reannounce: () => {},
    abortTarget: async () => {},
    now: () => now,
  });

  await recovery();
  now = 15_000;
  await recovery();
  sourceReachable = false;
  now = 16_000;
  await recovery();

  expect(activated).toBeFalse();
  expect(relocations).toEqual([]);

  now = 31_000;
  await recovery();
  expect(activated).toBeTrue();
});

test("relocation recovery rejects a target that has not durably accepted the handoff", async () => {
  const relocations: string[] = [];
  let activated = false;
  const recovery = createCoordRelocationRecovery({
    relocation: {
      load: () => journal,
      activate: () => { activated = true; },
    } as unknown as WorkerCoordRelocation,
    link: link(relocations),
    statusAt: async (url) => {
      if (url === journal.source_url) throw new Error("source unavailable");
      return { phase: CoordinatorMovePhase.PREPARING_TARGET };
    },
    setCoordinatorEndpoint: () => {},
    reannounce: () => {},
    abortTarget: async () => {},
    unavailableAfterMs: 0,
  });

  await recovery();

  expect(activated).toBeFalse();
  expect(relocations).toEqual([]);
});

test("relocation recovery leaves a reachable source authoritative", async () => {
  let now = 15_000;
  const relocations: string[] = [];
  let activated = false;
  const recovery = createCoordRelocationRecovery({
    relocation: {
      load: () => journal,
      activate: () => { activated = true; },
    } as unknown as WorkerCoordRelocation,
    link: link(relocations),
    statusAt: async () => ({ phase: CoordinatorMovePhase.COMMITTING }),
    setCoordinatorEndpoint: () => {},
    reannounce: () => {},
    abortTarget: async () => {},
    now: () => now,
    unavailableAfterMs: 0,
  });

  await recovery();
  now += 15_000;
  await recovery();

  expect(activated).toBeFalse();
  expect(relocations).toEqual([]);
});

test("relocation recovery restores target staging before returning to a rolled-back source", async () => {
  const calls: string[] = [];
  const relocations: string[] = [];
  const recovery = createCoordRelocationRecovery({
    relocation: {
      load: () => journal,
      abort: async (_handoffId: string, relocate: (url: string) => void) => {
        calls.push("worker-abort");
        relocate(journal.source_url);
      },
    } as unknown as WorkerCoordRelocation,
    link: link(relocations),
    statusAt: async () => ({ phase: CoordinatorMovePhase.ROLLED_BACK }),
    setCoordinatorEndpoint: (url) => { calls.push(`endpoint:${url}`); },
    reannounce: () => {},
    abortTarget: async (handoffId) => { calls.push(`target-abort:${handoffId}`); },
    unavailableAfterMs: 0,
  });

  await recovery();

  expect(calls).toEqual([
    `target-abort:${journal.handoff_id}`,
    "worker-abort",
    `endpoint:${journal.source_url}`,
  ]);
  expect(relocations).toEqual([journal.source_url]);
});

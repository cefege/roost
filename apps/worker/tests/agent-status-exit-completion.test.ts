// Process-exit completion contracts for the worker registry: a finished agent
// that leaves keeps its completion on the wire until the session closes and
// says so with `occupant_exited`, an agent with nothing to acknowledge is
// retired, an explicit `active: false` withdrawal stays a withdrawal, and a
// dead occupant backs no prompt proof.
import { describe, expect, test } from "bun:test";
import type { AgentStatusUpdate } from "@roost/shared/wire";
import { AgentStatusRegistry } from "../src/agent-status/registry.ts";

const SESSION_ID = "33333333-3333-4333-8333-333333333333";

function registryHarness(startAt = 5_000) {
  const clock = { now: startAt };
  const published: AgentStatusUpdate[] = [];
  const registry = new AgentStatusRegistry({
    publish: (status) => { published.push(status); },
    now: () => clock.now,
    leaseMs: 100,
    startLeaseTimer: false,
  });
  return { clock, published, registry };
}

describe("agent status process exit", () => {
  test("a completed agent that loses its process keeps the completion until close", () => {
    const { clock, published, registry } = registryHarness();
    registry.reportScreen(SESSION_ID, { agentId: "omp", processId: 71, state: "working", visibleBlocker: false });
    const working = published.at(-1)!;
    clock.now += 5;
    registry.reportScreen(SESSION_ID, { agentId: "omp", processId: 71, state: "idle", visibleBlocker: false });
    const completed = published.at(-1)!;
    expect(completed.completed_revision).toBe(completed.revision);

    clock.now += 5;
    registry.clearScreen(SESSION_ID);
    const exited = published.at(-1)!;
    expect(exited).toMatchObject({
      active: true,
      state: "idle",
      occupant_id: working.occupant_id,
      completed_revision: completed.completed_revision,
      occupant_exited: true,
    });
    expect(completed.occupant_exited).toBe(false);
    expect(exited.revision).toBeGreaterThan(completed.revision);
    expect(registry.snapshot()).toEqual([exited]);

    clock.now += 5;
    registry.closeSession(SESSION_ID);
    expect(published.at(-1)).toMatchObject({
      active: false,
      occupant_id: working.occupant_id,
      completed_revision: completed.completed_revision,
    });
    registry.dispose();
  });

  test("an exit while working publishes the completion the viewer never saw", () => {
    const { clock, published, registry } = registryHarness();
    registry.reportIntegration({
      sessionId: SESSION_ID,
      agentId: "omp",
      processId: 72,
      state: "blocked",
      message: "Approval needed",
      seq: 1,
      active: true,
    });
    const blocked = published.at(-1)!;

    clock.now += 101;
    registry.expireLeases();
    const exited = published.at(-1)!;
    expect(exited).toMatchObject({
      active: true,
      state: "idle",
      occupant_id: blocked.occupant_id,
      source: blocked.source,
      occupant_exited: true,
    });
    expect(exited.completed_revision).toBe(exited.revision);
    expect(exited.message).toBeUndefined();
    registry.dispose();
  });

  test("an idle agent with nothing to acknowledge is retired on exit", () => {
    const { clock, published, registry } = registryHarness();
    registry.reportScreen(SESSION_ID, { agentId: "pi", processId: 73, state: "idle", visibleBlocker: false });
    const idle = published.at(-1)!;
    expect(idle.completed_revision).toBe(0);

    clock.now += 5;
    registry.clearScreen(SESSION_ID);
    expect(published.at(-1)).toMatchObject({
      active: false,
      occupant_id: idle.occupant_id,
      completed_revision: 0,
    });
    expect(registry.snapshot()).toEqual([]);
    registry.dispose();
  });

  test("an integration's explicit withdrawal retires the row instead of completing", () => {
    const { published, registry } = registryHarness();
    registry.reportIntegration({
      sessionId: SESSION_ID,
      agentId: "omp",
      processId: 74,
      state: "working",
      seq: 1,
      active: true,
    });
    registry.reportIntegration({
      sessionId: SESSION_ID,
      agentId: "omp",
      processId: 74,
      state: "working",
      seq: 2,
      active: false,
    });
    expect(published.at(-1)).toMatchObject({ active: false, state: "working" });
    expect(registry.snapshot()).toEqual([]);
    registry.dispose();
  });

  test("a retained completion backs no prompt proof", () => {
    const { registry } = registryHarness();
    registry.reportScreen(SESSION_ID, { agentId: "omp", processId: 75, state: "working", visibleBlocker: false });
    expect(registry.currentPrivateProof(SESSION_ID)).toMatchObject({
      state: "working",
      process: { agentId: "omp", pid: 75 },
    });

    registry.clearScreen(SESSION_ID);
    expect(registry.currentPrivateProof(SESSION_ID)).toBeNull();
    registry.dispose();
  });
});

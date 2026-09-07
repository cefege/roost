// Durable observed-agent identity contracts for the worker registry. These
// tests pin private PID continuity, ordered replacement tombstones, retired
// reporter fencing, and reconnect snapshots without exposing a PID on wire
// status values.
import { describe, expect, test } from "bun:test";
import type { AgentStatusUpdate } from "@roost/shared/wire";
import { AgentStatusRegistry } from "../src/agent-status/registry.ts";

const SESSION_ID = "11111111-1111-4111-8111-111111111111";
const OTHER_SESSION_ID = "22222222-2222-4222-8222-222222222222";

function registryHarness(startAt = 1_000) {
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

function expectNoProcessId(status: AgentStatusUpdate): void {
  expect(Object.keys(status)).not.toContain("pid");
  expect(Object.keys(status)).not.toContain("processId");
  expect(Object.keys(status)).not.toContain("process_id");
}

describe("agent status occupant continuity", () => {
  test("source, state, and message changes retain one occupant for one process", () => {
    const { clock, published, registry } = registryHarness();
    registry.reportScreen(SESSION_ID, {
      agentId: "omp",
      processId: 101,
      state: "working",
      visibleBlocker: false,
    });
    const screenWorking = published.at(-1)!;
    expect(screenWorking).toMatchObject({
      active: true,
      source: "screen",
      state: "working",
      completed_revision: 0,
    });

    expect(registry.reportIntegration({
      sessionId: SESSION_ID,
      agentId: "omp",
      processId: 101,
      state: "working",
      seq: 1,
      active: true,
    })).toBe(true);
    const sourceOnly = published.at(-1)!;
    expect(sourceOnly).toMatchObject({
      source: "integration",
      state: screenWorking.state,
      status_epoch: screenWorking.status_epoch,
      occupant_id: screenWorking.occupant_id,
    });

    expect(registry.reportIntegration({
      sessionId: SESSION_ID,
      agentId: "omp",
      processId: 101,
      state: "blocked",
      message: "approval needed",
      seq: 2,
      active: true,
    })).toBe(true);
    const integrated = published.at(-1)!;
    expect(integrated).toMatchObject({
      active: true,
      source: "integration",
      state: "blocked",
      message: "approval needed",
      status_epoch: screenWorking.status_epoch,
      occupant_id: screenWorking.occupant_id,
    });

    expect(registry.reportIntegration({
      sessionId: SESSION_ID,
      agentId: "omp",
      processId: 101,
      state: "idle",
      message: "complete",
      seq: 3,
      active: true,
    })).toBe(true);
    const integratedIdle = published.at(-1)!;
    expect(integratedIdle.occupant_id).toBe(screenWorking.occupant_id);
    expect(integratedIdle.completed_revision).toBe(integratedIdle.revision);

    registry.reportScreen(SESSION_ID, {
      agentId: "omp",
      processId: 101,
      state: "working",
      visibleBlocker: false,
    });
    expect(published.at(-1)).toBe(integratedIdle);
    clock.now += 101;
    registry.expireLeases();
    const screenFallback = published.at(-1)!;
    expect(screenFallback).toMatchObject({
      active: true,
      source: "screen",
      state: "working",
      status_epoch: screenWorking.status_epoch,
      occupant_id: screenWorking.occupant_id,
      completed_revision: integratedIdle.completed_revision,
    });
    expect(screenFallback.message).toBeUndefined();
    for (const status of published) expectNoProcessId(status);
    registry.dispose();
  });

  test("each registry has one epoch and separate registries have different epochs", () => {
    const first = registryHarness();
    const second = registryHarness();
    first.registry.reportScreen(SESSION_ID, {
      agentId: "codex", processId: 11, state: "working",
      visibleBlocker: false,
    });
    first.registry.reportScreen(OTHER_SESSION_ID, {
      agentId: "pi", processId: 12, state: "idle",
      visibleBlocker: false,
    });
    second.registry.reportScreen(SESSION_ID, {
      agentId: "codex", processId: 11, state: "working",
      visibleBlocker: false,
    });

    expect(first.published[0]?.status_epoch).toBe(first.published[1]?.status_epoch);
    expect(first.published[0]?.status_epoch).not.toBe(second.published[0]?.status_epoch);
    first.registry.dispose();
    second.registry.dispose();
  });
});

describe("agent status occupant replacement", () => {
  test("new PID publishes exact old inactive before fresh idle occupant", () => {
    const { published, registry } = registryHarness();
    expect(registry.reportIntegration({
      sessionId: SESSION_ID,
      agentId: "omp",
      processId: 100,
      state: "working",
      message: "old process",
      seq: 900,
      active: true,
    })).toBe(true);
    const oldActive = published.at(-1)!;

    expect(registry.reportIntegration({
      sessionId: SESSION_ID,
      agentId: "omp",
      processId: 200,
      state: "idle",
      seq: 0,
      active: true,
    })).toBe(true);
    const [oldInactive, newActive] = published.slice(-2);
    expect(oldInactive).toMatchObject({
      active: false,
      agent_id: oldActive.agent_id,
      state: oldActive.state,
      message: oldActive.message,
      status_epoch: oldActive.status_epoch,
      occupant_id: oldActive.occupant_id,
      source: oldActive.source,
      completed_revision: oldActive.completed_revision,
    });
    expect(oldInactive!.revision).toBeGreaterThan(oldActive.revision);
    expect(newActive).toMatchObject({
      active: true,
      state: "idle",
      status_epoch: oldActive.status_epoch,
      completed_revision: 0,
    });
    expect(newActive!.occupant_id).not.toBe(oldActive.occupant_id);
    expect(newActive!.revision).toBeGreaterThan(oldInactive!.revision);
    registry.dispose();
  });

  test("agent-kind change at one numeric PID is also a replacement", () => {
    const { published, registry } = registryHarness();
    registry.reportScreen(SESSION_ID, {
      agentId: "omp", processId: 300, state: "working",
      visibleBlocker: false,
    });
    const oldOccupant = published.at(-1)!.occupant_id;
    registry.reportScreen(SESSION_ID, {
      agentId: "pi", processId: 300, state: "working",
      visibleBlocker: false,
    });

    expect(published.slice(-2).map((status) => status.active)).toEqual([false, true]);
    expect(published.at(-2)?.occupant_id).toBe(oldOccupant);
    expect(published.at(-1)?.occupant_id).not.toBe(oldOccupant);
    registry.dispose();
  });

  test("a replacement reporter resets sequence while a retired reporter cannot reclaim", () => {
    const { published, registry } = registryHarness();
    registry.reportIntegration({
      sessionId: SESSION_ID,
      agentId: "omp",
      processId: 401,
      state: "working",
      seq: 50_000,
      active: true,
    });
    expect(registry.reportIntegration({
      sessionId: SESSION_ID,
      agentId: "omp",
      processId: 402,
      state: "blocked",
      seq: 0,
      active: true,
    })).toBe(true);
    const replacement = published.at(-1)!;
    const countAfterReplacement = published.length;

    expect(registry.reportIntegration({
      sessionId: SESSION_ID,
      agentId: "omp",
      processId: 401,
      state: "idle",
      seq: 50_001,
      active: true,
    })).toBe(false);
    expect(registry.reportIntegration({
      sessionId: SESSION_ID,
      agentId: "omp",
      processId: 401,
      state: "working",
      seq: 50_002,
      active: false,
    })).toBe(false);
    expect(published).toHaveLength(countAfterReplacement);
    expect(registry.snapshot()[0]?.occupant_id).toBe(replacement.occupant_id);
    registry.dispose();
  });

  test("disappearance followed by the same numeric PID mints a new occupant", () => {
    const { published, registry } = registryHarness();
    registry.reportScreen(SESSION_ID, {
      agentId: "codex", processId: 501, state: "working",
      visibleBlocker: false,
    });
    const first = published.at(-1)!;
    registry.clearScreen(SESSION_ID);
    const exited = published.at(-1)!;
    expect(exited).toMatchObject({
      active: true,
      state: "idle",
      occupant_id: first.occupant_id,
    });

    registry.reportScreen(SESSION_ID, {
      agentId: "codex", processId: 501, state: "idle",
      visibleBlocker: false,
    });
    const [retired, reappeared] = published.slice(-2);
    expect(retired).toMatchObject({
      active: false,
      occupant_id: first.occupant_id,
      completed_revision: exited.completed_revision,
    });
    expect(reappeared).toMatchObject({ active: true, completed_revision: 0 });
    expect(reappeared!.status_epoch).toBe(first.status_epoch);
    expect(reappeared!.occupant_id).not.toBe(first.occupant_id);
    registry.dispose();
  });

  test("inactive report rejects delayed active until absence proves a new incarnation", () => {
    const { published, registry } = registryHarness();
    registry.reportIntegration({
      sessionId: SESSION_ID,
      agentId: "pi",
      processId: 502,
      state: "working",
      seq: 900,
      active: true,
    });
    const first = published.at(-1)!;
    expect(registry.reportIntegration({
      sessionId: SESSION_ID,
      agentId: "pi",
      processId: 502,
      state: "working",
      seq: 901,
      active: false,
    })).toBe(true);
    expect(published.at(-1)).toMatchObject({
      active: false,
      occupant_id: first.occupant_id,
    });
    const countAfterInactive = published.length;

    for (const seq of [900, 901, 902]) {
      expect(registry.reportIntegration({
        sessionId: SESSION_ID,
        agentId: "pi",
        processId: 502,
        state: "idle",
        seq,
        active: true,
      })).toBe(false);
    }
    expect(registry.reportScreen(SESSION_ID, {
      agentId: "pi",
      processId: 502,
      state: "idle",
      visibleBlocker: false,
    })).toBe(false);
    expect(published).toHaveLength(countAfterInactive);

    registry.clearScreen(SESSION_ID);
    expect(registry.reportScreen(SESSION_ID, {
      agentId: "pi",
      processId: 502,
      state: "idle",
      visibleBlocker: false,
    })).toBe(true);
    const reappeared = published.at(-1)!;
    expect(reappeared).toMatchObject({
      active: true,
      source: "screen",
      completed_revision: 0,
      status_epoch: first.status_epoch,
    });
    expect(reappeared.occupant_id).not.toBe(first.occupant_id);
    expect(registry.reportIntegration({
      sessionId: SESSION_ID,
      agentId: "pi",
      processId: 502,
      state: "idle",
      seq: 0,
      active: true,
    })).toBe(true);
    expect(published.at(-1)).toMatchObject({
      active: true,
      source: "integration",
      occupant_id: reappeared.occupant_id,
    });
    registry.dispose();
  });
});

test("snapshot and reconnect resend preserve exact identity and revision", () => {
  const { published, registry } = registryHarness();
  registry.reportIntegration({
    sessionId: SESSION_ID,
    agentId: "pi",
    processId: 601,
    state: "blocked",
    message: "waiting",
    seq: 1,
    active: true,
  });
  const original = published.at(-1)!;
  const snapshot = registry.snapshot();
  expect(snapshot).toEqual([original]);

  registry.resend();
  expect(published.at(-1)).toEqual(original);
  expect(published.at(-1)).not.toBe(original);
  expect(published.at(-1)).toMatchObject({
    status_epoch: original.status_epoch,
    occupant_id: original.occupant_id,
    source: original.source,
    revision: original.revision,
  });
  registry.dispose();
});

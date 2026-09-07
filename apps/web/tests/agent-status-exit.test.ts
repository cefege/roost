// Released-occupant retirement in the browser projection: a row whose occupant
// already exited exists only to carry the completion that occupant earned, so
// it survives while unacknowledged and disappears once this profile has seen
// it — whether the acknowledgement arrives before or after the exit frame.

import { afterAll, beforeAll, beforeEach, describe, expect, test } from "bun:test";
import type { AgentStatus as AgentStatusValue } from "@roost/shared/wire";
import {
  applyAgentStatusFrame,
  resetAgentStatusProjection,
  retireSpentReleasedAgentStatuses,
} from "../src/store/agent-status.ts";
import { rootStore } from "../src/store/root.ts";
import {
  markAgentSeen,
  resetAgentSeenForTest,
  seenAgentRevision,
} from "../src/lib/agentSeen.ts";
import { deriveAgentStatusLevel } from "../src/lib/agentStatus.ts";
import {
  IDENTITY_A,
  SESSION_ID,
  frame,
  installBrowserProfileGlobals,
  status,
  storage,
} from "./agent-status-test-harness.ts";

let restoreBrowserProfileGlobals = () => {};

beforeAll(() => { restoreBrowserProfileGlobals = installBrowserProfileGlobals(); });

beforeEach(() => {
  storage.clear();
  resetAgentStatusProjection();
  resetAgentSeenForTest();
});

afterAll(() => { restoreBrowserProfileGlobals(); });

function current(): AgentStatusValue | undefined {
  return rootStore.agent_status[SESSION_ID] as AgentStatusValue | undefined;
}

describe("released agent occupants", () => {
  test("an unacknowledged completion outlives the occupant that earned it", () => {
    expect(applyAgentStatusFrame(
      frame(status("idle", 30, 30, SESSION_ID, "omp", IDENTITY_A)),
    )).toBe(true);

    const exited = { ...status("idle", 31, 30, SESSION_ID, "omp", IDENTITY_A), occupant_exited: true };
    expect(applyAgentStatusFrame(frame(exited))).toBe(true);
    expect(current()?.revision).toBe(31);
    expect(deriveAgentStatusLevel(current(), seenAgentRevision(current()))).toBe("done");

    retireSpentReleasedAgentStatuses();
    expect(current()?.revision).toBe(31);
  });

  test("an acknowledged completion retires the row its occupant left behind", () => {
    applyAgentStatusFrame(frame(status("idle", 40, 40, SESSION_ID, "omp", IDENTITY_A)));
    expect(markAgentSeen(current()!)).toBe(true);

    // A present agent keeps its row: acknowledgement only downgrades Done to idle.
    expect(applyAgentStatusFrame(
      frame(status("idle", 41, 40, SESSION_ID, "omp", IDENTITY_A)),
    )).toBe(true);
    retireSpentReleasedAgentStatuses();
    expect(current()?.revision).toBe(41);
    expect(deriveAgentStatusLevel(current(), seenAgentRevision(current()))).toBe("idle");

    const exited = { ...status("idle", 42, 40, SESSION_ID, "omp", IDENTITY_A), occupant_exited: true };
    expect(applyAgentStatusFrame(frame(exited))).toBe(true);
    expect(current()).toBeUndefined();
  });

  test("acknowledging after the occupant left retires the row it was holding", () => {
    applyAgentStatusFrame(frame(status("idle", 50, 50, SESSION_ID, "omp", IDENTITY_A)));
    const exited = { ...status("idle", 51, 50, SESSION_ID, "omp", IDENTITY_A), occupant_exited: true };
    applyAgentStatusFrame(frame(exited));
    expect(current()?.revision).toBe(51);

    markAgentSeen(current()!);
    retireSpentReleasedAgentStatuses();
    expect(current()).toBeUndefined();
  });

  test("a released occupant with no completion to carry is never retained", () => {
    const exited = { ...status("idle", 60, 0, SESSION_ID, "omp", IDENTITY_A), occupant_exited: true };
    expect(applyAgentStatusFrame(frame(exited))).toBe(true);
    expect(current()).toBeUndefined();
  });
});

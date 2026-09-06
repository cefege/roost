// Browser-profile agent notification tests cover identity-pinned tab election
// and the explicit desktop-delivery preference boundary. Storage is isolated
// from the process so claims cannot leak between test cases.

import { afterAll, beforeAll, beforeEach, describe, expect, test } from "bun:test";
import {
  AgentOccupantId,
  AgentStatus,
  StatusEpoch,
  asSessionId,
  type AgentStatusIdentity,
} from "@roost/shared/wire";
import { claimAgentNotification } from "../src/lib/agentNotificationClaim.ts";
import type { AgentNotificationDelivery } from "../src/lib/agentNotificationCore.ts";
import { agentStatusRevisionToken } from "../src/lib/agentStatus.ts";
import {
  disableDesktopNotifications,
  enableDesktopNotifications,
  notifyPrefs,
  resetNotifyPrefsForTest,
} from "../src/lib/notifyPrefs.ts";

const SESSION_ID = asSessionId("11111111-1111-4111-8111-111111111111");
const STATUS_EPOCH = StatusEpoch.parse("aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa");
const IDENTITY_A: AgentStatusIdentity = {
  status_epoch: STATUS_EPOCH,
  occupant_id: AgentOccupantId.parse("aaaaaaaa-1111-4111-8111-aaaaaaaaaaaa"),
  source: "integration",
};
const IDENTITY_B: AgentStatusIdentity = {
  status_epoch: STATUS_EPOCH,
  occupant_id: AgentOccupantId.parse("bbbbbbbb-2222-4222-8222-bbbbbbbbbbbb"),
  source: "integration",
};

class MemoryStorage implements Storage {
  private readonly values = new Map<string, string>();
  get length(): number { return this.values.size; }
  clear(): void { this.values.clear(); }
  getItem(key: string): string | null { return this.values.get(key) ?? null; }
  key(index: number): string | null { return [...this.values.keys()][index] ?? null; }
  removeItem(key: string): void { this.values.delete(key); }
  setItem(key: string, value: string): void { this.values.set(key, String(value)); }
}

const storage = new MemoryStorage();
const originalStorage = Object.getOwnPropertyDescriptor(globalThis, "localStorage");
const originalNavigator = Object.getOwnPropertyDescriptor(globalThis, "navigator");

beforeAll(() => {
  Object.defineProperty(globalThis, "localStorage", { configurable: true, value: storage });
  Object.defineProperty(globalThis, "navigator", { configurable: true, value: {} });
});

beforeEach(() => {
  storage.clear();
  resetNotifyPrefsForTest();
});

afterAll(() => {
  if (originalStorage) Object.defineProperty(globalThis, "localStorage", originalStorage);
  else Reflect.deleteProperty(globalThis, "localStorage");
  if (originalNavigator) Object.defineProperty(globalThis, "navigator", originalNavigator);
  else Reflect.deleteProperty(globalThis, "navigator");
});

function blocked(identity: AgentStatusIdentity) {
  return AgentStatus.parse({
    session_id: SESSION_ID,
    agent_id: "omp",
    state: "blocked",
    revision: 42,
    completed_revision: 0,
    updated_at: 42,
    active: true,
    ...identity,
  });
}

describe("browser-profile agent notification state", () => {
  test("elects claims per exact occupant and revision", async () => {
    const claimA: AgentNotificationDelivery = {
      sessionId: SESSION_ID,
      token: agentStatusRevisionToken(blocked(IDENTITY_A)),
      statusRevision: 42,
      kind: "blocked",
    };
    const claimB: AgentNotificationDelivery = {
      ...claimA,
      token: agentStatusRevisionToken(blocked(IDENTITY_B)),
    };
    const results = await Promise.all([
      claimAgentNotification(claimA),
      claimAgentNotification(claimA),
    ]);
    expect(results.filter(Boolean)).toHaveLength(1);
    expect(await claimAgentNotification(claimA)).toBe(false);
    expect(await claimAgentNotification(claimB)).toBe(true);
  });

  test("persists desktop enable only after subscription succeeds", async () => {
    await expect(enableDesktopNotifications(async () => {
      throw new Error("permission denied");
    })).rejects.toThrow("permission denied");
    expect(notifyPrefs().desktop).toBe(false);

    await enableDesktopNotifications(async () => {});
    expect(notifyPrefs().desktop).toBe(true);
    let unsubscribed = false;
    await disableDesktopNotifications(async () => { unsubscribed = true; });
    expect(unsubscribed).toBe(true);
    expect(notifyPrefs().desktop).toBe(false);
  });
});

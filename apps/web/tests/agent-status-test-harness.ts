// Shared browser-profile harness for the agent-status test files. Fake profile
// storage and a fake window let acknowledgement persistence and its cross-tab
// merge run without a browser runtime; the identity constants and the
// status/frame builders keep both files projecting the same wire shapes.

import { create } from "@bufbuild/protobuf";
import {
  AgentOccupantId,
  AgentStatus,
  StatusEpoch,
  asSessionId,
  type AgentStatus as AgentStatusValue,
  type AgentStatusIdentity,
} from "@roost/shared/wire";
import { AgentStatusFrameSchema } from "@roost/shared/proto/sync_pb";

export const SESSION_ID = asSessionId("11111111-1111-4111-8111-111111111111");
export const OTHER_ID = asSessionId("22222222-2222-4222-8222-222222222222");
const EPOCH_A = StatusEpoch.parse("aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa");
const OCCUPANT_A = AgentOccupantId.parse("aaaaaaaa-1111-4111-8111-aaaaaaaaaaaa");
const OCCUPANT_B = AgentOccupantId.parse("bbbbbbbb-2222-4222-8222-bbbbbbbbbbbb");
export const IDENTITY_A: AgentStatusIdentity = {
  status_epoch: EPOCH_A,
  occupant_id: OCCUPANT_A,
  source: "integration",
};
export const IDENTITY_B: AgentStatusIdentity = {
  status_epoch: EPOCH_A,
  occupant_id: OCCUPANT_B,
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

/** Cross-tab writes and pagehide are the only window events the acknowledgement
 *  store listens for, and it reads nothing but the changed key and its value. */
type ProfileWindowEvent = Partial<Pick<StorageEvent, "key" | "newValue">>;

class FakeWindow {
  private readonly listeners = new Map<string, Set<(event: ProfileWindowEvent) => void>>();
  addEventListener(type: string, listener: (event: ProfileWindowEvent) => void): void {
    const listeners = this.listeners.get(type) ?? new Set();
    listeners.add(listener);
    this.listeners.set(type, listeners);
  }
  removeEventListener(type: string, listener: (event: ProfileWindowEvent) => void): void {
    this.listeners.get(type)?.delete(listener);
  }
  emit(type: string, event: ProfileWindowEvent): void {
    for (const listener of this.listeners.get(type) ?? []) listener(event);
  }
}

export const storage = new MemoryStorage();
export const fakeWindow = new FakeWindow();

/** Install the fake profile globals; the returned callback restores them. */
export function installBrowserProfileGlobals(): () => void {
  const originalStorage = Object.getOwnPropertyDescriptor(globalThis, "localStorage");
  const originalWindow = Object.getOwnPropertyDescriptor(globalThis, "window");
  Object.defineProperty(globalThis, "localStorage", { configurable: true, value: storage });
  Object.defineProperty(globalThis, "window", { configurable: true, value: fakeWindow });
  return () => {
    if (originalStorage) Object.defineProperty(globalThis, "localStorage", originalStorage);
    else Reflect.deleteProperty(globalThis, "localStorage");
    if (originalWindow) Object.defineProperty(globalThis, "window", originalWindow);
    else Reflect.deleteProperty(globalThis, "window");
  };
}

export function status(
  state: AgentStatusValue["state"],
  revision: number,
  completedRevision = 0,
  sessionId = SESSION_ID,
  agentId = "omp",
  identity?: AgentStatusIdentity,
): AgentStatusValue {
  return AgentStatus.parse({
    session_id: sessionId,
    agent_id: agentId,
    state,
    revision,
    completed_revision: completedRevision,
    updated_at: revision,
    active: true,
    ...identity,
  });
}

export function frame(
  value: AgentStatusValue | (Omit<AgentStatusValue, "active"> & { active: false }),
) {
  return create(AgentStatusFrameSchema, {
    sessionId: value.session_id,
    agentId: value.agent_id,
    state: value.state,
    message: value.message,
    revision: BigInt(value.revision),
    completedRevision: BigInt(value.completed_revision),
    updatedAt: value.updated_at,
    active: value.active,
    statusEpoch: value.status_epoch,
    occupantId: value.occupant_id,
    source: value.source,
    occupantExited: value.occupant_exited,
  });
}

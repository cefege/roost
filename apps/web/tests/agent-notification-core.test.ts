// Agent notification debounce regressions cover exact-occupant carry-forward.
// Metadata revisions may refresh the current-status fence without moving the
// original timer or completion token; lifecycle changes must cancel it.

import { afterEach, describe, expect, test, vi } from "bun:test";
import {
  AgentOccupantId,
  AgentStatus,
  StatusEpoch,
  asSessionId,
  type AgentStatus as AgentStatusValue,
  type AgentStatusIdentity,
} from "@roost/shared/wire";
import {
  AgentNotificationScheduler,
  type AgentNotificationDelivery,
} from "../src/lib/agentNotificationCore.ts";
import { agentStatusRevisionToken } from "../src/lib/agentStatus.ts";

const SESSION_ID = asSessionId("11111111-1111-4111-8111-111111111111");
const STATUS_EPOCH = StatusEpoch.parse("aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa");
const IDENTITY_A: AgentStatusIdentity = {
  status_epoch: STATUS_EPOCH,
  occupant_id: AgentOccupantId.parse("aaaaaaaa-1111-4111-8111-aaaaaaaaaaaa"),
  source: "integration",
};
const IDENTITY_A_SCREEN: AgentStatusIdentity = { ...IDENTITY_A, source: "screen" };
const IDENTITY_B: AgentStatusIdentity = {
  status_epoch: STATUS_EPOCH,
  occupant_id: AgentOccupantId.parse("bbbbbbbb-2222-4222-8222-bbbbbbbbbbbb"),
  source: "integration",
};

function status(
  state: AgentStatusValue["state"],
  revision: number,
  completedRevision: number,
  identity?: AgentStatusIdentity,
  message?: string,
): AgentStatusValue {
  return AgentStatus.parse({
    session_id: SESSION_ID,
    agent_id: "omp",
    state,
    message,
    revision,
    completed_revision: completedRevision,
    updated_at: revision,
    active: true,
    ...identity,
  });
}

function schedulerFixture() {
  const current = new Map<string, AgentStatusValue>();
  const deliveries: AgentNotificationDelivery[] = [];
  const scheduler = new AgentNotificationScheduler({
    statusFor: (sessionId) => current.get(sessionId),
    isViewed: () => false,
    markSeen: () => {},
    deliver: (delivery) => { deliveries.push(delivery); },
  });
  const publish = (previous: AgentStatusValue | null, next: AgentStatusValue | null) => {
    if (next) current.set(SESSION_ID, next);
    else current.delete(SESSION_ID);
    scheduler.handle({
      sessionId: SESSION_ID,
      previous,
      next,
      revision: next?.revision ?? (previous?.revision ?? 0) + 1,
    });
  };
  return { current, deliveries, publish, scheduler };
}

afterEach(() => {
  vi.useRealTimers();
});

describe("agent notification debounce identity", () => {
  test("keeps the original blocked timer through same-occupant source and message revisions", () => {
    vi.useFakeTimers();
    const fixture = schedulerFixture();
    const working = status("working", 1, 0, IDENTITY_A);
    const blocked = status("blocked", 2, 0, IDENTITY_A);
    fixture.publish(working, blocked);

    vi.advanceTimersByTime(600);
    const sourceUpdate = status("blocked", 3, 0, IDENTITY_A_SCREEN);
    fixture.publish(blocked, sourceUpdate);
    vi.advanceTimersByTime(200);
    const messageUpdate = status("blocked", 4, 0, IDENTITY_A_SCREEN, "still waiting");
    fixture.publish(sourceUpdate, messageUpdate);

    vi.advanceTimersByTime(199);
    expect(fixture.deliveries).toHaveLength(0);
    vi.advanceTimersByTime(1);
    expect(fixture.deliveries).toEqual([{
      sessionId: SESSION_ID,
      token: agentStatusRevisionToken(blocked),
      statusRevision: 4,
      kind: "blocked",
    }]);
    fixture.scheduler.dispose();
  });

  test("keeps the original done token while refreshing the current-status revision", () => {
    vi.useFakeTimers();
    const fixture = schedulerFixture();
    const blocked = status("blocked", 7, 0, IDENTITY_A);
    const completed = status("idle", 8, 8, IDENTITY_A);
    fixture.publish(blocked, completed);

    vi.advanceTimersByTime(600);
    const messageUpdate = status("idle", 9, 8, IDENTITY_A, "summary ready");
    fixture.publish(completed, messageUpdate);
    const sourceUpdate = status("idle", 10, 8, IDENTITY_A_SCREEN, "summary ready");
    fixture.publish(messageUpdate, sourceUpdate);

    vi.advanceTimersByTime(400);
    expect(fixture.deliveries).toEqual([{
      sessionId: SESSION_ID,
      token: agentStatusRevisionToken(completed),
      statusRevision: 10,
      kind: "done",
      completedRevision: 8,
    }]);
    fixture.scheduler.dispose();
  });

  test("cancels pending notifications on occupant replacement, state exit, and retirement", () => {
    vi.useFakeTimers();

    const replacementFixture = schedulerFixture();
    const working = status("working", 1, 0, IDENTITY_A);
    const blocked = status("blocked", 2, 0, IDENTITY_A);
    replacementFixture.publish(working, blocked);
    replacementFixture.publish(blocked, status("blocked", 1, 0, IDENTITY_B));
    expect(replacementFixture.scheduler.pendingCount()).toBe(0);

    const exitFixture = schedulerFixture();
    const completed = status("idle", 3, 3, IDENTITY_A);
    exitFixture.publish(blocked, completed);
    exitFixture.publish(completed, status("working", 4, 3, IDENTITY_A));
    expect(exitFixture.scheduler.pendingCount()).toBe(0);

    const retirementFixture = schedulerFixture();
    retirementFixture.publish(working, blocked);
    retirementFixture.publish(blocked, null);
    expect(retirementFixture.scheduler.pendingCount()).toBe(0);

    vi.advanceTimersByTime(1_000);
    expect(replacementFixture.deliveries).toHaveLength(0);
    expect(exitFixture.deliveries).toHaveLength(0);
    expect(retirementFixture.deliveries).toHaveLength(0);
  });

  test("does not carry an identityless legacy notification", () => {
    vi.useFakeTimers();
    const fixture = schedulerFixture();
    const working = status("working", 1, 0);
    const blocked = status("blocked", 2, 0);
    fixture.publish(working, blocked);
    fixture.publish(blocked, status("blocked", 3, 0, undefined, "legacy update"));

    expect(fixture.scheduler.pendingCount()).toBe(0);
    vi.advanceTimersByTime(1_000);
    expect(fixture.deliveries).toHaveLength(0);
  });
});

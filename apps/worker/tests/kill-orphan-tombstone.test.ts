import { afterEach, describe, expect, test } from "bun:test";
import { asSessionId, asWorkerFp } from "@roost/shared/wire";
import { handleKill } from "../src/browser-command-spawn.ts";
import { SessionManager } from "../src/session-manager.ts";
import { LifecycleTestSink } from "./lifecycle-test-sink.ts";

const managers: SessionManager[] = [];

afterEach(() => {
  for (const manager of managers.splice(0)) manager.dispose();
});

function fixture(capacity = Number.MAX_SAFE_INTEGER) {
  const sink = new LifecycleTestSink(capacity);
  const manager = new SessionManager({
    workerFp: asWorkerFp("00".repeat(32)),
    sink,
  });
  managers.push(manager);
  return { manager, sink };
}

const orphanSid = asSessionId("11111111-1111-1111-1111-111111111111");

describe("lifecycle close admission", () => {
  test("unknown kill consumes one reserved closed tombstone", () => {
    const { manager, sink } = fixture();
    manager.emitClosedTombstone(orphanSid);

    expect(sink.active.size).toBe(0);
    expect(sink.events).toContainEqual({
      kind: "closed",
      session_id: orphanSid,
      exit_code: null,
      ts: expect.any(Number),
    });
  });

  test("unknown kill at capacity returns the bounded failure and preserves the breadcrumb", () => {
    const { manager, sink } = fixture(0);
    const sent: unknown[] = [];
    handleKill(
      { kind: "kill", session_id: orphanSid },
      "request-1",
      {
        sessionMgr: manager,
        coordLink: { send: (frame: unknown) => sent.push(frame) } as never,
      },
    );

    expect(sink.events).toHaveLength(0);
    expect(sink.active.size).toBe(0);
    expect(sent).toEqual([{
      kind: "rpc-error",
      request_id: "request-1",
      message: "session lifecycle outbox full",
    }]);
  });

  test("an admitted close commits while unreserved capacity is exhausted", () => {
    const { manager, sink } = fixture(2);
    const heldClose = manager.reserveLifecycleEvent("closed");
    const capacityBlocker = manager.reserveLifecycleEvent("opened");
    expect(sink.active.size).toBe(2);

    manager.emitClosedTombstone(orphanSid, heldClose);

    expect(sink.events).toHaveLength(1);
    expect(sink.active.size).toBe(1);
    manager.releaseLifecycleEvent(capacityBlocker);
    expect(sink.active.size).toBe(0);
  });

  test("failed durable insertion releases the caller-owned tombstone reservation", () => {
    const { manager, sink } = fixture();
    sink.failNextEmit = true;

    expect(() => manager.emitClosedTombstone(orphanSid)).toThrow(
      "injected lifecycle append failure",
    );
    expect(sink.active.size).toBe(0);
    expect(sink.events).toHaveLength(0);
  });
});

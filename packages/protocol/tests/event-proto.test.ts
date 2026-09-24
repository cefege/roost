// Round-trip every SessionEvent variant through the shared protobuf adapter.
// Property cases cover public events; focused cases pin snapshots and private references.
// Event IDs stay transport metadata rather than part of the durable event value.

import { describe, it, expect } from "bun:test";
import fc from "fast-check";
import { eventToProto, protoToEvent } from "../src/wire/event-proto.ts";
import { SessionEvent, asSessionId, asWorkerFp, asWorkspaceId, asChannelId } from "../src/wire/index.ts";

const sidArb = fc.uuid().map(asSessionId);
const wfpArb = fc.hexaString({ minLength: 64, maxLength: 64 }).map(asWorkerFp);
const wsidArb = fc.uuid().map(asWorkspaceId);
const channelArb = fc.integer({ min: 0, max: 0xffffffff }).map(asChannelId);
const tsArb = fc.integer({ min: 1, max: 2 ** 50 });

function stripEventId<T extends Record<string, unknown>>(o: T): Omit<T, "_event_id"> {
  const { _event_id, ...rest } = o as { _event_id?: number } & Record<string, unknown>;
  return rest as Omit<T, "_event_id">;
}

describe("eventToProto round-trip", () => {
  it("opened", () => {
    fc.assert(fc.property(sidArb, wfpArb, channelArb, fc.constant("shell"), fc.string(), tsArb, (sid, wfp, ch, sk, cwd, ts) => {
      const e = SessionEvent.parse({ kind: "opened", session_id: sid, worker_fp: wfp, channel: ch, session_kind: sk, cwd, ts });
      const proto = eventToProto(e, 42);
      expect(proto).not.toBeNull();
      const back = protoToEvent(proto!) as Record<string, unknown> & { _event_id: number };
      expect(back._event_id).toBe(42);
      expect(stripEventId(back)).toEqual(e as unknown as Record<string, unknown>);
    }));
  });

  it("closed", () => {
    fc.assert(fc.property(sidArb, fc.option(fc.integer({ min: -128, max: 255 }), { nil: null }), tsArb, (sid, exit, ts) => {
      const e = SessionEvent.parse({ kind: "closed", session_id: sid, exit_code: exit, ts });
      const back = protoToEvent(eventToProto(e, 7)!) as Record<string, unknown> & { _event_id: number };
      expect(back._event_id).toBe(7);
      expect(stripEventId(back)).toEqual(e as unknown as Record<string, unknown>);
    }));
  });

  it("attached / detached", () => {
    fc.assert(fc.property(sidArb, tsArb, (sid, ts) => {
      for (const kind of ["attached", "detached"] as const) {
        const e = SessionEvent.parse({ kind, session_id: sid, ts });
        const back = protoToEvent(eventToProto(e, 1)!) as Record<string, unknown> & { _event_id: number };
        expect(stripEventId(back)).toEqual(e as unknown as Record<string, unknown>);
      }
    }));
  });

  it("cwd", () => {
    fc.assert(fc.property(sidArb, fc.string(), tsArb, (sid, cwd, ts) => {
      const e = SessionEvent.parse({ kind: "cwd", session_id: sid, cwd, ts });
      const back = protoToEvent(eventToProto(e, 1)!) as Record<string, unknown> & { _event_id: number };
      expect(stripEventId(back)).toEqual(e as unknown as Record<string, unknown>);
    }));
  });

  it("workspace_assigned", () => {
    fc.assert(fc.property(sidArb, fc.option(wsidArb, { nil: null }), tsArb, (sid, wsid, ts) => {
      const e = SessionEvent.parse({ kind: "workspace_assigned", session_id: sid, workspace_id: wsid, ts });
      const back = protoToEvent(eventToProto(e, 1)!) as Record<string, unknown> & { _event_id: number };
      expect(stripEventId(back)).toEqual(e as unknown as Record<string, unknown>);
    }));
  });


  it("snapshot — empty sessions array round-trips", () => {
    fc.assert(fc.property(wfpArb, tsArb, (wfp, ts) => {
      const e = SessionEvent.parse({ kind: "snapshot", worker_fp: wfp, sessions: [], ts });
      const back = protoToEvent(eventToProto(e, 1)!) as Record<string, unknown> & { _event_id: number };
      expect(stripEventId(back)).toEqual(e as unknown as Record<string, unknown>);
    }));
  });

  it("respawned — round-trips", () => {
    fc.assert(fc.property(sidArb, channelArb, tsArb, (sid, ch, ts) => {
      const e = SessionEvent.parse({ kind: "respawned", session_id: sid, new_channel: ch, ts });
      const back = protoToEvent(eventToProto(e, 99)!) as Record<string, unknown> & { _event_id: number };
      expect(back._event_id).toBe(99);
      expect(stripEventId(back)).toEqual(e as unknown as Record<string, unknown>);
    }));
  });

  it("snapshot — populated sessions array round-trips typed per-field", () => {
    const wfp = asWorkerFp("aa".repeat(32));
    const sessions = [
      {
        id: asSessionId("00000000-0000-4000-8000-000000000010"),
        worker_fp: wfp,
        channel: asChannelId(1),
        kind: "shell" as const,
        cwd: "/home/x",
        workspace_id: null,
        status: "open" as const,
        created_at: 1781500000,
        closed_at: null,
        custom_title: null,
      },
      {
        id: asSessionId("00000000-0000-4000-8000-000000000011"),
        worker_fp: wfp,
        channel: asChannelId(2),
        kind: "shell" as const,
        cwd: "/home/y",
        workspace_id: asWorkspaceId("00000000-0000-4000-8000-000000000100"),
        status: "open" as const,
        created_at: 1781500002,
        closed_at: null,
        custom_title: null,
      },
    ];
    const event = SessionEvent.parse({
      kind: "snapshot",
      worker_fp: wfp,
      sessions,
      ts: 1781500003,
    });
    const back = protoToEvent(eventToProto(event, 7));
    if (back?.kind !== "snapshot") throw new Error("expected snapshot round-trip");
    expect(back._event_id).toBe(7);
    expect(back.sessions.length).toBe(2);
    expect(back.sessions[0]).toEqual(sessions[0]);
    expect(back.sessions[1]).toEqual(sessions[1]);
  });

  it("git — branch + remote round-trip; absent remote stays absent", () => {
    const sid = asSessionId("00000000-0000-4000-8000-0000000000a0");
    const withRemote = SessionEvent.parse({ kind: "git", session_id: sid, branch: "n6/pr", remote: "owner/repo", ts: 1781500010 });
    const back1 = protoToEvent(eventToProto(withRemote, 1)!) as Record<string, unknown> & { _event_id: number };
    expect(stripEventId(back1)).toEqual(withRemote as unknown as Record<string, unknown>);
    const noRemote = SessionEvent.parse({ kind: "git", session_id: sid, branch: null, ts: 1781500011 });
    const back2 = protoToEvent(eventToProto(noRemote, 2)!) as Record<string, unknown>;
    expect("remote" in stripEventId(back2)).toBe(false);
  });

  it("pr — number/state/checks/url round-trip; null PR round-trips", () => {
    const sid = asSessionId("00000000-0000-4000-8000-0000000000b0");
    const open = SessionEvent.parse({
      kind: "pr", session_id: sid, number: 1481, state: "open", checks: "failing",
      url: "https://github.com/owner/repo/pull/1481", ts: 1781500020,
    });
    const back1 = protoToEvent(eventToProto(open, 3)!) as Record<string, unknown> & { _event_id: number };
    expect(back1._event_id).toBe(3);
    expect(stripEventId(back1)).toEqual(open as unknown as Record<string, unknown>);
    const none = SessionEvent.parse({ kind: "pr", session_id: sid, number: null, state: null, checks: null, url: null, ts: 1781500021 });
    const back2 = protoToEvent(eventToProto(none, 4)!) as Record<string, unknown>;
    expect(stripEventId(back2)).toEqual(none as unknown as Record<string, unknown>);
  });

  it("ports — port array round-trips; empty round-trips", () => {
    const sid = asSessionId("00000000-0000-4000-8000-0000000000c0");
    const some = SessionEvent.parse({ kind: "ports", session_id: sid, ports: [5174, 8765], ts: 1781500030 });
    const back1 = protoToEvent(eventToProto(some, 5)!) as Record<string, unknown> & { _event_id: number };
    expect(back1._event_id).toBe(5);
    expect(stripEventId(back1)).toEqual(some as unknown as Record<string, unknown>);
    const empty = SessionEvent.parse({ kind: "ports", session_id: sid, ports: [], ts: 1781500031 });
    const back2 = protoToEvent(eventToProto(empty, 6)!) as Record<string, unknown>;
    expect(stripEventId(back2)).toEqual(empty as unknown as Record<string, unknown>);
  });
  it("agent_reference — set and clear round-trip without interpreting value", () => {
    const sid = asSessionId("00000000-0000-4000-8000-0000000000d0");
    const events = [
      SessionEvent.parse({
        kind: "agent_reference",
        session_id: sid,
        reference: {
          schema_version: 1,
          agent_id: "omp",
          kind: "path",
          value: "/tmp/a path/'$opaque.json",
        },
        ts: 1781500040,
        trace_id: "0123456789abcdef",
      }),
      SessionEvent.parse({
        kind: "agent_reference",
        session_id: sid,
        reference: null,
        ts: 1781500041,
      }),
    ];
    for (const [index, event] of events.entries()) {
      const back = protoToEvent(eventToProto(event, index + 10));
      expect(back?._event_id).toBe(index + 10);
      expect(stripEventId(back as Record<string, unknown> & { _event_id: number }))
        .toEqual(event as unknown as Record<string, unknown>);
    }
  });
});

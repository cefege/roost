// T1.2 — round-trip test: eventToProto then protoToEvent should preserve
// the SessionEvent shape across all typed variants.

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

  it("agent — sparse patches round-trip per-field (kind is redundant; projector re-asserts it)", () => {
    fc.assert(fc.property(sidArb, tsArb, (sid, ts) => {
      const patch = { status: "running" as const, mode: "default" as const };
      const e = SessionEvent.parse({ kind: "agent", session_id: sid, patch, ts });
      const back = protoToEvent(eventToProto(e, 1)!) as any;
      expect(back.patch.status).toBe("running");
      expect(back.patch.mode).toBe("default");
      expect(back.session_id).toBe(sid);
      expect(back.ts).toBe(ts);
    }));
  });

  it("agent — current_tool:null clear survives round-trip", () => {
    const sid = asSessionId("00000000-0000-4000-8000-000000000001");
    const e = SessionEvent.parse({
      kind: "agent", session_id: sid,
      patch: { kind: "agent", current_tool: null }, ts: 1,
    });
    const back = protoToEvent(eventToProto(e, 1)!) as any;
    expect(back.patch.current_tool).toBeNull();
  });

  it("agent — current_tool:{value} set survives round-trip", () => {
    const sid = asSessionId("00000000-0000-4000-8000-000000000002");
    const e = SessionEvent.parse({
      kind: "agent", session_id: sid,
      patch: { kind: "agent", current_tool: { name: "Read", input_summary: "/etc/hosts" } }, ts: 1,
    });
    const back = protoToEvent(eventToProto(e, 1)!) as any;
    expect(back.patch.current_tool).toEqual({ name: "Read", input_summary: "/etc/hosts" });
  });

  it("agent — absent current_tool stays absent (not cleared)", () => {
    const sid = asSessionId("00000000-0000-4000-8000-000000000003");
    const e = SessionEvent.parse({
      kind: "agent", session_id: sid,
      patch: { kind: "agent", status: "running" }, ts: 1,
    });
    const back = protoToEvent(eventToProto(e, 1)!) as any;
    expect("current_tool" in back.patch).toBe(false);
  });

  it("agent — last_message:null clear survives round-trip (D-2)", () => {
    const sid = asSessionId("00000000-0000-4000-8000-000000000020");
    const e = SessionEvent.parse({
      kind: "agent", session_id: sid,
      patch: { kind: "agent", last_message: null }, ts: 1,
    });
    const back = protoToEvent(eventToProto(e, 1)!) as any;
    expect(back.patch.last_message).toBeNull();
  });

  it("agent — current_block:null clear survives round-trip (D-2)", () => {
    const sid = asSessionId("00000000-0000-4000-8000-000000000021");
    const e = SessionEvent.parse({
      kind: "agent", session_id: sid,
      patch: { kind: "agent", current_block: null }, ts: 1,
    });
    const back = protoToEvent(eventToProto(e, 1)!) as any;
    expect(back.patch.current_block).toBeNull();
  });

  it("agent — permission_request:null clear survives round-trip (D-2)", () => {
    const sid = asSessionId("00000000-0000-4000-8000-000000000022");
    const e = SessionEvent.parse({
      kind: "agent", session_id: sid,
      patch: { kind: "agent", permission_request: null }, ts: 1,
    });
    const back = protoToEvent(eventToProto(e, 1)!) as any;
    expect(back.patch.permission_request).toBeNull();
  });

  it("agent — multi-field patch with tokens + last_message + cost", () => {
    const sid = asSessionId("00000000-0000-4000-8000-000000000004");
    const e = SessionEvent.parse({
      kind: "agent", session_id: sid,
      patch: {
        kind: "agent", status: "idle", cost_usd: 0.42,
        tokens: { in: 1000, out: 500, cached: 200 },
        last_message: { role: "assistant", text: "done", ts: 1781500000 },
      },
      ts: 1,
    });
    const back = protoToEvent(eventToProto(e, 1)!) as any;
    expect(back.patch.status).toBe("idle");
    expect(back.patch.cost_usd).toBe(0.42);
    expect(back.patch.tokens).toEqual({ in: 1000, out: 500, cached: 200 });
    expect(back.patch.last_message).toEqual({ role: "assistant", text: "done", ts: 1781500000 });
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
        agent: null,
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
        agent: {
          kind: "agent" as const, mode: "default" as const, model: "adapter", status: "running" as const,
          tokens: { in: 12, out: 7, cached: 3 }, cost_usd: 0.05,
          last_message: { role: "assistant" as const, text: "hi", ts: 1781500001 },
          current_tool: { name: "Read", input_summary: "/etc/hosts" },
          current_block: null, permission_request: null, sub_agents: [],
        },
        created_at: 1781500002,
        closed_at: null,
        custom_title: null,
      },
    ];
    const e = SessionEvent.parse({ kind: "snapshot", worker_fp: wfp, sessions, ts: 1781500003 });
    const back = protoToEvent(eventToProto(e, 7)!) as any;
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
});

// R-2 — worker→coord event frame is now typed SessionEventProto. This
// test verifies the eventToProto → wire → protoToEvent round-trip
// matches the path the worker-WS handler runs end-to-end.

import { describe, it, expect } from "bun:test";
import { create, fromBinary, toBinary } from "@bufbuild/protobuf";
import { CoordWorkerUpSchema, WSessionEventSchema, type CoordWorkerUp } from "@roost/shared/proto/worker_transport_pb";
import { eventToProto, protoToEvent } from "@roost/shared/wire/event-proto";
import { SessionEvent, asSessionId, asWorkerFp, asChannelId } from "@roost/shared/wire";

function roundTripFrame(frame: CoordWorkerUp): CoordWorkerUp {
  const bytes = toBinary(CoordWorkerUpSchema, frame);
  return fromBinary(CoordWorkerUpSchema, bytes);
}

function wrapEvent(proto: NonNullable<ReturnType<typeof eventToProto>>, clientSeq = 0) {
  return create(WSessionEventSchema, { event: proto, clientSeq: BigInt(clientSeq) });
}

describe("worker→coord event frame (R-2 typed wire)", () => {
  it("opened event survives proto encode/decode", () => {
    const sid = asSessionId("00000000-0000-4000-8000-000000000001");
    const ev = SessionEvent.parse({
      kind: "opened",
      session_id: sid,
      worker_fp: asWorkerFp("aa".repeat(32)),
      channel: asChannelId(1),
      session_kind: "shell",
      cwd: "/tmp",
      ts: 1781500000,
    });
    const proto = eventToProto(ev, 0);
    expect(proto).not.toBeNull();
    const frame = create(CoordWorkerUpSchema, { frame: { case: "event", value: wrapEvent(proto!) }});
    const wireBack = roundTripFrame(frame);
    expect(wireBack.frame.case).toBe("event");
    const decoded = protoToEvent((wireBack.frame.value as any).event) as any;
    expect(decoded.kind).toBe("opened");
    expect(decoded.session_id).toBe(sid);
    expect(decoded.cwd).toBe("/tmp");
  });

  it("agent event with current_tool:null survives proto encode/decode", () => {
    const ev = SessionEvent.parse({
      kind: "agent",
      session_id: asSessionId("00000000-0000-4000-8000-000000000002"),
      patch: { kind: "claude", current_tool: null, status: "idle" },
      ts: 1781500001,
    });
    const proto = eventToProto(ev, 0);
    const frame = create(CoordWorkerUpSchema, { frame: { case: "event", value: wrapEvent(proto!) }});
    const wireBack = roundTripFrame(frame);
    const decoded = protoToEvent((wireBack.frame.value as any).event) as any;
    expect(decoded.kind).toBe("agent");
    expect(decoded.patch.current_tool).toBeNull();
    expect(decoded.patch.status).toBe("idle");
  });

  it("snapshot event with populated sessions survives proto encode/decode", () => {
    const wfp = asWorkerFp("bb".repeat(32));
    const ev = SessionEvent.parse({
      kind: "snapshot",
      worker_fp: wfp,
      sessions: [{
        id: asSessionId("00000000-0000-4000-8000-000000000003"),
        worker_fp: wfp,
        channel: asChannelId(2),
        kind: "shell",
        cwd: "/home/x",
        workspace_id: null,
        status: "open",
        agent: null,
        created_at: 1781500002,
        closed_at: null,
        custom_title: null,
      }],
      ts: 1781500003,
    });
    const proto = eventToProto(ev, 0);
    const frame = create(CoordWorkerUpSchema, { frame: { case: "event", value: wrapEvent(proto!) }});
    const wireBack = roundTripFrame(frame);
    const decoded = protoToEvent((wireBack.frame.value as any).event) as any;
    expect(decoded.kind).toBe("snapshot");
    expect(decoded.sessions.length).toBe(1);
    expect(decoded.sessions[0].cwd).toBe("/home/x");
  });
});

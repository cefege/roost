// Session schema and protobuf ingress accept terminal sessions only.

import { describe, it, expect } from "bun:test";
import { sessionFromProto, sessionToProto } from "../src/wire/session-proto.ts";
import { Session, asSessionId, asWorkerFp, asChannelId } from "../src/wire/index.ts";

const base = {
  id: asSessionId("00000000-0000-4000-8000-000000000001"),
  worker_fp: asWorkerFp("aa".repeat(32)),
  channel: asChannelId(1),
  cwd: "/tmp",
  workspace_id: null,
  status: "open" as const,
  created_at: 1,
  closed_at: null,
  custom_title: null,
};

describe("Session kind", () => {
  it("accepts shell terminals", () => {
    expect(Session.parse({ ...base, kind: "shell" })).toEqual({ ...base, kind: "shell" });
  });

  it("rejects structured and unsupported session kinds", () => {
    expect(Session.safeParse({ ...base, kind: "agent" }).success).toBe(false);
    expect(Session.safeParse({ ...base, kind: "claude" }).success).toBe(false);
    expect(Session.safeParse({ ...base, kind: "unsupported" }).success).toBe(false);
  });
});

describe("Session protobuf ingress", () => {
  it("rejects a non-shell kind at the protobuf boundary", () => {
    const shell = { ...base, kind: "shell" as const };
    const proto = sessionToProto(shell);
    expect(sessionFromProto(proto)).toEqual(shell);
    proto.kind = "agent";
    expect(() => sessionFromProto(proto)).toThrow(/kind/);
  });
});

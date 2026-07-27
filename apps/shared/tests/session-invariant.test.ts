// Session schema accepts shell terminals with optional adapter state.

import { describe, it, expect } from "bun:test";
import { sessionFromProto, sessionToProto } from "../src/wire/agent-proto.ts";
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
const agent = {
  kind: "agent" as const, mode: "default" as const, model: "adapter", status: "running" as const,
  tokens: { in: 0, out: 0, cached: 0 }, cost_usd: 0,
  last_message: null, current_tool: null, current_block: null,
  permission_request: null, sub_agents: [],
};

describe("Session kind and adapter state", () => {
  it("accepts shell without adapter state", () => {
    expect(() => Session.parse({ ...base, kind: "shell", agent: null })).not.toThrow();
  });
  it("accepts shell with adapter state", () => {
    expect(() => Session.parse({ ...base, kind: "shell", agent })).not.toThrow();
  });
  it("rejects retired and unsupported session kinds", () => {
    expect(Session.safeParse({ ...base, kind: "claude", agent: null }).success).toBe(false);
    expect(Session.safeParse({ ...base, kind: "unsupported", agent: null }).success).toBe(false);
  });
});

describe("Session protobuf ingress", () => {
  it("rejects a retired kind at the protobuf boundary", () => {
    const shell = { ...base, kind: "shell" as const, agent: null };
    const proto = sessionToProto(shell);
    expect(sessionFromProto(proto)).toEqual(shell);
    proto.kind = "claude";
    expect(() => sessionFromProto(proto)).toThrow(/kind/);
  });
});

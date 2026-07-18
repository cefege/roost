// D-3: shell-implies-agent-null invariant on Session schema.

import { describe, it, expect } from "bun:test";
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
const claudeAgent = {
  kind: "claude" as const, mode: "default" as const, model: "x", status: "running" as const,
  tokens: { in: 0, out: 0, cached: 0 }, cost_usd: 0,
  last_message: null, current_tool: null, current_block: null,
  permission_request: null, sub_agents: [],
};

describe("Session.kind/agent invariant", () => {
  it("accepts shell + agent:null", () => {
    expect(() => Session.parse({ ...base, kind: "shell", agent: null })).not.toThrow();
  });
  it("accepts claude + agent:null (brief window before first agent event)", () => {
    expect(() => Session.parse({ ...base, kind: "claude", agent: null })).not.toThrow();
  });
  it("accepts claude + agent:populated", () => {
    expect(() => Session.parse({ ...base, kind: "claude", agent: claudeAgent })).not.toThrow();
  });
  it("rejects shell + agent:populated", () => {
    const r = Session.safeParse({ ...base, kind: "shell", agent: claudeAgent });
    expect(r.success).toBe(false);
    if (!r.success) {
      expect(r.error.issues[0].message).toMatch(/shell session must have agent:null/);
    }
  });
});

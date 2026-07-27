import { describe, expect, test } from "bun:test";
import { AgentState, Session } from "@roost/shared/wire";
import { isAgentSession } from "../src/lib/isAgentSession.ts";

const sessionBase = {
  id: "00000000-0000-4000-8000-000000000001",
  worker_fp: "a".repeat(64),
  channel: 1,
  kind: "shell",
  cwd: "/tmp",
  workspace_id: null,
  status: "open",
  created_at: 1,
  closed_at: null,
  custom_title: null,
};

describe("isAgentSession", () => {
  test("classifies an open OMP agent session by structured state", () => {
    const session = Session.parse({
      ...sessionBase,
      agent: AgentState.parse({
        kind: "agent",
        mode: "default",
        model: "",
        status: "running",
        tokens: { in: 0, out: 0, cached: 0 },
        cost_usd: 0,
        last_message: null,
        current_tool: null,
        current_block: null,
        permission_request: null,
        sub_agents: [],
      }),
    });

    expect(isAgentSession(session)).toBe(true);
  });

  test("does not classify a plain shell as an agent session", () => {
    expect(isAgentSession(Session.parse({ ...sessionBase, agent: null }))).toBe(false);
  });
});

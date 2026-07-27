import { expect, test } from "bun:test";
import { asChannelId, asSessionId, asWorkerFp } from "@roost/shared/wire";
import type { Session } from "@roost/shared/wire";
import { latestAssistantOutput, liveStatus } from "../src/lib/attention.ts";

const session: Session = {
  id: asSessionId("00000000-0000-4000-8000-000000000001"),
  worker_fp: asWorkerFp("aa".repeat(32)),
  channel: asChannelId(1),
  kind: "shell",
  cwd: "/repo",
  spawn_cwd: "/repo",
  workspace_id: null,
  status: "open",
  created_at: 1_000,
  closed_at: null,
  custom_title: null,
  agent: {
    kind: "agent",
    mode: "default",
    model: "test",
    status: "idle",
    tokens: { in: 0, out: 0, cached: 0 },
    cost_usd: 0,
    last_message: { role: "assistant", text: "Finished", ts: 2_000 },
    current_tool: null,
    current_block: null,
    permission_request: null,
    sub_agents: [],
  },
};

test("projects structured agent state for status and assistant output", () => {
  expect(liveStatus(session)).toBe("idle");
  expect(latestAssistantOutput(session)).toEqual({ role: "assistant", text: "Finished", ts: 2_000 });
  expect(liveStatus({ ...session, agent: null })).toBeUndefined();
  expect(latestAssistantOutput({
    ...session,
    agent: { ...session.agent!, last_message: { role: "user", text: "Continue", ts: 2_001 } },
  })).toBeNull();
});

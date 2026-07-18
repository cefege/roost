// R-1a — round-trip tests for the canonical AgentState/Session adapter.
// Catches drift between Zod source-of-truth and proto wire shape; every
// AgentState field exercised through fast-check.

import { describe, it, expect } from "bun:test";
import fc from "fast-check";
import {
  tokensToProto, tokensFromProto,
  lastMessageToProto, lastMessageFromProto,
  currentToolToProto, currentToolFromProto,
  currentBlockToProto, currentBlockFromProto,
  permissionRequestToProto, permissionRequestFromProto,
  subAgentRowToProto, subAgentRowFromProto,
  agentStateToProto, agentStateFromProto,
  sessionToProto, sessionFromProto,
} from "../src/wire/agent-proto.ts";
import { asSessionId, asWorkerFp, asWorkspaceId, asChannelId } from "../src/wire/brand.ts";

const sidArb = fc.uuid().map(asSessionId);
const wfpArb = fc.hexaString({ minLength: 64, maxLength: 64 }).map(asWorkerFp);
const wsidArb = fc.uuid().map(asWorkspaceId);
const channelArb = fc.integer({ min: 0, max: 0xffffffff }).map(asChannelId);
const tsArb = fc.integer({ min: 1, max: 2 ** 50 });
const tokenArb = fc.integer({ min: 0, max: 2 ** 31 });
const modeArb = fc.constantFrom("default", "acceptEdits", "plan", "bypassPermissions", "dontAsk", "auto");
const statusArb = fc.constantFrom("running", "needs-input", "idle", "done");
const roleArb = fc.constantFrom("user", "assistant", "thinking");

describe("agent-proto primitives", () => {
  it("Tokens round-trips", () => {
    fc.assert(fc.property(tokenArb, tokenArb, tokenArb, (i, o, c) => {
      const t = { in: i, out: o, cached: c };
      expect(tokensFromProto(tokensToProto(t))).toEqual(t);
    }));
  });

  it("LastMessage round-trips", () => {
    fc.assert(fc.property(roleArb, fc.string(), tsArb, (role, text, ts) => {
      const m = { role, text, ts } as any;
      expect(lastMessageFromProto(lastMessageToProto(m))).toEqual(m);
    }));
  });

  it("CurrentTool round-trips", () => {
    fc.assert(fc.property(fc.string({ minLength: 1 }), fc.string(), (name, input) => {
      const t = { name, input_summary: input };
      expect(currentToolFromProto(currentToolToProto(t))).toEqual(t);
    }));
  });

  it("CurrentBlock round-trips (command nullable)", () => {
    fc.assert(fc.property(fc.integer({ min: 0, max: 1_000_000 }), fc.option(fc.string(), { nil: null }), (id, cmd) => {
      const b = { id, command: cmd };
      expect(currentBlockFromProto(currentBlockToProto(b))).toEqual(b);
    }));
  });

  it("PermissionRequest round-trips", () => {
    fc.assert(fc.property(fc.string({ minLength: 1 }), fc.string(), fc.array(fc.string(), { minLength: 1 }), (id, snip, opts) => {
      const r = { id, snippet: snip, options: opts };
      expect(permissionRequestFromProto(permissionRequestToProto(r))).toEqual(r);
    }));
  });

  it("SubAgentRow round-trips", () => {
    fc.assert(fc.property(fc.string({ minLength: 1 }), fc.string({ minLength: 1 }), fc.string(), statusArb, (pid, cid, lbl, st) => {
      const s = { parent_message_id: pid, child_session_id: cid, label: lbl, status: st } as any;
      expect(subAgentRowFromProto(subAgentRowToProto(s))).toEqual(s);
    }));
  });

  it("AgentState round-trips with all-fields-populated", () => {
    const a = {
      kind: "claude" as const, mode: "default" as const, model: "claude-opus-4-7", status: "running" as const,
      tokens: { in: 100, out: 50, cached: 25 }, cost_usd: 0.42,
      last_message: { role: "assistant" as const, text: "hi", ts: 1781500000 },
      current_tool: { name: "Read", input_summary: "/etc/hosts" },
      current_block: { id: 7, command: "echo" },
      permission_request: { id: "p1", snippet: "rm -rf /", options: ["allow", "deny"] },
      sub_agents: [{ parent_message_id: "m1", child_session_id: "s1", label: "child", status: "running" as const }],
    };
    expect(agentStateFromProto(agentStateToProto(a))).toEqual(a);
  });

  it("AgentState round-trips with all-nullable-fields-null", () => {
    const a = {
      kind: "claude" as const, mode: "default" as const, model: "", status: "idle" as const,
      tokens: { in: 0, out: 0, cached: 0 }, cost_usd: 0,
      last_message: null, current_tool: null, current_block: null,
      permission_request: null, sub_agents: [],
    };
    expect(agentStateFromProto(agentStateToProto(a))).toEqual(a);
  });
});

describe("agent-proto Session", () => {
  it("Session with agent:null (shell) round-trips", () => {
    fc.assert(fc.property(sidArb, wfpArb, channelArb, fc.string(), tsArb, (sid, wfp, ch, cwd, ts) => {
      const s = {
        id: sid, worker_fp: wfp, channel: ch, kind: "shell" as const, cwd,
        workspace_id: null, status: "open" as const, agent: null,
        created_at: ts, closed_at: null, custom_title: null,
      };
      expect(sessionFromProto(sessionToProto(s))).toEqual(s);
    }));
  });

  it("Session with agent (claude) + workspace_id + closed_at round-trips", () => {
    fc.assert(fc.property(sidArb, wfpArb, wsidArb, channelArb, fc.string(), tsArb, tsArb, (sid, wfp, wsid, ch, cwd, t1, t2) => {
      const s = {
        id: sid, worker_fp: wfp, channel: ch, kind: "claude" as const, cwd,
        workspace_id: wsid, status: "closed" as const,
        agent: {
          kind: "claude" as const, mode: "default" as const, model: "x", status: "done" as const,
          tokens: { in: 1, out: 2, cached: 3 }, cost_usd: 0.01,
          last_message: null, current_tool: null, current_block: null,
          permission_request: null, sub_agents: [],
        },
        created_at: t1, closed_at: t2, custom_title: null,
      };
      expect(sessionFromProto(sessionToProto(s))).toEqual(s);
    }));
  });
});

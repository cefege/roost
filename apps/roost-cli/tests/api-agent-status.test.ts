// Pins the public agent-status CLI projection and deterministic human output.
// Exercises the focused dispatcher with protobuf messages and an injected writer.
// No coordinator process or credential enrollment is involved.

import { Code, ConnectError } from "@connectrpc/connect";
import { create } from "@bufbuild/protobuf";
import { describe, expect, test } from "bun:test";
import {
  AgentStatusViewSchema,
  type AgentStatusView,
} from "@roost/shared/proto/coordinator_pb";
import {
  dispatchAgentStatusApi,
  type AgentStatusApiClient,
} from "../src/api-agent-status.ts";

const IDENTIFIED = create(AgentStatusViewSchema, {
  sessionId: "session-b",
  agentId: "omp",
  state: "blocked",
  message: "Approval required",
  revision: 12n,
  completedRevision: 7n,
  updatedAt: 1_725_000_000_123,
  active: true,
  statusEpoch: "epoch-b",
  occupantId: "occupant-b",
  source: "integration",
  promptable: true,
});

const SCREEN = create(AgentStatusViewSchema, {
  sessionId: "session-a",
  agentId: "codex",
  state: "working",
  message: "waiting\tfor\ninput",
  revision: 8n,
  completedRevision: 3n,
  updatedAt: 1_725_000_000_100,
  active: true,
  statusEpoch: "epoch-a",
  occupantId: "occupant-a",
  source: "screen",
  promptable: false,
});

const LEGACY = create(AgentStatusViewSchema, {
  sessionId: "session-z",
  agentId: "pi",
  state: "idle",
  revision: 2n,
  completedRevision: 2n,
  updatedAt: 1_725_000_000_200,
  active: true,
  promptable: false,
});

const CONTROL_MESSAGE = "\u001b[31mring\u0007\u007f\u009b31m café 漢字";
const CONTROL_MESSAGE_STATUS = create(AgentStatusViewSchema, {
  sessionId: "session-controls",
  agentId: "omp",
  state: "blocked",
  message: CONTROL_MESSAGE,
  revision: 13n,
  completedRevision: 7n,
  updatedAt: 1_725_000_000_300,
  active: true,
  statusEpoch: "epoch-controls",
  occupantId: "occupant-controls",
  source: "integration",
  promptable: true,
});

function fakeClient(statuses: AgentStatusView[], calls: string[] = []): AgentStatusApiClient {
  return {
    async agentStatusGet({ sessionId }) {
      calls.push(`get:${sessionId}`);
      return { status: statuses.find((status) => status.sessionId === sessionId) };
    },
    async agentStatusList() {
      calls.push("list");
      return { statuses };
    },
    async agentStatusWait() {
      calls.push("wait");
      return { outcome: "matched" };
    },
  };
}

async function capture(
  client: AgentStatusApiClient,
  verb: string,
  args: string[],
): Promise<{ handled: boolean; output: string }> {
  const lines: string[] = [];
  const handled = await dispatchAgentStatusApi(client, verb, args, (line) => lines.push(line));
  return { handled, output: lines.join("\n") };
}

describe("agent status API CLI", () => {
  test("prints one identified integration status as deterministic TSV", async () => {
    const calls: string[] = [];
    const result = await capture(fakeClient([IDENTIFIED], calls), "agent-status", ["session-b"]);

    expect(result).toEqual({
      handled: true,
      output: [
        "session_id\tagent_id\tstate\tmessage\tstatus_epoch\toccupant_id\tsource\trevision\tcompleted_revision\tupdated_at\tpromptable",
        "session-b\tomp\tblocked\tApproval required\tepoch-b\toccupant-b\tintegration\t12\t7\t1725000000123\ttrue",
      ].join("\n"),
    });
    expect(calls).toEqual(["get:session-b"]);
  });

  test("escapes terminal controls while preserving printable Unicode in TSV", async () => {
    const { output } = await capture(
      fakeClient([CONTROL_MESSAGE_STATUS]),
      "agent-status",
      ["session-controls"],
    );

    const lines = output.split("\n");
    expect(lines[1]).toBe(
      "session-controls\tomp\tblocked\t\\x1b[31mring\\x07\\x7f\\x9b31m café 漢字\tepoch-controls\toccupant-controls\tintegration\t13\t7\t1725000000300\ttrue",
    );
    const messageCell = lines[1]?.split("\t")[3];
    expect(messageCell).not.toMatch(/[\x00-\x1f\x7f-\x9f]/);
  });

  test("preserves JSON.stringify output for control-bearing messages", async () => {
    const { output } = await capture(
      fakeClient([CONTROL_MESSAGE_STATUS]),
      "agent-status",
      ["session-controls", "--json"],
    );

    expect(output).toBe(JSON.stringify({
      session_id: "session-controls",
      agent_id: "omp",
      state: "blocked",
      message: CONTROL_MESSAGE,
      status_epoch: "epoch-controls",
      occupant_id: "occupant-controls",
      source: "integration",
      revision: 13,
      completed_revision: 7,
      updated_at: 1_725_000_000_300,
      promptable: true,
    }, null, 2));
  });

  test("prints an exact legacy JSON projection with numbers and null identity", async () => {
    const { output } = await capture(fakeClient([LEGACY]), "agent-status", ["session-z", "--json"]);

    expect(output).toBe(`{
  "session_id": "session-z",
  "agent_id": "pi",
  "state": "idle",
  "message": null,
  "status_epoch": null,
  "occupant_id": null,
  "source": null,
  "revision": 2,
  "completed_revision": 2,
  "updated_at": 1725000000200,
  "promptable": false
}`);
    const parsed = JSON.parse(output) as Record<string, unknown>;
    expect(Object.keys(parsed)).toEqual([
      "session_id",
      "agent_id",
      "state",
      "message",
      "status_epoch",
      "occupant_id",
      "source",
      "revision",
      "completed_revision",
      "updated_at",
      "promptable",
    ]);
    expect(typeof parsed.revision).toBe("number");
    expect(typeof parsed.completed_revision).toBe("number");
    expect(typeof parsed.updated_at).toBe("number");
  });

  test("sorts list output by session id and marks screen and legacy rows unpromptable", async () => {
    const result = await capture(fakeClient([LEGACY, SCREEN]), "agents", []);

    expect(result.output).toBe([
      "session_id\tagent_id\tstate\tmessage\tstatus_epoch\toccupant_id\tsource\trevision\tcompleted_revision\tupdated_at\tpromptable",
      "session-a\tcodex\tworking\twaiting\\tfor\\ninput\tepoch-a\toccupant-a\tscreen\t8\t3\t1725000000100\tfalse",
      "session-z\tpi\tidle\t-\t-\t-\tlegacy\t2\t2\t1725000000200\tfalse",
    ].join("\n"));
  });

  test("keeps list JSON sorted and explicitly projected", async () => {
    const { output } = await capture(fakeClient([LEGACY, SCREEN]), "agents", ["--json"]);

    expect(JSON.parse(output)).toEqual([
      {
        session_id: "session-a",
        agent_id: "codex",
        state: "working",
        message: "waiting\tfor\ninput",
        status_epoch: "epoch-a",
        occupant_id: "occupant-a",
        source: "screen",
        revision: 8,
        completed_revision: 3,
        updated_at: 1_725_000_000_100,
        promptable: false,
      },
      {
        session_id: "session-z",
        agent_id: "pi",
        state: "idle",
        message: null,
        status_epoch: null,
        occupant_id: null,
        source: null,
        revision: 2,
        completed_revision: 2,
        updated_at: 1_725_000_000_200,
        promptable: false,
      },
    ]);
  });

  test("returns unknown verbs to the existing dispatcher", async () => {
    const result = await capture(fakeClient([]), "sessions", []);
    expect(result).toEqual({ handled: false, output: "" });
  });

  test("rejects a missing session before issuing an RPC", async () => {
    const calls: string[] = [];
    await expect(dispatchAgentStatusApi(fakeClient([], calls), "agent-status", ["--json"]))
      .rejects.toThrow("agent-status: missing <session>");
    expect(calls).toEqual([]);
  });

  test("does not translate coordinator NotFound failures", async () => {
    const notFound = new ConnectError("session not found", Code.NotFound);
    const client = fakeClient([]);
    client.agentStatusGet = async () => { throw notFound; };

    await expect(dispatchAgentStatusApi(client, "agent-status", ["missing-session"]))
      .rejects.toBe(notFound);
  });
});

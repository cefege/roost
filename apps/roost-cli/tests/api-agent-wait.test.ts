// Agent-wait CLI tests pin conservative argument parsing, exact occupant lookup,
// terminal outcome output, and exit status without mutating process-global state.
// The focused dispatcher uses an injected client, line writer, and exit writer.

import { create } from "@bufbuild/protobuf";
import { describe, expect, test } from "bun:test";
import { AgentStatusViewSchema } from "@roost/shared/proto/coordinator_pb";
import {
  dispatchAgentStatusApi,
  type AgentStatusApiClient,
} from "../src/api-agent-status.ts";

const STATUS = create(AgentStatusViewSchema, {
  sessionId: "80000000-0000-4000-8000-000000000001",
  agentId: "omp",
  state: "working",
  revision: 4n,
  completedRevision: 1n,
  updatedAt: 1_800_000_000_000,
  active: true,
  statusEpoch: "90000000-0000-4000-8000-000000000001",
  occupantId: "a0000000-0000-4000-8000-000000000001",
  source: "integration",
  promptable: true,
});

interface CallRecord {
  method: "get" | "wait";
  request: unknown;
}

function fakeClient(outcome = "matched", status = STATUS): {
  client: AgentStatusApiClient;
  calls: CallRecord[];
} {
  const calls: CallRecord[] = [];
  return {
    calls,
    client: {
      async agentStatusGet(request) {
        calls.push({ method: "get", request });
        return { status };
      },
      async agentStatusList() {
        return { statuses: status ? [status] : [] };
      },
      async agentStatusWait(request) {
        calls.push({ method: "wait", request });
        return { outcome };
      },
    },
  };
}

async function dispatch(
  client: AgentStatusApiClient,
  args: string[],
): Promise<{ output: string[]; exits: number[] }> {
  const output: string[] = [];
  const exits: number[] = [];
  const handled = await dispatchAgentStatusApi(
    client,
    "agent-wait",
    args,
    (line) => output.push(line),
    (code) => exits.push(code),
  );
  expect(handled).toBe(true);
  return { output, exits };
}

describe("agent-wait API CLI", () => {
  test("gets first, pins the exact occupant, and prints a matched outcome", async () => {
    const { client, calls } = fakeClient();
    const result = await dispatch(client, [
      STATUS.sessionId,
      "--until",
      "blocked,idle",
      "--timeout",
      "30s",
    ]);

    expect(calls).toEqual([
      { method: "get", request: { sessionId: STATUS.sessionId } },
      {
        method: "wait",
        request: {
          sessionId: STATUS.sessionId,
          statusEpoch: STATUS.statusEpoch,
          occupantId: STATUS.occupantId,
          desiredStates: ["blocked", "idle"],
          timeoutMs: 30_000,
        },
      },
    ]);
    expect(result).toEqual({ output: ["matched"], exits: [] });
  });

  test("accepts only integral millisecond, second, and minute durations through five minutes", async () => {
    for (const [duration, timeoutMs] of [
      ["1ms", 1],
      ["300s", 300_000],
      ["5m", 300_000],
    ] as const) {
      const { client, calls } = fakeClient();
      await dispatch(client, [
        STATUS.sessionId,
        "--until=working",
        `--timeout=${duration}`,
      ]);
      expect(calls[1]?.request).toMatchObject({ timeoutMs });
    }
  });

  test("prints every non-match outcome and assigns a nonzero exit", async () => {
    for (const outcome of ["timed_out", "occupant_changed", "session_closed"]) {
      const { client } = fakeClient(outcome);
      await expect(dispatch(client, [
        STATUS.sessionId,
        "--until",
        "idle",
        "--timeout",
        "1m",
      ])).resolves.toEqual({ output: [outcome], exits: [1] });
    }
  });

  test("rejects missing, duplicate, invalid, and over-cap options before lookup", async () => {
    for (const args of [
      [] as string[],
      [STATUS.sessionId, "--timeout", "1s"],
      [STATUS.sessionId, "--until", "idle"],
      [STATUS.sessionId, "--until", "idle,idle", "--timeout", "1s"],
      [STATUS.sessionId, "--until", "done", "--timeout", "1s"],
      [STATUS.sessionId, "--until", "idle", "--timeout", "0ms"],
      [STATUS.sessionId, "--until", "idle", "--timeout", "300001ms"],
      [STATUS.sessionId, "--until", "idle", "--timeout", "1.5s"],
      [STATUS.sessionId, "--until", "idle", "--timeout", "1s", "extra"],
    ]) {
      const { client, calls } = fakeClient();
      await expect(dispatch(client, args)).rejects.toThrow(/agent-wait:/);
      expect(calls).toEqual([]);
    }
  });

  test("refuses an identityless current row without invoking wait", async () => {
    const legacy = create(AgentStatusViewSchema, {
      sessionId: STATUS.sessionId,
      agentId: "omp",
      state: "idle",
      revision: 1n,
      completedRevision: 1n,
      updatedAt: 1,
      active: true,
      promptable: false,
    });
    const { client, calls } = fakeClient("matched", legacy);
    await expect(dispatch(client, [
      STATUS.sessionId,
      "--until",
      "idle",
      "--timeout",
      "1s",
    ])).rejects.toThrow("agent-wait: current agent status has no occupant identity");
    expect(calls.map((call) => call.method)).toEqual(["get"]);
  });

  test("rejects an unknown coordinator outcome without assigning an exit code", async () => {
    const { client } = fakeClient("unknown");
    await expect(dispatch(client, [
      STATUS.sessionId,
      "--until",
      "idle",
      "--timeout",
      "1s",
    ])).rejects.toThrow("agent-wait: coordinator returned an invalid outcome");
  });
});

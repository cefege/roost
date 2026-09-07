// Agent-prompt CLI tests pin strict argv parsing, status-fence lookup order,
// stable result lines, and conservative exit behavior without process globals.
// An injected client proves invalid input never reaches the prompt mutation.

import { create } from "@bufbuild/protobuf";
import { describe, expect, test } from "bun:test";
import {
  AgentPromptInputOutcome,
  AgentPromptRejection,
  AgentPromptWaitOutcome,
  AgentStatusViewSchema,
  type AgentStatusView,
} from "@roost/shared/proto/coordinator_pb";
import { AGENT_PROMPT_MAX_WRITE_BYTES } from "@roost/shared/terminal-input";
import {
  AGENT_PROMPT_API_COMMAND,
  dispatchAgentPromptApi,
  parseAgentPromptArgs,
  type AgentPromptApiClient,
} from "../src/api-agent-prompt.ts";

const SESSION_ID = "80000000-0000-4000-8000-000000000001";
const STATUS = create(AgentStatusViewSchema, {
  sessionId: SESSION_ID,
  agentId: "omp",
  state: "working",
  message: "private status detail",
  revision: 23n,
  completedRevision: 18n,
  updatedAt: 1_800_000_000_000,
  active: true,
  statusEpoch: "90000000-0000-4000-8000-000000000001",
  occupantId: "a0000000-0000-4000-8000-000000000001",
  source: "integration",
  promptable: true,
});

interface FakeOptions {
  status?: AgentStatusView | null;
  inputOutcome?: AgentPromptInputOutcome;
  writtenBytes?: number;
  reason?: string;
  waitOutcome?: AgentPromptWaitOutcome;
  rejection?: AgentPromptRejection;
}

interface CallRecord {
  method: "get" | "prompt";
  request: unknown;
}

function fakeClient(options: FakeOptions = {}): {
  client: AgentPromptApiClient;
  calls: CallRecord[];
} {
  const calls: CallRecord[] = [];
  return {
    calls,
    client: {
      async agentStatusGet(request) {
        calls.push({ method: "get", request });
        return options.status === null ? {} : { status: options.status ?? STATUS };
      },
      async sessionsPrompt(request) {
        calls.push({ method: "prompt", request });
        return {
          inputOutcome: options.inputOutcome ?? AgentPromptInputOutcome.ACCEPTED,
          writtenBytes: options.writtenBytes ?? 12,
          reason: options.reason ?? "",
          ...(options.waitOutcome === undefined ? {} : { waitOutcome: options.waitOutcome }),
          ...(options.rejection === undefined ? {} : { rejection: options.rejection }),
        };
      },
    },
  };
}

async function capture(
  client: AgentPromptApiClient,
  args: readonly string[],
): Promise<{ output: string[]; exits: number[] }> {
  const output: string[] = [];
  const exits: number[] = [];
  const handled = await dispatchAgentPromptApi(
    client,
    AGENT_PROMPT_API_COMMAND.verb,
    args,
    (line) => output.push(line),
    (code) => exits.push(code),
  );
  expect(handled).toBe(true);
  return { output, exits };
}

describe("agent-prompt API CLI", () => {
  test("exports canonical command metadata and leaves other verbs untouched", async () => {
    expect(AGENT_PROMPT_API_COMMAND).toMatchObject({
      verb: "agent-prompt",
      usage: "roost api agent-prompt <session> <text> [--wait --until <states> --timeout <duration>]",
      parseArgs: parseAgentPromptArgs,
    });
    const { client, calls } = fakeClient();
    await expect(dispatchAgentPromptApi(client, "input", [SESSION_ID, "raw"])).resolves.toBe(false);
    expect(calls).toEqual([]);
  });

  test("parses exact positional text and a complete wait group", () => {
    const text = "--literal\\ntext";
    expect(parseAgentPromptArgs([
      SESSION_ID,
      text,
      "--wait",
      "--until=blocked,idle",
      "--timeout",
      "5m",
    ])).toEqual({
      sessionId: SESSION_ID,
      text,
      wait: { states: ["blocked", "idle"], timeoutMs: 300_000 },
    });
    expect(parseAgentPromptArgs([SESSION_ID, "--wait"])).toEqual({
      sessionId: SESSION_ID,
      text: "--wait",
      wait: null,
    });
  });

  test("accepts exact ASCII and multibyte byte limits and rejects empty or oversized text", async () => {
    const exactAscii = "a".repeat(16_384);
    const exactMultibyte = "é".repeat(8_192);
    expect(parseAgentPromptArgs([SESSION_ID, exactAscii]).text).toBe(exactAscii);
    expect(parseAgentPromptArgs([SESSION_ID, exactMultibyte]).text).toBe(exactMultibyte);
    for (const text of ["", `${exactAscii}a`, `${exactMultibyte}é`]) {
      const { client, calls } = fakeClient();
      await expect(capture(client, [SESSION_ID, text])).rejects.toThrow(
        "agent-prompt: <text> must be nonempty and at most 16384 UTF-8 bytes",
      );
      expect(calls).toEqual([]);
    }
  });

  test("rejects partial, duplicate, unknown, and out-of-range options before lookup", async () => {
    const invalidArgv = [
      [] as string[],
      [SESSION_ID],
      [SESSION_ID, "text", "--wait"],
      [SESSION_ID, "text", "--until", "idle", "--timeout", "1s"],
      [SESSION_ID, "text", "--wait", "--timeout", "1s"],
      [SESSION_ID, "text", "--wait", "--until", "idle"],
      [SESSION_ID, "text", "--wait", "--wait", "--until", "idle", "--timeout", "1s"],
      [SESSION_ID, "text", "--wait", "--until", "idle", "--until", "working", "--timeout", "1s"],
      [SESSION_ID, "text", "--wait", "--until", "idle", "--timeout", "1s", "--timeout", "2s"],
      [SESSION_ID, "text", "--wait", "--until", "idle", "--timeout", "1s", "--unknown"],
      [SESSION_ID, "text", "--wait", "--until", "idle,idle", "--timeout", "1s"],
      [SESSION_ID, "text", "--wait", "--until", "done", "--timeout", "1s"],
      [SESSION_ID, "text", "--wait", "--until", "idle", "--timeout", "0ms"],
      [SESSION_ID, "text", "--wait", "--until", "idle", "--timeout", "300001ms"],
      [SESSION_ID, "text", "--wait", "--until", "idle", "--timeout", "1.5s"],
      [SESSION_ID, "text", "unexpected-secret-extra"],
    ];
    for (const args of invalidArgv) {
      const { client, calls } = fakeClient();
      let failure = "";
      try {
        await capture(client, args);
      } catch (error) {
        failure = String(error);
      }
      expect(failure).toContain("agent-prompt:");
      expect(failure).not.toContain("unexpected-secret-extra");
      expect(calls).toEqual([]);
    }
  });

  test("gets status first and sends its exact integration fence once", async () => {
    const text = "literal\\ntext";
    const { client, calls } = fakeClient({ writtenBytes: 13 });
    await expect(capture(client, [SESSION_ID, text])).resolves.toEqual({
      output: ["input\taccepted\t13\t-"],
      exits: [],
    });
    expect(calls).toEqual([
      { method: "get", request: { sessionId: SESSION_ID } },
      {
        method: "prompt",
        request: {
          sessionId: SESSION_ID,
          expectedStatusEpoch: STATUS.statusEpoch,
          expectedOccupantId: STATUS.occupantId,
          expectedRevision: 23n,
          text,
          waitStates: [],
        },
      },
    ]);
  });

  test("forwards a complete wait and prints it separately from accepted input", async () => {
    const { client, calls } = fakeClient({
      writtenBytes: 9,
      waitOutcome: AgentPromptWaitOutcome.MATCHED,
    });
    await expect(capture(client, [
      SESSION_ID,
      "continue",
      "--wait",
      "--until",
      "working,idle",
      "--timeout=30s",
    ])).resolves.toEqual({
      output: ["input\taccepted\t9\t-", "wait\tmatched"],
      exits: [],
    });
    expect(calls[1]).toEqual({
      method: "prompt",
      request: expect.objectContaining({
        waitStates: ["working", "idle"],
        waitTimeoutMs: 30_000,
      }),
    });
  });

  test("prints each bounded rejection cause and exits nonzero without retry", async () => {
    for (const [rejection, label] of [
      [AgentPromptRejection.BLOCKED, "blocked"],
      [AgentPromptRejection.NOT_PROMPTABLE, "not_promptable"],
      [AgentPromptRejection.NOT_FOREGROUND, "not_foreground"],
      [AgentPromptRejection.FENCE_CHANGED, "fence_changed"],
      [AgentPromptRejection.PROCESS_CHANGED, "process_changed"],
      [AgentPromptRejection.SESSION_UNAVAILABLE, "session_unavailable"],
      [AgentPromptRejection.EXPIRED, "expired"],
      [AgentPromptRejection.KEEPER_REJECTED, "keeper_rejected"],
    ] as const) {
      const { client, calls } = fakeClient({
        inputOutcome: AgentPromptInputOutcome.REJECTED,
        writtenBytes: 0,
        reason: "agent prompt rejected",
        rejection,
      });
      await expect(capture(client, [SESSION_ID, "one attempt"])).resolves.toEqual({
        output: [`input\trejected\t0\t${label}`],
        exits: [1],
      });
      expect(calls.filter((call) => call.method === "prompt")).toHaveLength(1);
    }
  });

  test("prints ambiguous detail, and a rejection skips the wait line", async () => {
    const ambiguous = fakeClient({
      inputOutcome: AgentPromptInputOutcome.AMBIGUOUS,
      writtenBytes: 4,
      reason: "agent prompt outcome is ambiguous",
    });
    await expect(capture(ambiguous.client, [SESSION_ID, "one attempt"])).resolves.toEqual({
      output: ["input\tambiguous\t4\tagent prompt outcome is ambiguous"],
      exits: [1],
    });
    const rejectedWait = fakeClient({
      inputOutcome: AgentPromptInputOutcome.REJECTED,
      writtenBytes: 0,
      reason: "agent prompt rejected",
      rejection: AgentPromptRejection.BLOCKED,
    });
    await expect(capture(rejectedWait.client, [
      SESSION_ID, "continue", "--wait", "--until", "idle", "--timeout", "1s",
    ])).resolves.toEqual({
      output: ["input\trejected\t0\tblocked"],
      exits: [1],
    });
    expect(rejectedWait.calls.filter((call) => call.method === "prompt")).toHaveLength(1);
  });

  test("prints every non-matched wait and keeps ambiguous input nonzero when matched", async () => {
    for (const [waitOutcome, label] of [
      [AgentPromptWaitOutcome.TIMED_OUT, "timed_out"],
      [AgentPromptWaitOutcome.OCCUPANT_CHANGED, "occupant_changed"],
      [AgentPromptWaitOutcome.SESSION_CLOSED, "session_closed"],
      [AgentPromptWaitOutcome.PROMPT_STALLED, "prompt_stalled"],
    ] as const) {
      const { client } = fakeClient({ waitOutcome });
      await expect(capture(client, [
        SESSION_ID, "continue", "--wait", "--until", "idle", "--timeout", "1s",
      ])).resolves.toEqual({
        output: ["input\taccepted\t12\t-", `wait\t${label}`],
        exits: [1],
      });
    }
    const { client, calls } = fakeClient({
      inputOutcome: AgentPromptInputOutcome.AMBIGUOUS,
      writtenBytes: 3,
      reason: "completion unknown",
      waitOutcome: AgentPromptWaitOutcome.MATCHED,
    });
    await expect(capture(client, [
      SESSION_ID, "continue", "--wait", "--until", "idle", "--timeout", "1s",
    ])).resolves.toEqual({
      output: ["input\tambiguous\t3\tcompletion unknown", "wait\tmatched"],
      exits: [1],
    });
    expect(calls.filter((call) => call.method === "prompt")).toHaveLength(1);
  });

  test("rejects impossible coordinator byte counts or wait shapes", async () => {
    for (const options of [
      { inputOutcome: AgentPromptInputOutcome.ACCEPTED, writtenBytes: 0 },
      { inputOutcome: AgentPromptInputOutcome.REJECTED, writtenBytes: 1 },
      {
        inputOutcome: AgentPromptInputOutcome.AMBIGUOUS,
        writtenBytes: AGENT_PROMPT_MAX_WRITE_BYTES + 1,
      },
    ]) {
      const { client, calls } = fakeClient(options);
      await expect(capture(client, [SESSION_ID, "one attempt"])).rejects.toThrow(
        "agent-prompt: coordinator returned an invalid response",
      );
      expect(calls.filter((call) => call.method === "prompt")).toHaveLength(1);
    }
    const missingWait = fakeClient();
    await expect(capture(missingWait.client, [
      SESSION_ID, "one attempt", "--wait", "--until", "idle", "--timeout", "1s",
    ])).rejects.toThrow("agent-prompt: coordinator returned an invalid response");
    const unsolicitedWait = fakeClient({ waitOutcome: AgentPromptWaitOutcome.MATCHED });
    await expect(capture(unsolicitedWait.client, [SESSION_ID, "one attempt"])).rejects.toThrow(
      "agent-prompt: coordinator returned an invalid response",
    );
    const foreignRejection = fakeClient({
      inputOutcome: AgentPromptInputOutcome.ACCEPTED,
      rejection: AgentPromptRejection.BLOCKED,
    });
    await expect(capture(foreignRejection.client, [SESSION_ID, "one attempt"])).rejects.toThrow(
      "agent-prompt: coordinator returned an invalid response",
    );
    const unspecifiedRejection = fakeClient({
      inputOutcome: AgentPromptInputOutcome.REJECTED,
      writtenBytes: 0,
      rejection: AgentPromptRejection.UNSPECIFIED,
    });
    await expect(capture(unspecifiedRejection.client, [SESSION_ID, "one attempt"]))
      .rejects.toThrow("agent-prompt: coordinator returned an invalid response");
  });

  test("refuses missing, malformed, screen-only, or unsafe status fences before prompting", async () => {
    const missingIdentity = create(AgentStatusViewSchema, {
      sessionId: SESSION_ID,
      agentId: "omp",
      state: "idle",
      revision: 1n,
      active: true,
      promptable: false,
    });
    const screenOnly = create(AgentStatusViewSchema, {
      ...STATUS,
      source: "screen",
      promptable: false,
    });
    const unsafeRevision = create(AgentStatusViewSchema, {
      ...STATUS,
      revision: BigInt(Number.MAX_SAFE_INTEGER) + 1n,
    });
    const malformedIdentity = create(AgentStatusViewSchema, {
      ...STATUS,
      statusEpoch: "not-a-uuid",
    });
    for (const [status, message] of [
      [missingIdentity, "agent-prompt: current agent status has no occupant identity"],
      [screenOnly, "agent-prompt: current agent status is not promptable"],
      [unsafeRevision, "agent-prompt: current agent status revision is not a safe integer"],
      [malformedIdentity, "agent-prompt: current agent status has invalid occupant identity"],
    ] as const) {
      const { client, calls } = fakeClient({ status });
      await expect(capture(client, [SESSION_ID, "do not send"])).rejects.toThrow(message);
      expect(calls.map((call) => call.method)).toEqual(["get"]);
    }
  });

  test("does not reflect prompt text or agent status messages through result reasons", async () => {
    for (const [text, reason] of [
      ["unique-prompt-secret", "failed: unique-prompt-secret"],
      ["ordinary prompt", "failed: private status detail"],
      [String.fromCharCode(0xd800), `failed: ${String.fromCodePoint(0xfffd)}`],
    ]) {
      const { client } = fakeClient({
        inputOutcome: AgentPromptInputOutcome.REJECTED,
        writtenBytes: 0,
        reason,
      });
      const result = await capture(client, [SESSION_ID, text]);
      expect(result).toEqual({ output: ["input\trejected\t0\t-"], exits: [1] });
      expect(result.output.join("\n")).not.toContain(text);
      expect(result.output.join("\n")).not.toContain(STATUS.message!);
    }
  });
});

// SessionsPrompt boundary tests pin dashboard/device authorization, exact
// validation, non-oracular session rejection, dedicated worker framing, and
// strict public outcome secrecy. Worker replies use the real pending-RPC table.

import { create } from "@bufbuild/protobuf";
import { Code } from "@connectrpc/connect";
import { afterAll, afterEach, beforeAll, beforeEach, describe, expect, spyOn, test } from "bun:test";
import {
  AgentPromptInputOutcome,
  AgentPromptWaitOutcome,
  SessionsPromptRequestSchema,
  type SessionsPromptRequest,
} from "@roost/shared/proto/coordinator_pb";
import {
  TerminalInputStatus,
  TerminalWritePhase,
  WInputResultSchema,
} from "@roost/shared/proto/worker_transport_pb";
import { AGENT_PROMPT_MAX_WRITE_BYTES } from "@roost/shared/terminal-input";
import { log } from "@roost/shared/log";
import { _agentStatusWaiterStats } from "../src/agent-status-hub.ts";
import { resolvePendingRpc } from "../src/router/pending-rpcs.ts";
import {
  FOREIGN_SESSION,
  MISSING_SESSION,
  PROMPT_OCCUPANT,
  PROMPT_SESSION,
  PROMPT_STATUS_EPOCH,
  PROMPT_WORKER,
  startAgentPromptTestFixture,
  type AgentPromptTestFixture,
} from "./agent-prompt-test-fixture.ts";

let fixture: AgentPromptTestFixture;

function request(overrides: Partial<{
  sessionId: string;
  expectedStatusEpoch: string;
  expectedOccupantId: string;
  expectedRevision: bigint;
  text: string;
  waitStates: string[];
  waitTimeoutMs: number;
}> = {}) {
  return create(SessionsPromptRequestSchema, {
    sessionId: PROMPT_SESSION,
    expectedStatusEpoch: PROMPT_STATUS_EPOCH,
    expectedOccupantId: PROMPT_OCCUPANT,
    expectedRevision: 1n,
    text: "continue",
    ...overrides,
  });
}

beforeAll(async () => {
  fixture = await startAgentPromptTestFixture();
});

beforeEach(() => fixture.resetStatus());
afterEach(() => fixture.clearCase());
afterAll(async () => fixture.cleanup());

describe("SessionsPrompt authorization and validation", () => {
  test("requires an existing dashboard device actor", async () => {
    await expect(fixture.handlers.sessionsPrompt(
      request(),
      fixture.anonymousContext(),
    )).rejects.toMatchObject({ code: Code.Unauthenticated });
  });

  test("rejects every malformed identity, revision, text, and wait combination", async () => {
    for (const invalid of [
      request({ sessionId: "not-a-uuid" }),
      request({ expectedStatusEpoch: "not-a-uuid" }),
      request({ expectedOccupantId: "not-a-uuid" }),
      request({ expectedRevision: BigInt(Number.MAX_SAFE_INTEGER) + 1n }),
      request({ text: "" }),
      request({ text: "é".repeat(8_193) }),
      request({ waitStates: ["idle"] }),
      request({ waitTimeoutMs: 1_000 }),
      request({ waitStates: ["idle", "idle"], waitTimeoutMs: 1_000 }),
      request({ waitStates: ["done"], waitTimeoutMs: 1_000 }),
      request({ waitStates: ["idle"], waitTimeoutMs: 0 }),
      request({ waitStates: ["idle"], waitTimeoutMs: 300_001 }),
    ]) {
      await expect(fixture.handlers.sessionsPrompt(invalid, fixture.context()))
        .rejects.toMatchObject({ code: Code.InvalidArgument });
    }
    expect(_agentStatusWaiterStats().total).toBe(0);
  });

  test("makes absent and foreign sessions the same definite rejection before wait or send", async () => {
    let sends = 0;
    fixture.attachWorker(() => {
      sends += 1;
      return 1;
    });
    const responses = await Promise.all([MISSING_SESSION, FOREIGN_SESSION].map((sessionId) =>
      fixture.handlers.sessionsPrompt(request({
        sessionId,
        waitStates: ["idle"],
        waitTimeoutMs: 300_000,
      }), fixture.context())
    ));
    expect(responses.map((response) => ({
      outcome: response.inputOutcome,
      writtenBytes: response.writtenBytes,
      reason: response.reason,
      waitOutcome: response.waitOutcome,
    }))).toEqual([
      {
        outcome: AgentPromptInputOutcome.REJECTED,
        writtenBytes: 0,
        reason: "agent prompt rejected",
        waitOutcome: undefined,
      },
      {
        outcome: AgentPromptInputOutcome.REJECTED,
        writtenBytes: 0,
        reason: "agent prompt rejected",
        waitOutcome: undefined,
      },
    ]);
    expect(sends).toBe(0);
    expect(_agentStatusWaiterStats().total).toBe(0);
  });
});

describe("SessionsPrompt worker truth and secrecy", () => {
  test("sends the exact dedicated status fence with a relative budget", async () => {
    const text = "first line\nsecond line";
    let observed = false;
    fixture.attachWorker((frame) => {
      expect(frame.frame.case).toBe("agentPrompt");
      if (frame.frame.case !== "agentPrompt") throw new Error("expected agent prompt");
      const prompt = frame.frame.value;
      observed = true;
      expect(prompt).toMatchObject({
        sessionId: PROMPT_SESSION,
        expectedStatusEpoch: PROMPT_STATUS_EPOCH,
        expectedOccupantId: PROMPT_OCCUPANT,
        expectedRevision: 1n,
        text,
      });
      expect(prompt.requestId).not.toBe("");
      expect(prompt.inputSeq).toBeGreaterThan(0n);
      expect(prompt.budgetMs).toBeGreaterThan(0);
      resolvePendingRpc(prompt.requestId, create(WInputResultSchema, {
        requestId: prompt.requestId,
        sessionId: prompt.sessionId,
        inputSeq: prompt.inputSeq,
        status: TerminalInputStatus.ACCEPTED,
        phase: TerminalWritePhase.WRITTEN,
        writtenBytes: 33,
      }), PROMPT_WORKER);
      return 1;
    });

    const response = await fixture.handlers.sessionsPrompt(request({ text }), fixture.context());
    expect(observed).toBe(true);
    expect(response).toMatchObject({
      inputOutcome: AgentPromptInputOutcome.ACCEPTED,
      writtenBytes: 33,
      reason: "",
    });
    expect(response.waitOutcome).toBeUndefined();
  });

  test("classifies every worker status/phase combination without retry", async () => {
    const cases = [
      [TerminalInputStatus.ACCEPTED, TerminalWritePhase.WRITTEN, 13, AgentPromptInputOutcome.ACCEPTED],
      [TerminalInputStatus.ACCEPTED, TerminalWritePhase.PRE_WRITE, 13, AgentPromptInputOutcome.AMBIGUOUS],
      [TerminalInputStatus.REJECTED, TerminalWritePhase.PRE_WRITE, 0, AgentPromptInputOutcome.REJECTED],
      [TerminalInputStatus.REJECTED, TerminalWritePhase.WRITTEN, 0, AgentPromptInputOutcome.AMBIGUOUS],
      [TerminalInputStatus.AMBIGUOUS, TerminalWritePhase.UNKNOWN, 4, AgentPromptInputOutcome.AMBIGUOUS],
      [TerminalInputStatus.ACCEPTED, TerminalWritePhase.WRITTEN, AGENT_PROMPT_MAX_WRITE_BYTES + 1, AgentPromptInputOutcome.AMBIGUOUS],
      [TerminalInputStatus.REJECTED, TerminalWritePhase.PRE_WRITE, 1, AgentPromptInputOutcome.AMBIGUOUS],
    ] as const;
    let sends = 0;
    for (const [status, phase, writtenBytes, expected] of cases) {
      fixture.attachWorker((frame) => {
        sends += 1;
        if (frame.frame.case !== "agentPrompt") throw new Error("expected agent prompt");
        const prompt = frame.frame.value;
        resolvePendingRpc(prompt.requestId, create(WInputResultSchema, {
          requestId: prompt.requestId,
          sessionId: prompt.sessionId,
          inputSeq: prompt.inputSeq,
          status,
          phase,
          writtenBytes,
          reason: "private worker status detail",
        }), PROMPT_WORKER);
        return 1;
      });
      const response = await fixture.handlers.sessionsPrompt(request(), fixture.context());
      expect(response.inputOutcome).toBe(expected);
      expect(response.writtenBytes).toBeLessThanOrEqual(AGENT_PROMPT_MAX_WRITE_BYTES);
      expect(response.reason).not.toContain("private worker status detail");
      expect(response.waitOutcome).toBeUndefined();
    }
    expect(sends).toBe(cases.length);
  });

  test("never emits prompt text or retained status messages in response or logs", async () => {
    const promptSecret = "PROMPT_SECRET_7ac142";
    const statusSecret = "STATUS_SECRET_921ec3";
    fixture.retainStatus("working", 2, statusSecret);
    const logSpy = spyOn(log, "info").mockImplementation(() => undefined);
    fixture.attachWorker((frame) => {
      if (frame.frame.case !== "agentPrompt") throw new Error("expected agent prompt");
      const prompt = frame.frame.value;
      resolvePendingRpc(prompt.requestId, create(WInputResultSchema, {
        requestId: prompt.requestId,
        sessionId: prompt.sessionId,
        inputSeq: prompt.inputSeq,
        status: TerminalInputStatus.REJECTED,
        phase: TerminalWritePhase.PRE_WRITE,
        writtenBytes: 0,
        reason: `${promptSecret}:${statusSecret}`,
      }), PROMPT_WORKER);
      return 1;
    });
    try {
      const response = await fixture.handlers.sessionsPrompt(request({
        text: promptSecret,
        expectedRevision: 2n,
      }), fixture.context());
      const publicPayload = JSON.stringify(response);
      expect(response.reason).toBe("agent prompt rejected");
      expect(publicPayload).not.toContain(promptSecret);
      expect(publicPayload).not.toContain(statusSecret);
      const logged = JSON.stringify(logSpy.mock.calls);
      expect(logged).not.toContain(promptSecret);
      expect(logged).not.toContain(statusSecret);
    } finally {
      logSpy.mockRestore();
    }
  });
});

describe("SessionsPrompt status waiting", () => {
  test("captures a fast accepted transition emitted during worker send", async () => {
    fixture.attachWorker((frame) => {
      if (frame.frame.case !== "agentPrompt") throw new Error("expected agent prompt");
      const prompt = frame.frame.value;
      fixture.retainStatus("idle", 2);
      resolvePendingRpc(prompt.requestId, create(WInputResultSchema, {
        requestId: prompt.requestId,
        sessionId: prompt.sessionId,
        inputSeq: prompt.inputSeq,
        status: TerminalInputStatus.ACCEPTED,
        phase: TerminalWritePhase.WRITTEN,
        writtenBytes: 9,
      }), PROMPT_WORKER);
      return 1;
    });
    const response = await fixture.handlers.sessionsPrompt(request({
      waitStates: ["idle"],
      waitTimeoutMs: 30_000,
    }), fixture.context());
    expect(response).toMatchObject({
      inputOutcome: AgentPromptInputOutcome.ACCEPTED,
      waitOutcome: AgentPromptWaitOutcome.MATCHED,
    });
    expect(_agentStatusWaiterStats().total).toBe(0);
  });

  test("awaits the requested state even when input completion is ambiguous", async () => {
    fixture.attachWorker((frame) => {
      if (frame.frame.case !== "agentPrompt") throw new Error("expected agent prompt");
      const prompt = frame.frame.value;
      fixture.retainStatus("blocked", 2);
      resolvePendingRpc(prompt.requestId, create(WInputResultSchema, {
        requestId: prompt.requestId,
        sessionId: prompt.sessionId,
        inputSeq: prompt.inputSeq,
        status: TerminalInputStatus.AMBIGUOUS,
        phase: TerminalWritePhase.UNKNOWN,
        writtenBytes: 2,
      }), PROMPT_WORKER);
      return 1;
    });
    const response = await fixture.handlers.sessionsPrompt(request({
      waitStates: ["blocked"],
      waitTimeoutMs: 30_000,
    }), fixture.context());
    expect(response).toMatchObject({
      inputOutcome: AgentPromptInputOutcome.AMBIGUOUS,
      waitOutcome: AgentPromptWaitOutcome.MATCHED,
    });
    expect(_agentStatusWaiterStats().total).toBe(0);
  });

  test("aborts and consumes the provisional waiter on a definite rejection", async () => {
    fixture.attachWorker((frame) => {
      if (frame.frame.case !== "agentPrompt") throw new Error("expected agent prompt");
      const prompt = frame.frame.value;
      resolvePendingRpc(prompt.requestId, create(WInputResultSchema, {
        requestId: prompt.requestId,
        sessionId: prompt.sessionId,
        inputSeq: prompt.inputSeq,
        status: TerminalInputStatus.REJECTED,
        phase: TerminalWritePhase.PRE_WRITE,
        writtenBytes: 0,
      }), PROMPT_WORKER);
      return 1;
    });
    const response = await fixture.handlers.sessionsPrompt(request({
      waitStates: ["idle"],
      waitTimeoutMs: 300_000,
    }), fixture.context());
    expect(response.inputOutcome).toBe(AgentPromptInputOutcome.REJECTED);
    expect(response.waitOutcome).toBeUndefined();
    expect(_agentStatusWaiterStats().total).toBe(0);
  });
});

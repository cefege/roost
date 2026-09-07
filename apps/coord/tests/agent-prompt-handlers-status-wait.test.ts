// Pins the SessionsPrompt status-wait arm: which agent transitions satisfy a
// requested wait, when a prompted idle agent stalls instead of matching, and
// that every waiter is consumed. Sibling agent-prompt-handlers.test.ts owns
// authorization, worker truth, secrecy, and rejection classification.
// Drives the real waiter table through ./agent-prompt-test-fixture.ts.

import { create } from "@bufbuild/protobuf";
import {
  afterAll,
  afterEach,
  beforeAll,
  beforeEach,
  describe,
  expect,
  test,
  vi,
} from "bun:test";
import {
  AgentPromptInputOutcome,
  AgentPromptWaitOutcome,
} from "@roost/shared/proto/coordinator_pb";
import {
  TerminalInputStatus,
  TerminalWritePhase,
  WInputResultSchema,
} from "@roost/shared/proto/worker_transport_pb";
import { _agentStatusWaiterStats } from "../src/agent-status-wait.ts";
import { AGENT_PROMPT_EFFECT_TIMEOUT_MS } from "../src/connect/agent-prompt-control.ts";
import { resolvePendingRpc } from "../src/router/pending-rpcs.ts";
import {
  PROMPT_WORKER,
  agentPromptRequest as request,
  startAgentPromptTestFixture,
  type AgentPromptTestFixture,
} from "./agent-prompt-test-fixture.ts";

let fixture: AgentPromptTestFixture;

beforeAll(async () => {
  fixture = await startAgentPromptTestFixture();
});

beforeEach(() => fixture.resetStatus());
afterEach(() => fixture.clearCase());
afterAll(async () => fixture.cleanup());

describe("SessionsPrompt status waiting", () => {
  test("captures a fast accepted transition emitted during worker send", async () => {
    fixture.attachWorker((frame) => {
      if (frame.frame.case !== "agentPrompt") throw new Error("expected agent prompt");
      const prompt = frame.frame.value;
      fixture.retainStatus("idle", 2, undefined, 2);
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

  test("stalls instead of matching when a prompted idle agent never starts a turn", async () => {
    fixture.retainStatus("idle", 2, undefined, 2);
    let acked = false;
    fixture.attachWorker((frame) => {
      if (frame.frame.case !== "agentPrompt") throw new Error("expected agent prompt");
      const prompt = frame.frame.value;
      // Same idle state, higher revision: only the status message changed.
      fixture.retainStatus("idle", 3, "waiting for you", 2);
      resolvePendingRpc(prompt.requestId, create(WInputResultSchema, {
        requestId: prompt.requestId,
        sessionId: prompt.sessionId,
        inputSeq: prompt.inputSeq,
        status: TerminalInputStatus.ACCEPTED,
        phase: TerminalWritePhase.WRITTEN,
        writtenBytes: 9,
      }), PROMPT_WORKER);
      acked = true;
      return 1;
    });
    vi.useFakeTimers();
    try {
      const pending = fixture.handlers.sessionsPrompt(request({
        expectedRevision: 2n,
        waitStates: ["idle"],
        waitTimeoutMs: 30_000,
      }), fixture.context());
      for (let round = 0; round < 200 && !acked; round += 1) {
        vi.advanceTimersByTime(0);
        await Promise.resolve();
      }
      expect(acked).toBe(true);
      for (let round = 0; round < 20; round += 1) await Promise.resolve();
      expect(_agentStatusWaiterStats().total).toBe(1);
      vi.advanceTimersByTime(AGENT_PROMPT_EFFECT_TIMEOUT_MS);
      await expect(pending).resolves.toMatchObject({
        inputOutcome: AgentPromptInputOutcome.ACCEPTED,
        waitOutcome: AgentPromptWaitOutcome.PROMPT_STALLED,
      });
    } finally {
      vi.useRealTimers();
    }
    expect(_agentStatusWaiterStats().total).toBe(0);
  });

  test("matches once a prompted idle agent works and then completes", async () => {
    fixture.retainStatus("idle", 2, undefined, 2);
    fixture.attachWorker((frame) => {
      if (frame.frame.case !== "agentPrompt") throw new Error("expected agent prompt");
      const prompt = frame.frame.value;
      fixture.retainStatus("working", 3, undefined, 2);
      fixture.retainStatus("idle", 4, undefined, 4);
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
      expectedRevision: 2n,
      waitStates: ["idle"],
      waitTimeoutMs: 30_000,
    }), fixture.context());
    expect(response).toMatchObject({
      inputOutcome: AgentPromptInputOutcome.ACCEPTED,
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

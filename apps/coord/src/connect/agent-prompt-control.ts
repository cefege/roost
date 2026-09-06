// Status-fenced agent-prompt orchestration registers an optional occupant wait
// before terminal FIFO admission, then routes one dedicated worker request.
// It shares the raw-input lane, move gate, deadline, and WInputResult classifier;
// cancellation always removes and consumes the provisional status waiter.

import { AGENT_PROMPT_MAX_WRITE_BYTES } from "@roost/shared/terminal-input";
import {
  AgentStatusWaitError,
  waitForAgentStatus,
  type AgentStatusWaitOutcome,
  type AgentStatusWaitResult,
} from "../agent-status-hub.ts";
import type { ConnectDeps } from "./router.ts";
import {
  INPUT_CONTROL_TIMEOUT_MS,
  sendAgentPromptRequest,
  startHopDeadline,
  type HopDeadline,
} from "./worker-send.ts";
import type { TerminalViewerIdentity } from "./terminal-control-lane.ts";
import {
  processTerminalWriteControl,
  type TerminalWriteControlResult,
} from "./terminal-write-control.ts";

export interface AgentPromptWaitConfig {
  states: readonly string[];
  timeoutMs: number;
}

export interface AgentPromptControlCommand {
  identity: TerminalViewerIdentity;
  sessionId: string;
  inputSeq: bigint;
  expectedStatusEpoch: string;
  expectedOccupantId: string;
  expectedRevision: number;
  text: string;
  wait?: AgentPromptWaitConfig;
  /** Test injection; production creates one monotonic deadline at entry. */
  deadline?: HopDeadline;
}

export interface AgentPromptControlResult {
  input: TerminalWriteControlResult;
  waitOutcome?: AgentStatusWaitOutcome;
}

type ObservedWait =
  | { fulfilled: true; result: AgentStatusWaitResult }
  | { fulfilled: false; error: unknown };

/** The observer is attached in the same turn as registration, before input can
 * enqueue, so an early match/cancel cannot become an unhandled rejection. */
function observeAgentStatusWait(
  promise: Promise<AgentStatusWaitResult>,
): Promise<ObservedWait> {
  return promise.then(
    (result) => ({ fulfilled: true, result }),
    (error) => ({ fulfilled: false, error }),
  );
}

function unwrapObservedWait(observed: ObservedWait): AgentStatusWaitResult {
  if (observed.fulfilled) return observed.result;
  throw observed.error;
}

/** Execute exactly one prompt write and, when configured, return the first
 * exact-occupant state transition observed after the requested revision. */
export async function processAgentPromptControl(
  deps: ConnectDeps,
  command: AgentPromptControlCommand,
  signal: AbortSignal,
): Promise<AgentPromptControlResult> {
  const deadline = command.deadline ?? startHopDeadline(INPUT_CONTROL_TIMEOUT_MS);
  if (!command.wait) {
    return {
      input: await processPromptWrite(deps, command, deadline),
    };
  }

  const waitController = new AbortController();
  const cancelWait = () => waitController.abort();
  signal.addEventListener("abort", cancelWait, { once: true });
  if (signal.aborted) waitController.abort();

  let observedWait: Promise<ObservedWait> | undefined;
  let waitConsumed = false;
  try {
    // waitForAgentStatus performs validation/capacity admission synchronously;
    // no terminal command is enqueued if registration cannot succeed.
    observedWait = observeAgentStatusWait(waitForAgentStatus({
      sessionId: command.sessionId,
      statusEpoch: command.expectedStatusEpoch,
      occupantId: command.expectedOccupantId,
      desiredStates: command.wait.states,
      afterRevision: command.expectedRevision,
      timeoutMs: command.wait.timeoutMs,
    }, waitController.signal));
    if (signal.aborted) {
      const canceled = await observedWait;
      waitConsumed = true;
      unwrapObservedWait(canceled);
      throw new AgentStatusWaitError("canceled", "agent status wait canceled");
    }

    const input = await processPromptWrite(deps, command, deadline);
    if (input.status === "rejected") {
      waitController.abort();
      await observedWait;
      waitConsumed = true;
      return { input };
    }

    const observed = await observedWait;
    waitConsumed = true;
    return {
      input,
      waitOutcome: unwrapObservedWait(observed).outcome,
    };
  } finally {
    signal.removeEventListener("abort", cancelWait);
    if (observedWait && !waitConsumed) {
      waitController.abort();
      await observedWait;
    }
  }
}

function processPromptWrite(
  deps: ConnectDeps,
  command: AgentPromptControlCommand,
  deadline: HopDeadline,
): Promise<TerminalWriteControlResult> {
  return processTerminalWriteControl(
    deps,
    {
      identity: command.identity,
      sessionId: command.sessionId,
      inputSeq: command.inputSeq,
      deadline,
    },
    {
      kind: "worker-written",
      maximumWrittenBytes: AGENT_PROMPT_MAX_WRITE_BYTES,
    },
    (workerFp, dashboardId, workerDeadline) => sendAgentPromptRequest(workerFp, {
      sessionId: command.sessionId,
      inputSeq: command.inputSeq,
      expectedStatusEpoch: command.expectedStatusEpoch,
      expectedOccupantId: command.expectedOccupantId,
      expectedRevision: BigInt(command.expectedRevision),
      text: command.text,
      dashboardId,
    }, workerDeadline),
  );
}

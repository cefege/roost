// Status-fenced agent-prompt orchestration registers an occupant wait before
// terminal FIFO admission, then routes one dedicated worker request.
// A settled-state wait is two-phase: observed activity is required first, so a
// prompt the agent never processed reports a stall instead of a match.
// It shares the raw-input lane, move gate, deadline, and WInputResult classifier;
// cancellation always removes and consumes the provisional status waiter.

import { AGENT_PROMPT_MAX_WRITE_BYTES } from "@roost/shared/terminal-input";
import {
  retainedAgentOccupantState,
  waitForAgentStatus,
} from "../agent-status-hub.ts";
import {
  AgentStatusWaitError,
  type AgentStatusWaitOutcome,
  type AgentStatusWaitResult,
} from "../agent-status-wait.ts";
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

/** A prompt must move its agent inside this window before a settled-state wait
 * is honoured; without it an idle agent satisfies the wait with the turn the
 * prompt was supposed to start. */
export const AGENT_PROMPT_EFFECT_TIMEOUT_MS = 5_000;

const PROMPT_ACTIVITY_STATES = ["working", "blocked"] as const;

export type AgentPromptWaitOutcomeName = AgentStatusWaitOutcome | "prompt_stalled";

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
  waitOutcome?: AgentPromptWaitOutcomeName;
}

interface PromptActivityGate {
  readonly timeoutMs: number;
  /** A gate timeout is a stall only when the whole window was available. */
  readonly stallOnTimeout: boolean;
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
  const wait = command.wait;
  if (!wait) {
    return {
      input: await processPromptWrite(deps, command, deadline),
    };
  }

  const waitController = new AbortController();
  const cancelWait = () => waitController.abort();
  signal.addEventListener("abort", cancelWait, { once: true });
  if (signal.aborted) waitController.abort();

  const waitBudget = startHopDeadline(wait.timeoutMs);
  const gate = promptActivityGate(command, wait, waitBudget);
  let observedWait: Promise<ObservedWait> | undefined;
  let waitConsumed = false;
  try {
    // waitForAgentStatus performs validation/capacity admission synchronously;
    // no terminal command is enqueued if registration cannot succeed.
    observedWait = observeAgentStatusWait(waitForAgentStatus({
      sessionId: command.sessionId,
      statusEpoch: command.expectedStatusEpoch,
      occupantId: command.expectedOccupantId,
      desiredStates: gate ? PROMPT_ACTIVITY_STATES : wait.states,
      afterRevision: command.expectedRevision,
      timeoutMs: gate ? gate.timeoutMs : wait.timeoutMs,
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
    const firstPhase = unwrapObservedWait(observed);
    if (!gate) return { input, waitOutcome: firstPhase.outcome };
    return {
      input,
      waitOutcome: await settledWaitOutcome({
        command,
        states: wait.states,
        gate,
        activity: firstPhase,
        budget: waitBudget,
        signal: waitController.signal,
      }),
    };
  } finally {
    signal.removeEventListener("abort", cancelWait);
    if (observedWait && !waitConsumed) {
      waitController.abort();
      await observedWait;
    }
  }
}

/** A caller that waits for activity itself needs no gate, and an agent already
 * working or blocked has already proved it. */
function promptActivityGate(
  command: AgentPromptControlCommand,
  wait: AgentPromptWaitConfig,
  budget: HopDeadline,
): PromptActivityGate | undefined {
  if (wait.states.some((state) => state === "working" || state === "blocked")) {
    return undefined;
  }
  const pinned = retainedAgentOccupantState(
    command.sessionId,
    command.expectedStatusEpoch,
    command.expectedOccupantId,
  );
  if (pinned === "working" || pinned === "blocked") return undefined;
  const remainingMs = Math.max(1, Math.floor(budget.remainingMs()));
  return {
    timeoutMs: Math.min(AGENT_PROMPT_EFFECT_TIMEOUT_MS, remainingMs),
    stallOnTimeout: remainingMs > AGENT_PROMPT_EFFECT_TIMEOUT_MS,
  };
}

/** Second phase: the caller's own wait, floored at the observed activity so a
 * completion that predates the prompt cannot satisfy it. */
async function settledWaitOutcome(input: {
  command: AgentPromptControlCommand;
  states: readonly string[];
  gate: PromptActivityGate;
  activity: AgentStatusWaitResult;
  budget: HopDeadline;
  signal: AbortSignal;
}): Promise<AgentPromptWaitOutcomeName> {
  if (input.activity.outcome === "timed_out") {
    return input.gate.stallOnTimeout ? "prompt_stalled" : "timed_out";
  }
  if (input.activity.outcome !== "matched") return input.activity.outcome;
  const remainingMs = Math.floor(input.budget.remainingMs());
  if (remainingMs < 1) return "timed_out";
  const settled = await waitForAgentStatus({
    sessionId: input.command.sessionId,
    statusEpoch: input.command.expectedStatusEpoch,
    occupantId: input.command.expectedOccupantId,
    desiredStates: input.states,
    afterRevision: Math.max(
      input.command.expectedRevision,
      input.activity.matchedRevision ?? 0,
    ),
    timeoutMs: remainingMs,
  }, input.signal);
  return settled.outcome;
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

// SessionsPrompt authorizes an open dashboard session, validates every public
// fence and bound, then delegates exactly-once write/wait orchestration.
// Responses expose only bounded outcomes; prompt text and agent status messages
// never enter coordinator logs, audits, or durable storage.

import { create } from "@bufbuild/protobuf";
import { Code, ConnectError, type ServiceImpl } from "@connectrpc/connect";
import {
  AgentPromptInputOutcome,
  AgentPromptRejection,
  AgentPromptWaitOutcome,
  CoordinatorService,
  SessionsPromptResponseSchema,
  type SessionsPromptRequest,
  type SessionsPromptResponse,
} from "@roost/shared/proto/coordinator_pb";
import {
  AgentPromptTextSchema,
  AgentPromptWaitTimeoutMsSchema,
} from "@roost/shared/terminal-input";
import {
  AgentOccupantId,
  AgentRuntimeState,
  SessionId,
  StatusEpoch,
} from "@roost/shared/wire";
import { log } from "@roost/shared/log";
import { AgentStatusWaitError } from "../agent-status-wait.ts";
import {
  processAgentPromptControl,
  type AgentPromptWaitConfig,
  type AgentPromptWaitOutcomeName,
} from "./agent-prompt-control.ts";
import {
  requireAccountDevice,
  requireDashboardActor,
  remoteAddressKey,
  tabIdKey,
} from "./auth-interceptor.ts";
import { nextCompatibilityInputSeq } from "./input-control.ts";
import type { ConnectDeps } from "./router.ts";
import { terminalViewerIdentity } from "./terminal-control-lane.ts";
import type { TerminalWriteControlResult } from "./terminal-write-control.ts";

export type AgentPromptHandlers = Pick<
  ServiceImpl<typeof CoordinatorService>,
  "sessionsPrompt"
>;

interface ValidatedAgentPrompt {
  sessionId: string;
  expectedStatusEpoch: string;
  expectedOccupantId: string;
  expectedRevision: number;
  text: string;
  wait?: AgentPromptWaitConfig;
}

export function makeAgentPromptHandlers(deps: ConnectDeps): AgentPromptHandlers {
  return {
    async sessionsPrompt(request, context) {
      const actor = requireDashboardActor(context.values);
      const caller = requireAccountDevice(context.values);
      const validated = validateAgentPromptRequest(request);
      const authorizedSession = await deps.db.selectFrom("sessions")
        .select("id")
        .where("id", "=", validated.sessionId)
        .where("dashboard_id", "=", actor.dashboardId)
        .where("status", "=", "open")
        .executeTakeFirst();
      if (!authorizedSession) {
        return promptResponse({
          status: "rejected",
          sessionId: validated.sessionId,
          inputSeq: 0n,
          writtenBytes: 0,
          reason: "session unavailable",
        });
      }

      try {
        const result = await processAgentPromptControl(deps, {
          identity: terminalViewerIdentity(
            caller.fingerprint,
            context.values.get(tabIdKey),
            context.values.get(remoteAddressKey),
            actor.dashboardId,
          ),
          sessionId: validated.sessionId,
          inputSeq: nextCompatibilityInputSeq(),
          expectedStatusEpoch: validated.expectedStatusEpoch,
          expectedOccupantId: validated.expectedOccupantId,
          expectedRevision: validated.expectedRevision,
          text: validated.text,
          wait: validated.wait,
        }, context.signal);
        log.info("agent-prompt", "completed", {
          session_id: validated.sessionId,
          status_epoch: validated.expectedStatusEpoch,
          occupant_id: validated.expectedOccupantId,
          input_outcome: result.input.status,
          wait_outcome: result.waitOutcome,
        });
        return promptResponse(result.input, result.waitOutcome);
      } catch (error) {
        remapAgentPromptWaitError(error);
      }
    },
  };
}

function validateAgentPromptRequest(
  request: SessionsPromptRequest,
): ValidatedAgentPrompt {
  const sessionId = SessionId.safeParse(request.sessionId);
  const statusEpoch = StatusEpoch.safeParse(request.expectedStatusEpoch);
  const occupantId = AgentOccupantId.safeParse(request.expectedOccupantId);
  const text = AgentPromptTextSchema.safeParse(request.text);
  const uniqueStates = new Set(request.waitStates);
  const parsedStates = request.waitStates.map((state) => AgentRuntimeState.safeParse(state));
  const hasWaitStates = request.waitStates.length > 0;
  const hasWaitTimeout = request.waitTimeoutMs !== undefined;
  const waitTimeout = hasWaitTimeout
    ? AgentPromptWaitTimeoutMsSchema.safeParse(request.waitTimeoutMs)
    : undefined;
  if (
    !sessionId.success
    || !statusEpoch.success
    || !occupantId.success
    || request.expectedRevision < 0n
    || request.expectedRevision > BigInt(Number.MAX_SAFE_INTEGER)
    || !text.success
    || hasWaitStates !== hasWaitTimeout
    || uniqueStates.size !== request.waitStates.length
    || parsedStates.some((state) => !state.success)
    || (waitTimeout !== undefined && !waitTimeout.success)
  ) {
    throw new ConnectError("invalid agent prompt request", Code.InvalidArgument);
  }
  return {
    sessionId: sessionId.data,
    expectedStatusEpoch: statusEpoch.data,
    expectedOccupantId: occupantId.data,
    expectedRevision: Number(request.expectedRevision),
    text: text.data,
    ...(hasWaitStates ? {
      wait: {
        states: parsedStates.map((state) => state.data!),
        timeoutMs: request.waitTimeoutMs!,
      },
    } : {}),
  };
}

const WAIT_OUTCOME_MEMBERS: Record<AgentPromptWaitOutcomeName, AgentPromptWaitOutcome> = {
  matched: AgentPromptWaitOutcome.MATCHED,
  timed_out: AgentPromptWaitOutcome.TIMED_OUT,
  occupant_changed: AgentPromptWaitOutcome.OCCUPANT_CHANGED,
  session_closed: AgentPromptWaitOutcome.SESSION_CLOSED,
  prompt_stalled: AgentPromptWaitOutcome.PROMPT_STALLED,
};

/**
 * Every definite rejection the worker or this coordinator can produce, mapped
 * to the one member a caller may branch on: `blocked` means stop and send keys
 * interactively, a changed fence or process means re-read status and retry, and
 * `expired` means the same request is still valid. The free-text reason stays
 * private, so an unmapped cause reports no member rather than leaking one.
 */
const REJECTION_MEMBERS: Record<string, AgentPromptRejection> = {
  "agent is blocked": AgentPromptRejection.BLOCKED,
  "agent status source is not integration": AgentPromptRejection.NOT_PROMPTABLE,
  "agent state does not admit prompts": AgentPromptRejection.NOT_PROMPTABLE,
  "agent is not the terminal foreground process": AgentPromptRejection.NOT_FOREGROUND,
  "agent status is unavailable": AgentPromptRejection.FENCE_CHANGED,
  "agent status fence changed": AgentPromptRejection.FENCE_CHANGED,
  "agent process proof could not be refreshed": AgentPromptRejection.PROCESS_CHANGED,
  "agent process proof changed before prompt admission": AgentPromptRejection.PROCESS_CHANGED,
  "agent process proof changed before the keeper write": AgentPromptRejection.PROCESS_CHANGED,
  "session unavailable": AgentPromptRejection.SESSION_UNAVAILABLE,
  "session is not live": AgentPromptRejection.SESSION_UNAVAILABLE,
  "session changed before prompt admission": AgentPromptRejection.SESSION_UNAVAILABLE,
  "session changed before the keeper write": AgentPromptRejection.SESSION_UNAVAILABLE,
  "terminal input mode could not be read": AgentPromptRejection.SESSION_UNAVAILABLE,
  "worker connection was superseded": AgentPromptRejection.SESSION_UNAVAILABLE,
  "unknown session": AgentPromptRejection.SESSION_UNAVAILABLE,
  "worker unavailable": AgentPromptRejection.SESSION_UNAVAILABLE,
  "terminal dashboard scope is unavailable": AgentPromptRejection.SESSION_UNAVAILABLE,
  "coordinator is not write-active": AgentPromptRejection.SESSION_UNAVAILABLE,
  "coordinator move in progress": AgentPromptRejection.SESSION_UNAVAILABLE,
  "prompt budget expired": AgentPromptRejection.EXPIRED,
  "prompt budget could not be verified": AgentPromptRejection.EXPIRED,
  "prompt budget cannot cover the submit delay": AgentPromptRejection.EXPIRED,
  "input budget expired before worker send": AgentPromptRejection.EXPIRED,
  "keeper rejected the agent prompt": AgentPromptRejection.KEEPER_REJECTED,
  "keeper did not admit the agent prompt": AgentPromptRejection.KEEPER_REJECTED,
  "keeper rejected input": AgentPromptRejection.KEEPER_REJECTED,
  "prompt admission could not be verified": AgentPromptRejection.KEEPER_REJECTED,
  "generation closed or control queue full": AgentPromptRejection.KEEPER_REJECTED,
};

function promptResponse(
  input: TerminalWriteControlResult,
  waitOutcome?: AgentPromptWaitOutcomeName,
): SessionsPromptResponse {
  const inputOutcome = input.status === "accepted"
    ? AgentPromptInputOutcome.ACCEPTED
    : input.status === "rejected"
      ? AgentPromptInputOutcome.REJECTED
      : AgentPromptInputOutcome.AMBIGUOUS;
  return create(SessionsPromptResponseSchema, {
    inputOutcome,
    writtenBytes: input.writtenBytes,
    reason: input.status === "accepted"
      ? ""
      : input.status === "rejected"
        ? "agent prompt rejected"
        : "agent prompt outcome is ambiguous",
    waitOutcome: input.status === "rejected" || waitOutcome === undefined
      ? undefined
      : WAIT_OUTCOME_MEMBERS[waitOutcome],
    rejection: input.status === "rejected" ? rejectionMember(input.reason, input.sessionId) : undefined,
  });
}

function rejectionMember(
  reason: string,
  sessionId: string,
): AgentPromptRejection | undefined {
  const member = REJECTION_MEMBERS[reason];
  // The reason itself is private, so record only that a cause went unclassified.
  if (member === undefined) {
    log.warn("agent-prompt", "rejection_unmapped", {
      session_id: sessionId,
      reason_length: reason.length,
    });
  }
  return member;
}

function remapAgentPromptWaitError(error: unknown): never {
  if (!(error instanceof AgentStatusWaitError)) throw error;
  const code = error.kind === "invalid"
    ? Code.InvalidArgument
    : error.kind === "capacity"
      ? Code.ResourceExhausted
      : Code.Canceled;
  throw new ConnectError(error.message, code);
}

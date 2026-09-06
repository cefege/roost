// SessionsPrompt authorizes an open dashboard session, validates every public
// fence and bound, then delegates exactly-once write/wait orchestration.
// Responses expose only bounded outcomes; prompt text and agent status messages
// never enter coordinator logs, audits, or durable storage.

import { create } from "@bufbuild/protobuf";
import { Code, ConnectError, type ServiceImpl } from "@connectrpc/connect";
import {
  AgentPromptInputOutcome,
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
import { AgentStatusWaitError } from "../agent-status-hub.ts";
import {
  processAgentPromptControl,
  type AgentPromptWaitConfig,
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

function promptResponse(
  input: TerminalWriteControlResult,
  waitOutcome?: "matched" | "timed_out" | "occupant_changed" | "session_closed",
): SessionsPromptResponse {
  const inputOutcome = input.status === "accepted"
    ? AgentPromptInputOutcome.ACCEPTED
    : input.status === "rejected"
      ? AgentPromptInputOutcome.REJECTED
      : AgentPromptInputOutcome.AMBIGUOUS;
  const mappedWaitOutcome = waitOutcome === undefined
    ? undefined
    : waitOutcome === "matched"
      ? AgentPromptWaitOutcome.MATCHED
      : waitOutcome === "timed_out"
        ? AgentPromptWaitOutcome.TIMED_OUT
        : waitOutcome === "occupant_changed"
          ? AgentPromptWaitOutcome.OCCUPANT_CHANGED
          : AgentPromptWaitOutcome.SESSION_CLOSED;
  return create(SessionsPromptResponseSchema, {
    inputOutcome,
    writtenBytes: input.writtenBytes,
    reason: input.status === "accepted"
      ? ""
      : input.status === "rejected"
        ? "agent prompt rejected"
        : "agent prompt outcome is ambiguous",
    waitOutcome: input.status === "rejected" ? undefined : mappedWaitOutcome,
  });
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

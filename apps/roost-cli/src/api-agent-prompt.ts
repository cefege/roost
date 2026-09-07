// Parses and dispatches the status-fenced agent-prompt API command.
// The command pins one coordinator-observed integration occupant before sending
// exactly one prompt, then prints stable input and optional wait result lines.

import {
  AgentPromptInputOutcome,
  AgentPromptRejection,
  AgentPromptWaitOutcome,
  type AgentStatusView,
} from "@roost/shared/proto/coordinator_pb";
import {
  AGENT_PROMPT_MAX_REASON_LENGTH,
  AGENT_PROMPT_MAX_TEXT_BYTES,
  AGENT_PROMPT_MAX_WRITE_BYTES,
  AgentPromptTextSchema,
} from "@roost/shared/terminal-input";
import { AgentOccupantId, StatusEpoch } from "@roost/shared/wire";
import {
  parseAgentWaitDuration,
  parseAgentWaitStates,
} from "./api-agent-status.ts";

export interface AgentPromptApiClient {
  agentStatusGet(request: { sessionId: string }): Promise<{ status?: AgentStatusView }>;
  sessionsPrompt(request: {
    sessionId: string;
    expectedStatusEpoch: string;
    expectedOccupantId: string;
    expectedRevision: bigint;
    text: string;
    waitStates: string[];
    waitTimeoutMs?: number;
  }): Promise<{
    inputOutcome: AgentPromptInputOutcome;
    writtenBytes: number;
    reason: string;
    waitOutcome?: AgentPromptWaitOutcome;
    rejection?: AgentPromptRejection;
  }>;
}

export interface ParsedAgentPromptArgs {
  sessionId: string;
  text: string;
  wait: {
    states: string[];
    timeoutMs: number;
  } | null;
}

export type AgentPromptLineWriter = (line: string) => void;
export type AgentPromptExitWriter = (code: number) => void;

export function parseAgentPromptArgs(args: readonly string[]): ParsedAgentPromptArgs {
  const sessionId = args[0];
  if (!sessionId || sessionId.startsWith("--")) {
    throw new Error("agent-prompt: missing <session>");
  }
  const text = args[1];
  if (text === undefined) throw new Error("agent-prompt: missing <text>");
  if (!AgentPromptTextSchema.safeParse(text).success) {
    throw new Error(
      `agent-prompt: <text> must be nonempty and at most ${AGENT_PROMPT_MAX_TEXT_BYTES} UTF-8 bytes`,
    );
  }

  let waitRequested = false;
  let untilValue: string | undefined;
  let timeoutValue: string | undefined;
  for (let index = 2; index < args.length; index += 1) {
    const argument = args[index]!;
    if (argument === "--wait") {
      if (waitRequested) throw new Error("agent-prompt: duplicate --wait");
      waitRequested = true;
      continue;
    }
    if (argument === "--until" || argument === "--timeout") {
      const value = args[index + 1];
      if (!value || value.startsWith("--")) {
        throw new Error(`agent-prompt: ${argument} requires a value`);
      }
      if (argument === "--until") {
        if (untilValue !== undefined) throw new Error("agent-prompt: duplicate --until");
        untilValue = value;
      } else {
        if (timeoutValue !== undefined) throw new Error("agent-prompt: duplicate --timeout");
        timeoutValue = value;
      }
      index += 1;
      continue;
    }
    if (argument.startsWith("--until=")) {
      if (untilValue !== undefined) throw new Error("agent-prompt: duplicate --until");
      untilValue = argument.slice("--until=".length);
      continue;
    }
    if (argument.startsWith("--timeout=")) {
      if (timeoutValue !== undefined) throw new Error("agent-prompt: duplicate --timeout");
      timeoutValue = argument.slice("--timeout=".length);
      continue;
    }
    throw new Error("agent-prompt: unexpected argument");
  }

  const anyWaitOption = waitRequested || untilValue !== undefined || timeoutValue !== undefined;
  const completeWait = waitRequested && untilValue !== undefined && timeoutValue !== undefined;
  if (anyWaitOption && !completeWait) {
    throw new Error("agent-prompt: --wait, --until, and --timeout must be provided together");
  }
  return {
    sessionId,
    text,
    wait: waitRequested && untilValue !== undefined && timeoutValue !== undefined ? {
      states: parseAgentWaitStates(untilValue, "agent-prompt"),
      timeoutMs: parseAgentWaitDuration(timeoutValue, "agent-prompt"),
    } : null,
  };
}

export const AGENT_PROMPT_API_COMMAND = {
  verb: "agent-prompt",
  usage: "roost api agent-prompt <session> <text> [--wait --until <states> --timeout <duration>]",
  parseArgs: parseAgentPromptArgs,
} as const;

export async function dispatchAgentPromptApi(
  client: AgentPromptApiClient,
  verb: string,
  args: readonly string[],
  writeLine: AgentPromptLineWriter = (line) => console.log(line),
  writeExitCode: AgentPromptExitWriter = (code) => {
    process.exitCode = code;
  },
): Promise<boolean> {
  if (verb !== AGENT_PROMPT_API_COMMAND.verb) return false;
  const parsed = parseAgentPromptArgs(args);
  const current = await client.agentStatusGet({ sessionId: parsed.sessionId });
  const status = current.status;
  if (!status) throw new Error("agent-prompt: coordinator returned an empty status response");
  const fence = promptFence(status, parsed.sessionId);
  const response = await client.sessionsPrompt({
    sessionId: parsed.sessionId,
    expectedStatusEpoch: fence.statusEpoch,
    expectedOccupantId: fence.occupantId,
    expectedRevision: fence.revision,
    text: parsed.text,
    waitStates: parsed.wait?.states ?? [],
    ...(parsed.wait ? { waitTimeoutMs: parsed.wait.timeoutMs } : {}),
  });

  const inputOutcome = INPUT_OUTCOME_NAMES[response.inputOutcome];
  if (inputOutcome === undefined) invalidPromptResponse();
  if (
    !Number.isSafeInteger(response.writtenBytes)
    || response.writtenBytes < 0
    || response.writtenBytes > AGENT_PROMPT_MAX_WRITE_BYTES
    || (inputOutcome === "accepted" && response.writtenBytes === 0)
    || (inputOutcome === "rejected" && response.writtenBytes !== 0)
  ) {
    invalidPromptResponse();
  }
  const waitOutcome = response.waitOutcome === undefined
    ? undefined
    : WAIT_OUTCOME_NAMES[response.waitOutcome];
  if (response.waitOutcome !== undefined && waitOutcome === undefined) invalidPromptResponse();
  const waitExpected = parsed.wait !== null && inputOutcome !== "rejected";
  if ((waitOutcome !== undefined) !== waitExpected) invalidPromptResponse();
  const rejection = response.rejection === undefined
    ? undefined
    : REJECTION_NAMES[response.rejection];
  if (
    response.rejection !== undefined
    && (rejection === undefined || inputOutcome !== "rejected")
  ) {
    invalidPromptResponse();
  }

  // A rejection cause is already a bounded member; only the ambiguous path
  // still carries coordinator text that has to be sanitized before printing.
  const reason = rejection ?? printableReason(response.reason, parsed.text, status.message);
  writeLine(`input\t${inputOutcome}\t${response.writtenBytes}\t${reason}`);
  if (waitOutcome !== undefined) writeLine(`wait\t${waitOutcome}`);
  if (inputOutcome !== "accepted" || (waitOutcome !== undefined && waitOutcome !== "matched")) {
    writeExitCode(1);
  }
  return true;
}

type InputOutcomeName = "accepted" | "rejected" | "ambiguous";
type WaitOutcomeName =
  | "matched"
  | "timed_out"
  | "occupant_changed"
  | "session_closed"
  | "prompt_stalled";
type RejectionName =
  | "blocked"
  | "not_promptable"
  | "not_foreground"
  | "fence_changed"
  | "process_changed"
  | "session_unavailable"
  | "expired"
  | "keeper_rejected";

const INPUT_OUTCOME_NAMES: Record<AgentPromptInputOutcome, InputOutcomeName | undefined> = {
  [AgentPromptInputOutcome.UNSPECIFIED]: undefined,
  [AgentPromptInputOutcome.ACCEPTED]: "accepted",
  [AgentPromptInputOutcome.REJECTED]: "rejected",
  [AgentPromptInputOutcome.AMBIGUOUS]: "ambiguous",
};
const WAIT_OUTCOME_NAMES: Record<AgentPromptWaitOutcome, WaitOutcomeName | undefined> = {
  [AgentPromptWaitOutcome.UNSPECIFIED]: undefined,
  [AgentPromptWaitOutcome.MATCHED]: "matched",
  [AgentPromptWaitOutcome.TIMED_OUT]: "timed_out",
  [AgentPromptWaitOutcome.OCCUPANT_CHANGED]: "occupant_changed",
  [AgentPromptWaitOutcome.SESSION_CLOSED]: "session_closed",
  [AgentPromptWaitOutcome.PROMPT_STALLED]: "prompt_stalled",
};
const REJECTION_NAMES: Record<AgentPromptRejection, RejectionName | undefined> = {
  [AgentPromptRejection.UNSPECIFIED]: undefined,
  [AgentPromptRejection.BLOCKED]: "blocked",
  [AgentPromptRejection.NOT_PROMPTABLE]: "not_promptable",
  [AgentPromptRejection.NOT_FOREGROUND]: "not_foreground",
  [AgentPromptRejection.FENCE_CHANGED]: "fence_changed",
  [AgentPromptRejection.PROCESS_CHANGED]: "process_changed",
  [AgentPromptRejection.SESSION_UNAVAILABLE]: "session_unavailable",
  [AgentPromptRejection.EXPIRED]: "expired",
  [AgentPromptRejection.KEEPER_REJECTED]: "keeper_rejected",
};

function promptFence(
  status: AgentStatusView,
  sessionId: string,
): { statusEpoch: string; occupantId: string; revision: bigint } {
  if (status.sessionId !== sessionId) invalidPromptResponse();
  if (!status.statusEpoch || !status.occupantId) {
    throw new Error("agent-prompt: current agent status has no occupant identity");
  }
  if (
    !StatusEpoch.safeParse(status.statusEpoch).success
    || !AgentOccupantId.safeParse(status.occupantId).success
  ) {
    throw new Error("agent-prompt: current agent status has invalid occupant identity");
  }
  if (!status.promptable || status.source !== "integration") {
    throw new Error("agent-prompt: current agent status is not promptable");
  }
  if (
    typeof status.revision !== "bigint"
    || status.revision < 0n
    || status.revision > BigInt(Number.MAX_SAFE_INTEGER)
  ) {
    throw new Error("agent-prompt: current agent status revision is not a safe integer");
  }
  return {
    statusEpoch: status.statusEpoch,
    occupantId: status.occupantId,
    revision: status.revision,
  };
}

function printableReason(
  reason: unknown,
  promptText: string,
  statusMessage: string | undefined,
): string {
  if (typeof reason !== "string" || reason.length === 0) return "-";
  if (
    reason.length > AGENT_PROMPT_MAX_REASON_LENGTH
    || !/^[\x20-\x7e]+$/.test(reason)
    || reason.includes(promptText)
    || (statusMessage !== undefined && statusMessage.length > 0 && reason.includes(statusMessage))
  ) {
    return "-";
  }
  return reason;
}

function invalidPromptResponse(): never {
  throw new Error("agent-prompt: coordinator returned an invalid response");
}

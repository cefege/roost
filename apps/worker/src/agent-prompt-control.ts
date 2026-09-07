// Owns status-fenced agent prompt admission from a coordinator request to the
// acknowledged keeper writes that carry it. It depends on the worker-private
// process proof, the pane's foreground job, and the same terminal-input lane
// and text encoder as interactive input.

import type { DAgentPrompt } from "@roost/shared/proto/worker_transport_pb";
import { AgentPromptTextSchema, buildPtyPayload } from "@roost/shared/terminal-input";
import {
  AgentOccupantId,
  SessionId,
  StatusEpoch,
} from "@roost/shared/wire";
import type { AgentScreenDetector } from "./agent-status/detector.ts";
import type {
  AgentStatusPrivateProof,
  AgentStatusRegistry,
} from "./agent-status/registry.ts";
import type { AgentProcessIdentity } from "./agent-status/process-scan.ts";
import { agentOwnsTerminalForeground } from "./agent-status/process-tree.ts";
import {
  PROMPT_SUBMIT_DELAY_MS,
  submitAgentPrompt,
} from "./agent-prompt-submit.ts";
import { acquireKeeperAdmission } from "./session-control-lanes.ts";
import type { SessionManager } from "./session-manager.ts";
import type { SessionRecord } from "./session-record.ts";
import type { WorkerInputResult } from "./session-terminal-control.ts";
import type { TerminalRequestBudget } from "./transport/coord-link-types.ts";
import { TERMINAL_REQUEST_BUDGET_CAP_MS } from "./transport/coord-link-constants.ts";

const REQUEST_ID_MAX_LENGTH = 128;
const UINT64_MAX = (1n << 64n) - 1n;
const MAX_SAFE_REVISION = BigInt(Number.MAX_SAFE_INTEGER);
const NOT_FOREGROUND_REASON = "agent is not the terminal foreground process";

export interface AgentPromptControlDeps {
  sessions: SessionManager;
  registry: Pick<AgentStatusRegistry, "currentPrivateProof">;
  detector: Pick<AgentScreenDetector, "reportingAgentForSession">;
}

interface ValidatedPromptRequest {
  expectedRevision: number;
}

type BudgetCheck =
  | { ok: true; remainingMs: number }
  | { ok: false; reason: string };

type ProcessProofRefresh =
  | { kind: "refreshed"; proof: AgentProcessIdentity | null }
  | { kind: "budget_failure"; reason: string };

function rejected(reason: string): WorkerInputResult {
  return { status: "rejected", writtenBytes: 0, reason };
}

function validateRequest(request: DAgentPrompt): ValidatedPromptRequest | WorkerInputResult {
  if (typeof request.requestId !== "string"
      || request.requestId.length === 0
      || request.requestId.length > REQUEST_ID_MAX_LENGTH) {
    return rejected("request_id is invalid");
  }
  if (!SessionId.safeParse(request.sessionId).success) {
    return rejected("session_id must be a UUID");
  }
  if (typeof request.inputSeq !== "bigint"
      || request.inputSeq <= 0n
      || request.inputSeq > UINT64_MAX) {
    return rejected("input sequence must be a positive uint64");
  }
  if (!StatusEpoch.safeParse(request.expectedStatusEpoch).success) {
    return rejected("expected_status_epoch must be a UUID");
  }
  if (!AgentOccupantId.safeParse(request.expectedOccupantId).success) {
    return rejected("expected_occupant_id must be a UUID");
  }
  if (typeof request.expectedRevision !== "bigint"
      || request.expectedRevision < 0n
      || request.expectedRevision > MAX_SAFE_REVISION) {
    return rejected("expected_revision must be a safe uint64");
  }
  if (!AgentPromptTextSchema.safeParse(request.text).success) {
    return rejected("prompt text is invalid");
  }
  if (!Number.isInteger(request.budgetMs)
      || request.budgetMs < 1
      || request.budgetMs > TERMINAL_REQUEST_BUDGET_CAP_MS) {
    return rejected("budget_ms is invalid");
  }
  return { expectedRevision: Number(request.expectedRevision) };
}

function checkBudget(budget: TerminalRequestBudget): BudgetCheck {
  try {
    if (!budget.isCurrentConnection()) {
      return { ok: false, reason: "worker connection was superseded" };
    }
    const remainingMs = budget.remainingMs();
    if (!Number.isFinite(remainingMs)) {
      return { ok: false, reason: "prompt budget could not be verified" };
    }
    if (remainingMs <= 0) return { ok: false, reason: "prompt budget expired" };
    return { ok: true, remainingMs };
  } catch {
    return { ok: false, reason: "prompt budget could not be verified" };
  }
}

function processProofMatches(
  proof: AgentProcessIdentity,
  expected: AgentProcessIdentity,
): boolean {
  return proof.agentId === expected.agentId && proof.pid === expected.pid;
}

function statusFenceFailure(
  proof: AgentStatusPrivateProof | null,
  request: DAgentPrompt,
  expectedRevision: number,
): string | null {
  if (!proof) return "agent status is unavailable";
  if (proof.statusEpoch !== request.expectedStatusEpoch
      || proof.occupantId !== request.expectedOccupantId
      || proof.revision !== expectedRevision) {
    return "agent status fence changed";
  }
  if (proof.source !== "integration") return "agent status source is not integration";
  if (proof.state === "blocked") return "agent is blocked";
  if (proof.state !== "idle" && proof.state !== "working") {
    return "agent state does not admit prompts";
  }
  return null;
}

function exactSession(
  sessions: SessionManager,
  sessionId: string,
  expected: SessionRecord,
): boolean {
  return sessions.getBySessionId(sessionId) === expected
    && sessions.sessions.get(expected.channelId) === expected;
}

function readStatusProof(
  registry: Pick<AgentStatusRegistry, "currentPrivateProof">,
  sessionId: string,
): AgentStatusPrivateProof | null {
  try {
    return registry.currentPrivateProof(sessionId);
  } catch {
    return null;
  }
}

async function refreshProcessProof(
  detector: Pick<AgentScreenDetector, "reportingAgentForSession">,
  sessionId: string,
  expected: AgentProcessIdentity,
  budget: TerminalRequestBudget,
): Promise<ProcessProofRefresh> {
  const scanBudget = checkBudget(budget);
  if (!scanBudget.ok) {
    return { kind: "budget_failure", reason: scanBudget.reason };
  }
  const controller = new AbortController();
  const expiredResult = { kind: "expired" } as const;
  const { promise: expired, resolve: resolveExpired } =
    Promise.withResolvers<typeof expiredResult>();
  const timer = setTimeout(() => {
    controller.abort();
    resolveExpired(expiredResult);
  }, scanBudget.remainingMs);
  const scan = (async () => {
    try {
      const refreshed = await detector.reportingAgentForSession(
        sessionId,
        expected.pid,
        controller.signal,
      );
      const proof = refreshed && processProofMatches(refreshed, expected)
        ? {
            agentId: refreshed.agentId,
            pid: refreshed.pid,
            foreground: refreshed.foreground,
          }
        : null;
      return { kind: "refreshed", proof } as const;
    } catch {
      return { kind: "refreshed", proof: null } as const;
    }
  })();
  try {
    const outcome = await Promise.race([scan, expired]);
    if (outcome.kind === "expired") {
      return { kind: "budget_failure", reason: "prompt budget expired" };
    }
    const completedBudget = checkBudget(budget);
    return completedBudget.ok
      ? outcome
      : { kind: "budget_failure", reason: completedBudget.reason };
  } finally {
    clearTimeout(timer);
  }
}

async function waitForAdmissionGrant(
  granted: Promise<void>,
  budget: TerminalRequestBudget,
): Promise<string | null> {
  const admissionBudget = checkBudget(budget);
  if (!admissionBudget.ok) return admissionBudget.reason;
  const expiredResult = { kind: "expired" } as const;
  const { promise: expired, resolve: resolveExpired } =
    Promise.withResolvers<typeof expiredResult>();
  const timer = setTimeout(
    () => resolveExpired(expiredResult),
    admissionBudget.remainingMs,
  );
  const admission = granted.then(
    () => ({ kind: "granted" }) as const,
    () => ({ kind: "failed" }) as const,
  );
  try {
    const outcome = await Promise.race([admission, expired]);
    if (outcome.kind === "expired") return "prompt budget expired";
    if (outcome.kind === "failed") return "prompt admission could not be verified";
    const completedBudget = checkBudget(budget);
    return completedBudget.ok ? null : completedBudget.reason;
  } finally {
    clearTimeout(timer);
  }
}

/** Admit exactly one prompt. Every return before the first keeper write is a
 * proven zero-write rejection; from there the keeper results are the truth. */
export async function writeAgentPrompt(
  request: DAgentPrompt,
  budget: TerminalRequestBudget,
  deps: AgentPromptControlDeps,
): Promise<WorkerInputResult> {
  const validated = validateRequest(request);
  if ("status" in validated) return validated;
  const initialBudget = checkBudget(budget);
  if (!initialBudget.ok) return rejected(initialBudget.reason);

  const expectedRecord = deps.sessions.getBySessionId(request.sessionId);
  if (!expectedRecord) return rejected("session is not live");
  const initialStatus = readStatusProof(deps.registry, request.sessionId);
  const initialFenceFailure = statusFenceFailure(
    initialStatus,
    request,
    validated.expectedRevision,
  );
  if (initialFenceFailure) return rejected(initialFenceFailure);

  const channelId = expectedRecord.channelId;
  const ticket = acquireKeeperAdmission(deps.sessions, channelId, "terminal_input");
  try {
    const firstRefresh = await refreshProcessProof(
      deps.detector,
      request.sessionId,
      initialStatus!.process,
      budget,
    );
    if (firstRefresh.kind === "budget_failure") return rejected(firstRefresh.reason);
    const firstProcessProof = firstRefresh.proof;
    if (!firstProcessProof) {
      return rejected("agent process proof could not be refreshed");
    }
    if (!agentOwnsTerminalForeground(firstProcessProof.foreground)) {
      return rejected(NOT_FOREGROUND_REASON);
    }
    if (!exactSession(deps.sessions, request.sessionId, expectedRecord)) {
      return rejected("session changed before prompt admission");
    }
    const preAdmissionStatus = readStatusProof(deps.registry, request.sessionId);
    const preAdmissionFenceFailure = statusFenceFailure(
      preAdmissionStatus,
      request,
      validated.expectedRevision,
    );
    if (preAdmissionFenceFailure) return rejected(preAdmissionFenceFailure);
    if (!processProofMatches(preAdmissionStatus!.process, firstProcessProof)) {
      return rejected("agent process proof changed before prompt admission");
    }
    const preAdmissionBudget = checkBudget(budget);
    if (!preAdmissionBudget.ok) return rejected(preAdmissionBudget.reason);

    const admissionFailure = await waitForAdmissionGrant(ticket.granted, budget);
    if (admissionFailure) return rejected(admissionFailure);
    const finalRefresh = await refreshProcessProof(
      deps.detector,
      request.sessionId,
      firstProcessProof,
      budget,
    );
    if (finalRefresh.kind === "budget_failure") return rejected(finalRefresh.reason);
    const finalProcessProof = finalRefresh.proof;

    let payload: Uint8Array;
    try {
      payload = buildPtyPayload(request.text, expectedRecord.wtermCore.bracketedPaste());
    } catch {
      return rejected("terminal input mode could not be read");
    }
    if (!exactSession(deps.sessions, request.sessionId, expectedRecord)) {
      return rejected("session changed before the keeper write");
    }
    const finalStatus = readStatusProof(deps.registry, request.sessionId);
    const finalFenceFailure = statusFenceFailure(
      finalStatus,
      request,
      validated.expectedRevision,
    );
    if (finalFenceFailure) return rejected(finalFenceFailure);
    if (!finalProcessProof
        || !processProofMatches(finalProcessProof, firstProcessProof)
        || !processProofMatches(finalStatus!.process, finalProcessProof)) {
      return rejected("agent process proof changed before the keeper write");
    }
    if (!agentOwnsTerminalForeground(finalProcessProof.foreground)) {
      return rejected(NOT_FOREGROUND_REASON);
    }
    const finalBudget = checkBudget(budget);
    if (!finalBudget.ok) return rejected(finalBudget.reason);
    // The CR is a second write PROMPT_SUBMIT_DELAY_MS after the text, so a
    // budget that cannot cover it would strand the text as an unsubmitted
    // draft with no way to finish the submission.
    if (finalBudget.remainingMs <= PROMPT_SUBMIT_DELAY_MS) {
      return rejected("prompt budget cannot cover the submit delay");
    }
    deps.sessions.markInputSensitive(channelId);
    return await submitAgentPrompt(channelId, payload, () => checkBudget(budget).ok);
  } catch {
    return rejected("prompt admission could not be verified");
  } finally {
    ticket.release();
  }
}

// Owns the pinned OMP conversation-resume plan and its single acknowledged
// PTY write after an ordinary replacement shell is durably admitted. Boot
// reconciliation supplies only private recovery metadata and a live session.

import {
  AgentConversationReferenceV1Schema,
  type AgentConversationReferenceV1,
} from "@roost/shared/agent-conversation-reference";
import {
  supportedHostPlatform,
  type SupportedHostPlatform,
} from "@roost/shared/platform";
import { posixShellQuote } from "@roost/shared/shell-quote";
import { log } from "@roost/shared/log";
import type { SessionManager } from "./session-manager.ts";
import {
  writeWorkerOwnedTerminalInput,
  type WorkerInputResult,
} from "./session-terminal-control.ts";

const UTF8_ENCODER = new TextEncoder();
const CARRIAGE_RETURN = "\r";
const MAX_SESSION_ID_LENGTH = 512;
const MAX_SESSION_PATH_LENGTH = 4096;
// A control character can be consumed by the line discipline or the agent's
// own editor before the shell parser sees the closing quote.
const CONTROL_CHARACTER_RE = /[\u0000-\u001f\u007f-\u009f]/u;

/** `omp` exposes resume as `-r, --resume=<value>` and has no `--session`
 * flag, so the reference travels as one `--resume=` token. */
export const OMP_CONVERSATION_RESUME_DESCRIPTOR_V1 = Object.freeze({
  schema_version: 1 as const,
  agent_id: "omp" as const,
  executable: "omp" as const,
  fixed_option_prefix: "--resume=" as const,
  reference_kinds: Object.freeze(["id", "path"] as const),
  platforms: Object.freeze(["darwin", "linux"] as const),
});

export type AgentConversationRestoreOutcome =
  | WorkerInputResult
  | {
      status: "skipped";
      reason: "disabled" | "missing_reference" | "unsupported" | "duplicate";
    };

/** Two sessions holding the same reference cannot both resume it: the second
 * agent process would attach to a conversation another one already owns. */
export function conversationRestoreDedupeKey(
  reference: AgentConversationReferenceV1,
): string {
  return `${reference.agent_id}\u0000${reference.kind}\u0000${reference.value}`;
}

export interface AgentConversationRestoreDeps {
  enabled: boolean;
  sessionMgr: SessionManager;
  platform?: SupportedHostPlatform;
  /** Reference keys already claimed by an earlier session in this pass. */
  resumedReferenceKeys?: Set<string>;
}

/** Materialize the pinned OMP argv plan as one shell command line. Integration
 * data contributes a single quoted argument and can never supply executable or
 * option text. */
export function materializeOmpConversationRestoreInput(
  reference: AgentConversationReferenceV1,
  platform: SupportedHostPlatform,
): Uint8Array | null {
  const descriptor = OMP_CONVERSATION_RESUME_DESCRIPTOR_V1;
  if (!descriptor.platforms.some(
    (supportedPlatform) => supportedPlatform === platform,
  )) return null;
  const parsed = AgentConversationReferenceV1Schema.safeParse(reference);
  if (!parsed.success) return null;
  const checked = parsed.data;
  if (checked.agent_id !== descriptor.agent_id
      || !descriptor.reference_kinds.includes(checked.kind)
      || checked.value.length === 0
      || CONTROL_CHARACTER_RE.test(checked.value)) return null;
  if (checked.kind === "id" && checked.value.length > MAX_SESSION_ID_LENGTH) {
    return null;
  }
  if (checked.kind === "path"
      && (checked.value.length > MAX_SESSION_PATH_LENGTH
        || !checked.value.startsWith("/"))) return null;
  const argv = [
    descriptor.executable,
    `${descriptor.fixed_option_prefix}${checked.value}`,
  ];
  const command = argv.map(posixShellQuote).join(" ");
  return UTF8_ENCODER.encode(`${command}${CARRIAGE_RETURN}`);
}

/** Attempt one worker-owned input batch. Every result, including an internal
 * failure after admission, is terminal for automatic restoration. */
export async function restoreAgentConversationAfterRespawn(
  deps: AgentConversationRestoreDeps,
  sessionId: string,
  reference: AgentConversationReferenceV1 | null,
): Promise<AgentConversationRestoreOutcome> {
  if (!deps.enabled) {
    return recordRestoreOutcome(sessionId, {
      status: "skipped",
      reason: "disabled",
    });
  }
  if (!reference) {
    return recordRestoreOutcome(sessionId, {
      status: "skipped",
      reason: "missing_reference",
    });
  }
  const dedupeKey = conversationRestoreDedupeKey(reference);
  if (deps.resumedReferenceKeys?.has(dedupeKey)) {
    return recordRestoreOutcome(sessionId, {
      status: "skipped",
      reason: "duplicate",
    });
  }
  let payload: Uint8Array | null;
  try {
    payload = materializeOmpConversationRestoreInput(
      reference,
      deps.platform ?? supportedHostPlatform(),
    );
  } catch {
    payload = null;
  }
  if (!payload) {
    return recordRestoreOutcome(sessionId, {
      status: "skipped",
      reason: "unsupported",
    });
  }
  // The claim precedes the write: an ambiguous outcome may still have reached
  // the PTY, so no second session may resume the same conversation.
  deps.resumedReferenceKeys?.add(dedupeKey);
  let outcome: AgentConversationRestoreOutcome;
  try {
    outcome = await writeWorkerOwnedTerminalInput.call(
      deps.sessionMgr,
      sessionId,
      payload,
    );
  } catch {
    outcome = {
      status: "ambiguous",
      writtenBytes: 0,
      reason: "worker-owned restore input outcome is unavailable",
    };
  }
  return recordRestoreOutcome(sessionId, outcome);
}

function recordRestoreOutcome(
  sessionId: string,
  outcome: AgentConversationRestoreOutcome,
): AgentConversationRestoreOutcome {
  const fields = {
    sessionId,
    outcome: outcome.status,
    ...(outcome.status === "skipped"
      ? { skip_reason: outcome.reason }
      : {}),
  };
  if (outcome.status === "accepted" || outcome.status === "skipped") {
    log.info("worker", "agent_conversation_restore_transition", fields);
  } else {
    log.warn("worker", "agent_conversation_restore_transition", fields);
  }
  return outcome;
}

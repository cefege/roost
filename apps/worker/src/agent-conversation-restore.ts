// Owns the pinned OMP conversation-resume plan, its single acknowledged PTY
// write after an ordinary replacement shell is durably admitted, and the
// line-discard write that cancels a partly delivered resume command. Boot
// reconciliation supplies only private recovery metadata and a live session.

import {
  AGENT_CONVERSATION_CONTROL_CHARACTER_RE,
  AGENT_CONVERSATION_SESSION_ID_MAX_UTF8_BYTES,
  AGENT_CONVERSATION_SESSION_PATH_MAX_UTF8_BYTES,
  AgentConversationReferenceV1Schema,
  isAbsoluteAgentConversationSessionPath,
  type AgentConversationReferenceV1,
} from "@roost/shared/agent-conversation-reference";
import {
  supportedHostPlatform,
  type SupportedHostPlatform,
} from "@roost/shared/platform";
import { posixShellQuote } from "@roost/shared/shell-quote";
import { log } from "@roost/shared/log";
import { hasAtMostUtf8Bytes } from "@roost/shared/ui-state";
import type { SessionManager } from "./session-manager.ts";
import {
  writeWorkerOwnedTerminalInput,
  type WorkerInputResult,
} from "./session-terminal-control.ts";

const UTF8_ENCODER = new TextEncoder();
const CARRIAGE_RETURN = "\r";
// ETX discards the current input line in bash and zsh, in both emacs and vi
// editing modes, so a partially delivered resume command cannot stay on the
// prompt where one Enter would run a truncated `--resume=` id prefix.
const PARTIAL_INPUT_DISCARD_BYTES = Uint8Array.of(0x03);

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
      || AGENT_CONVERSATION_CONTROL_CHARACTER_RE.test(checked.value)) {
    return null;
  }
  if (checked.kind === "id" && !hasAtMostUtf8Bytes(
    checked.value,
    AGENT_CONVERSATION_SESSION_ID_MAX_UTF8_BYTES,
  )) return null;
  if (checked.kind === "path"
      && (!hasAtMostUtf8Bytes(
        checked.value,
        AGENT_CONVERSATION_SESSION_PATH_MAX_UTF8_BYTES,
      )
        || !isAbsoluteAgentConversationSessionPath(checked.value))) return null;
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
  let outcome: WorkerInputResult;
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
  if (outcome.status === "rejected") {
    // A rejection is proven pre-write with zero keeper bytes, so no agent
    // process can have attached: another session holding the same reference
    // must still be allowed to resume it in this pass.
    deps.resumedReferenceKeys?.delete(dedupeKey);
  }
  const recorded = recordRestoreOutcome(sessionId, outcome);
  if (outcome.status !== "accepted" && outcome.writtenBytes > 0) {
    await discardPartialRestoreInput(deps, sessionId, outcome);
  }
  return recorded;
}

/** Cancel — never re-send — a resume command the keeper only partly
 * delivered. `omp --resume=` matches an id PREFIX, so a truncated command left
 * on the prompt could attach one Enter to a different conversation. Discarding
 * the input line is the only remedy available here: the replacement shell has
 * already emitted a durable `respawned` event, so ending it would tombstone
 * the session and delete its coordinator row over a stray prompt line. */
async function discardPartialRestoreInput(
  deps: AgentConversationRestoreDeps,
  sessionId: string,
  partial: WorkerInputResult,
): Promise<void> {
  let discard: WorkerInputResult;
  try {
    discard = await writeWorkerOwnedTerminalInput.call(
      deps.sessionMgr,
      sessionId,
      PARTIAL_INPUT_DISCARD_BYTES,
    );
  } catch {
    discard = {
      status: "ambiguous",
      writtenBytes: 0,
      reason: "worker-owned restore discard outcome is unavailable",
    };
  }
  const fields = {
    sessionId,
    outcome: discard.status,
    partial_outcome: partial.status,
    partial_bytes: partial.writtenBytes,
  };
  if (discard.status === "accepted") {
    log.info("worker", "agent_conversation_restore_discard_transition", fields);
  } else {
    log.warn("worker", "agent_conversation_restore_discard_transition", fields);
  }
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

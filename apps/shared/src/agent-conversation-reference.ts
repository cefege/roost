// Owns the private, opaque agent-conversation reference contract and recovery fold.
// Every rule deciding whether a reference is resumable lives here — worker
// reporters, coordinator persistence, boot recovery and restore import these
// bounds instead of restating them and drifting from what was durably stored.
// References are equality/continuation data only and never belong in public Session state.

import { z } from "zod";
import { hasAtMostUtf8Bytes } from "./ui-state.ts";
import { SessionId } from "./wire/brand.ts";

export const AGENT_CONVERSATION_SESSION_ID_MAX_UTF8_BYTES = 512;
export const AGENT_CONVERSATION_SESSION_PATH_MAX_UTF8_BYTES = 4_096;
/** The Unicode control class (Cc). A reference carrying any of these cannot
 *  survive the shell command line that resumes it. */
export const AGENT_CONVERSATION_CONTROL_CHARACTER_RE = /[\u0000-\u001f\u007f-\u009f]/u;
export const AGENT_CONVERSATION_REFERENCE_EVENT_MAX_UTF8_BYTES = 8_192;

/** POSIX or Windows absolute, exactly `path.posix.isAbsolute(value)
 *  || path.win32.isAbsolute(value)`: a leading separator, or a drive letter
 *  followed by one. Spelled as a pattern because this module is reachable from
 *  the browser wire graph, where node:path externalizes to a stub that throws
 *  on first use. */
const ABSOLUTE_SESSION_PATH_RE = /^(?:[/\\]|[A-Za-z]:[/\\])/;

/** Both shapes count: the reporting agent may run on a host platform other
 *  than the one validating or storing the report. */
export function isAbsoluteAgentConversationSessionPath(value: string): boolean {
  return ABSOLUTE_SESSION_PATH_RE.test(value);
}

export const AgentConversationReferenceAgentIdSchema = z.literal("omp");
export type AgentConversationReferenceAgentId = z.infer<
  typeof AgentConversationReferenceAgentIdSchema
>;

export const AgentConversationReferenceV1Schema = z.object({
  schema_version: z.literal(1),
  agent_id: AgentConversationReferenceAgentIdSchema,
  kind: z.enum(["id", "path"]),
  value: z.string()
    .min(1, "agent conversation reference must not be empty")
    .refine(
      (value) => !AGENT_CONVERSATION_CONTROL_CHARACTER_RE.test(value),
      "agent conversation reference must not contain control characters",
    )
    .refine(
      isWellFormedUnicode,
      "agent conversation reference must be valid Unicode",
    ),
}).strict().superRefine((reference, context) => {
  const maxUtf8Bytes = reference.kind === "id"
    ? AGENT_CONVERSATION_SESSION_ID_MAX_UTF8_BYTES
    : AGENT_CONVERSATION_SESSION_PATH_MAX_UTF8_BYTES;
  if (!hasAtMostUtf8Bytes(reference.value, maxUtf8Bytes)) {
    context.addIssue({
      code: z.ZodIssueCode.custom,
      path: ["value"],
      message: `agent conversation ${reference.kind} must not exceed ${maxUtf8Bytes} UTF-8 bytes`,
    });
  }
  if (
    reference.kind === "path"
    && !isAbsoluteAgentConversationSessionPath(reference.value)
  ) {
    context.addIssue({
      code: z.ZodIssueCode.custom,
      path: ["value"],
      message: "agent conversation session path must be absolute",
    });
  }
}).readonly();
export type AgentConversationReferenceV1 = z.infer<
  typeof AgentConversationReferenceV1Schema
>;

// Sequence zero is the worker-list sentinel for a session with no reference
// event; every durable set/clear passed to the fold remains strictly positive.
export const AgentConversationRecoveryMetadataSchema = z.object({
  session_id: SessionId,
  agent_reference: AgentConversationReferenceV1Schema.nullable(),
  agent_reference_client_seq: z.number().int().nonnegative().max(Number.MAX_SAFE_INTEGER),
}).strict().superRefine((metadata, context) => {
  if (
    metadata.agent_reference !== null
    && metadata.agent_reference_client_seq === 0
  ) {
    context.addIssue({
      code: z.ZodIssueCode.custom,
      path: ["agent_reference_client_seq"],
      message: "stored agent conversation reference requires a positive sequence",
    });
  }
}).readonly();
export type AgentConversationRecoveryMetadata = z.infer<
  typeof AgentConversationRecoveryMetadataSchema
>;

export interface AgentConversationReferenceUpdate {
  readonly session_id: AgentConversationRecoveryMetadata["session_id"];
  readonly reference: AgentConversationReferenceV1 | null;
}

/** Apply one worker-ordered update; a stale set or clear cannot replace newer state. */
export function foldAgentConversationRecoveryMetadata(
  previous: AgentConversationRecoveryMetadata | null,
  event: AgentConversationReferenceUpdate,
  clientSeq: number,
): AgentConversationRecoveryMetadata {
  if (!Number.isSafeInteger(clientSeq) || clientSeq <= 0) {
    throw new RangeError("agent conversation reference client sequence must be positive");
  }
  if (previous && previous.session_id !== event.session_id) {
    throw new Error("agent conversation recovery session mismatch");
  }
  if (previous && clientSeq <= previous.agent_reference_client_seq) return previous;
  return AgentConversationRecoveryMetadataSchema.parse({
    session_id: event.session_id,
    agent_reference: event.reference,
    agent_reference_client_seq: clientSeq,
  });
}

/** Bound the exact durable JSON envelope after its individual fields are parsed. */
export function isAgentConversationReferenceEventEnvelopeBounded(
  event: Readonly<{ trace_id?: unknown }>,
): boolean {
  const traceId = event.trace_id;
  if (
    typeof traceId === "string"
    && traceId.length > AGENT_CONVERSATION_REFERENCE_EVENT_MAX_UTF8_BYTES
  ) return false;
  try {
    const serialized = JSON.stringify(event);
    return serialized !== undefined && hasAtMostUtf8Bytes(
      serialized,
      AGENT_CONVERSATION_REFERENCE_EVENT_MAX_UTF8_BYTES,
    );
  } catch {
    return false;
  }
}

function isWellFormedUnicode(value: string): boolean {
  for (let offset = 0; offset < value.length; offset++) {
    const codeUnit = value.charCodeAt(offset);
    if (codeUnit >= 0xdc00 && codeUnit <= 0xdfff) return false;
    if (codeUnit < 0xd800 || codeUnit > 0xdbff) continue;
    if (++offset >= value.length) return false;
    const trailing = value.charCodeAt(offset);
    if (trailing < 0xdc00 || trailing > 0xdfff) return false;
  }
  return true;
}

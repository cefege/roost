// Owns the private, opaque agent-conversation reference contract and recovery fold.
// Worker reporters, coordinator persistence, and boot recovery share these bounds.
// References are equality/continuation data only and never belong in public Session state.

import { z } from "zod";
import { hasAtMostUtf8Bytes } from "./ui-state.ts";
import { SessionId } from "./wire/brand.ts";

export const AGENT_CONVERSATION_REFERENCE_MAX_UTF8_BYTES = 4_096;
export const AGENT_CONVERSATION_REFERENCE_EVENT_MAX_UTF8_BYTES = 8_192;

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
      (value) => !value.includes("\0"),
      "agent conversation reference must not contain NUL",
    )
    .refine(
      isWellFormedUnicode,
      "agent conversation reference must be valid Unicode",
    )
    .refine(
      (value) => hasAtMostUtf8Bytes(
        value,
        AGENT_CONVERSATION_REFERENCE_MAX_UTF8_BYTES,
      ),
      `agent conversation reference must not exceed ${AGENT_CONVERSATION_REFERENCE_MAX_UTF8_BYTES} UTF-8 bytes`,
    ),
}).strict().readonly();
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

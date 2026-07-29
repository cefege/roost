// Agent transcript entries — the projection of omp's RPC event stream into a
// flat, seq-addressed list the SPA renders directly. Zod twin of the
// AgentEntry messages in proto/roost/v1/wire.proto.
//
// Invariant: `seq` is monotonic per session and starts at 1. An entry may be
// re-emitted with the same `seq` and a fuller body (streaming text, tool
// completion, prompt answered); the client upserts by `seq`, which is what
// makes the live stream idempotent and reconnect-safe.

import { z } from "zod";

// Enforced by the worker before an entry goes on the wire; the client trusts
// them. framePayload bounds one AgentEntriesFrame; ringEntries bounds the
// worker's in-memory transcript.
export const AGENT_ENTRY_CAPS = {
  text: 500_000,
  toolText: 500_000,
  toolDetails: 500_000,
  imageBytes: 1_048_576, // Maximum encoded base64 characters carried on the wire.
  ringEntries: 2_000,
  // Leaves room for protobuf framing around a maximum-size base64 image entry.
  framePayload: 2_097_152,
} as const;

export const AgentToolStatus = z.enum(["running", "ok", "error"]);
export type AgentToolStatus = z.infer<typeof AgentToolStatus>;

export const AgentPromptKind = z.enum(["approval", "question", "input"]);
export type AgentPromptKind = z.infer<typeof AgentPromptKind>;

export const AgentPromptState = z.enum(["pending", "answered", "cancelled"]);
export type AgentPromptState = z.infer<typeof AgentPromptState>;

export const AgentNoticeLevel = z.enum(["info", "warn", "error"]);
export type AgentNoticeLevel = z.infer<typeof AgentNoticeLevel>;

const Base = z.object({
  seq: z.number().int().positive(),
  ts: z.number().int(),
});

const TextBody = { text: z.string(), done: z.boolean() };

export const AgentEntry = z.discriminatedUnion("kind", [
  Base.extend({ kind: z.literal("user"), ...TextBody }),
  Base.extend({ kind: z.literal("assistant"), ...TextBody }),
  Base.extend({ kind: z.literal("thinking"), ...TextBody }),
  Base.extend({
    kind: z.literal("tool"),
    tool_call_id: z.string(),
    name: z.string(),
    args_json: z.string(),
    status: AgentToolStatus,
    text: z.string(),
    details_json: z.string(),
    intent: z.string(),
  }),
  Base.extend({
    kind: z.literal("prompt"),
    // omp extension_ui_request id, verbatim — the reply must echo it.
    prompt_id: z.string(),
    prompt_kind: AgentPromptKind,
    title: z.string(),
    options: z.array(z.string()),
    allow_free_text: z.boolean(),
    state: AgentPromptState,
    answer: z.string(),
  }),
  Base.extend({
    kind: z.literal("notice"),
    level: AgentNoticeLevel,
    text: z.string(),
    details_json: z.string().default(""),
  }),
  Base.extend({
    kind: z.literal("todo"),
    phases_json: z.string(),
  }),
  Base.extend({
    kind: z.literal("subagent"),
    subagent_id: z.string(),
    name: z.string(),
    state: z.enum(["running", "done", "failed", "aborted"]),
    text: z.string(),
  }),
  Base.extend({
    kind: z.literal("image"),
    media_type: z.string(),
    data_b64: z.string(),
    alt: z.string(),
  }),
]);
export type AgentEntry = z.infer<typeof AgentEntry>;

export type AgentTextEntry = Extract<AgentEntry, { kind: "user" | "assistant" | "thinking" }>;
export type AgentToolEntry = Extract<AgentEntry, { kind: "tool" }>;
export type AgentPromptEntry = Extract<AgentEntry, { kind: "prompt" }>;
export type AgentNoticeEntry = Extract<AgentEntry, { kind: "notice" }>;
export type AgentTodoEntry = Extract<AgentEntry, { kind: "todo" }>;
export type AgentSubagentEntry = Extract<AgentEntry, { kind: "subagent" }>;
export type AgentImageEntry = Extract<AgentEntry, { kind: "image" }>;

const TRUNCATION_MARKER = "…[truncated]";

export function clampText(s: string, cap: number): string {
  if (s.length <= cap) return s;
  if (cap <= 0) return "";
  if (cap <= TRUNCATION_MARKER.length) return TRUNCATION_MARKER.slice(0, cap);
  return `${s.slice(0, cap - TRUNCATION_MARKER.length)}${TRUNCATION_MARKER}`;
}

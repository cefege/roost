// Chat wire types — the ONE shared chat contract.
//
// Transport-only (same status as SessionEvent/AgentState): every omp-specific
// module (worker parse/watch, web store/UI) composes from these. Drift caught
// here = drift fixed everywhere. The Zod schema is the in-app gate; the proto
// adapters convert at the wire boundary so renderer + parser stay protobuf-free.
//
// Mirrors the adapter pattern of apps/shared/src/wire/agent-proto.ts.

import { z } from "zod";
import { create } from "@bufbuild/protobuf";
import {
  ChatMessageSchema, ChatFrameSchema,
  ContentBlockSchema,
  ContentBlock_ThinkingTextSchema,
  ContentBlock_ToolCallSchema, ContentBlock_ToolResultSchema,
  ContentBlock_ToolEventSchema, ContentBlock_ImageRefSchema,
  ContentBlock_ApprovalSchema, ContentBlock_Approval_ChoiceSchema,
  type ChatMessage as PbChatMessage,
  type ChatFrame as PbChatFrame,
  type ContentBlock as PbContentBlock,
  type ContentBlock_ThinkingText as PbThinkingText,
  type ContentBlock_ToolCall as PbToolCall,
  type ContentBlock_ToolResult as PbToolResult,
  type ContentBlock_ToolEvent as PbToolEvent,
  type ContentBlock_ImageRef as PbImageRef,
  type ContentBlock_Approval as PbApproval,
} from "../gen/roost/v1/sync_pb.ts";

// Cap applied by the worker parser to thinking + tool_result text. Anything
// beyond this is fetched on demand via SessionsGetChatBlock.
export const TRUNC_CAP = 8192;

// Ceiling for ToolResultBlock.rawJson. 32x TRUNC_CAP: omp's tool views render
// the whole payload client-side and cannot lazy-fetch more.
export const RAW_CAP = 262144;

// ─── ContentBlock variants (discriminated union on `kind`) ────────────────

export const TextBlock = z.object({
  kind: z.literal("text"),
  text: z.string(),
});
export type TextBlock = z.infer<typeof TextBlock>;

export const ThinkingBlock = z.object({
  kind: z.literal("thinking"),
  text: z.string(),
  truncated: z.boolean().default(false),
  fullLen: z.number().int().nonnegative().default(0),
});
export type ThinkingBlock = z.infer<typeof ThinkingBlock>;

export const ToolCallBlock = z.object({
  kind: z.literal("toolCall"),
  callId: z.string(),
  name: z.string(),
  argsJson: z.string(),
});
export type ToolCallBlock = z.infer<typeof ToolCallBlock>;

export const ToolResultBlock = z.object({
  kind: z.literal("toolResult"),
  callId: z.string(),
  name: z.string(),
  text: z.string(),
  isError: z.boolean().default(false),
  truncated: z.boolean().default(false),
  fullLen: z.number().int().nonnegative().default(0),
  // omp ToolResultMessage JSON ({toolCallId,toolName,content,isError,details}) —
  // the payload <omp-tool-view> renders. Defaulted: proto3 decode and any
  // non-omp producer yield "", which the renderer treats as "no rich result".
  rawJson: z.string().default(""),
});
export type ToolResultBlock = z.infer<typeof ToolResultBlock>;

export const ToolEventBlock = z.object({
  kind: z.literal("toolEvent"),
  callId: z.string(),
  name: z.string(),
  phase: z.string(),             // "start" | "update" | "end"
  intent: z.string().default(""),
  output: z.string().default(""),   // live partial output while the tool runs
});
export type ToolEventBlock = z.infer<typeof ToolEventBlock>;

export const ImageBlock = z.object({
  kind: z.literal("image"),
  blobPath: z.string(),
  mime: z.string(),
});
export type ImageBlock = z.infer<typeof ImageBlock>;

// One select option as the pane renders it. omp's RPC select frame carries
// bare labels, so the worker rebuilds this from the ask tool's arguments.
export const ApprovalChoice = z.object({
  value: z.string(),             // exact string to echo back on answer
  label: z.string(),             // " (Recommended)" stripped
  description: z.string().default(""),
  recommended: z.boolean().default(false),
  checked: z.boolean().default(false),
  // string, not enum: an unknown role must degrade to a plain row rather than
  // throw the whole frame away.
  role: z.string().default("option"),
});
export type ApprovalChoice = z.infer<typeof ApprovalChoice>;

// Inline approval prompt (native RPC chat only). omp asks via
// extension_ui_request; the pane answers with extension_ui_response, so the
// block carries both the question and — once answered — the decision.
export const ApprovalBlock = z.object({
  kind: z.literal("approval"),
  requestId: z.string(),
  method: z.string(),            // "confirm" | "select" | "input"
  title: z.string().default(""),
  message: z.string().default(""),
  options: z.array(z.string()).default([]),   // raw frame echo
  resolved: z.boolean().default(false),
  answer: z.string().default(""),
  richOptions: z.array(ApprovalChoice).default([]),  // render model, parallel to options
  header: z.string().default(""),
  progress: z.string().default(""),
  multi: z.boolean().default(false),
});
export type ApprovalBlock = z.infer<typeof ApprovalBlock>;

export const ContentBlock = z.discriminatedUnion("kind", [
  TextBlock, ThinkingBlock, ToolCallBlock, ToolResultBlock, ToolEventBlock, ImageBlock, ApprovalBlock,
]);
export type ContentBlock = z.infer<typeof ContentBlock>;

// ─── ChatMessage / ChatFrame ──────────────────────────────────────────────

export const ChatRole = z.enum(["user", "assistant", "toolResult", "developer"]);
export type ChatRole = z.infer<typeof ChatRole>;

export const ChatMessage = z.object({
  id: z.string(),
  parentId: z.string().default(""),
  ts: z.string(),
  role: ChatRole,
  blocks: z.array(ContentBlock),
});
export type ChatMessage = z.infer<typeof ChatMessage>;

// `append` is APPEND-OR-REPLACE BY id, not append-only: a streaming message is
// re-emitted under the same id as it grows, so a receiver that already holds
// that id MUST replace it in place rather than skip the entry.
export const ChatFrame = z.object({
  sessionId: z.string(),
  append: z.array(ChatMessage),
  seq: z.number().int().nonnegative(),
  reset: z.boolean().default(false),
  streaming: z.boolean().default(false),
  // Status the omp TUI keeps permanently on screen. Empty/zero on the mirror
  // engine, which has no session state to ask for.
  model: z.string().default(""),
  modelName: z.string().default(""),
  thinkingLevel: z.string().default(""),
  contextPct: z.number().int().nonnegative().default(0),
  contextTokens: z.number().int().nonnegative().default(0),
});
export type ChatFrame = z.infer<typeof ChatFrame>;

// ─── to proto ─────────────────────────────────────────────────────────────

export function contentBlockToProto(b: ContentBlock): PbContentBlock {
  switch (b.kind) {
    case "text":
      return create(ContentBlockSchema, { kind: { case: "text", value: b.text } });
    case "thinking":
      return create(ContentBlockSchema, {
        kind: { case: "thinking", value: create(ContentBlock_ThinkingTextSchema, {
          text: b.text, truncated: b.truncated, fullLen: b.fullLen,
        }) },
      });
    case "toolCall":
      return create(ContentBlockSchema, {
        kind: { case: "toolCall", value: create(ContentBlock_ToolCallSchema, {
          callId: b.callId, name: b.name, argsJson: b.argsJson,
        }) },
      });
    case "toolResult":
      return create(ContentBlockSchema, {
        kind: { case: "toolResult", value: create(ContentBlock_ToolResultSchema, {
          callId: b.callId, name: b.name, text: b.text,
          isError: b.isError, truncated: b.truncated, fullLen: b.fullLen, rawJson: b.rawJson,
        }) },
      });
    case "toolEvent":
      return create(ContentBlockSchema, {
        kind: { case: "toolEvent", value: create(ContentBlock_ToolEventSchema, {
          callId: b.callId, name: b.name, phase: b.phase, intent: b.intent, output: b.output,
        }) },
      });
    case "image":
      return create(ContentBlockSchema, {
        kind: { case: "image", value: create(ContentBlock_ImageRefSchema, {
          blobPath: b.blobPath, mime: b.mime,
        }) },
      });
    case "approval":
      return create(ContentBlockSchema, {
        kind: { case: "approval", value: create(ContentBlock_ApprovalSchema, {
          requestId: b.requestId, method: b.method, title: b.title, message: b.message,
          options: b.options, resolved: b.resolved, answer: b.answer,
          richOptions: b.richOptions.map((c) => create(ContentBlock_Approval_ChoiceSchema, c)),
          header: b.header, progress: b.progress, multi: b.multi,
        }) },
      });
  }
}

export function chatMessageToProto(m: ChatMessage): PbChatMessage {
  return create(ChatMessageSchema, {
    id: m.id, parentId: m.parentId, ts: m.ts, role: m.role,
    blocks: m.blocks.map(contentBlockToProto),
  });
}

export function chatFrameToProto(f: ChatFrame): PbChatFrame {
  return create(ChatFrameSchema, {
    sessionId: f.sessionId,
    append: f.append.map(chatMessageToProto),
    seq: BigInt(f.seq),
    reset: f.reset,
    streaming: f.streaming,
    model: f.model, modelName: f.modelName, thinkingLevel: f.thinkingLevel,
    contextPct: f.contextPct, contextTokens: f.contextTokens,
  });
}

// ─── from proto ───────────────────────────────────────────────────────────
// Re-Zod-parses at the boundary so enum membership + shape are enforced before
// the value hits the store/projector (same gate as agentStateFromProto).

export function contentBlockFromProto(p: PbContentBlock): ContentBlock {
  const k = p.kind;
  switch (k.case) {
    case "text":
      return ContentBlock.parse({ kind: "text", text: k.value });
    case "thinking": {
      const v: PbThinkingText = k.value;
      return ContentBlock.parse({
        kind: "thinking", text: v.text, truncated: v.truncated, fullLen: v.fullLen,
      });
    }
    case "toolCall": {
      const v: PbToolCall = k.value;
      return ContentBlock.parse({
        kind: "toolCall", callId: v.callId, name: v.name, argsJson: v.argsJson,
      });
    }
    case "toolResult": {
      const v: PbToolResult = k.value;
      return ContentBlock.parse({
        kind: "toolResult", callId: v.callId, name: v.name, text: v.text,
        isError: v.isError, truncated: v.truncated, fullLen: v.fullLen, rawJson: v.rawJson,
      });
    }
    case "toolEvent": {
      const v: PbToolEvent = k.value;
      return ContentBlock.parse({
        kind: "toolEvent", callId: v.callId, name: v.name, phase: v.phase, intent: v.intent, output: v.output,
      });
    }
    case "image": {
      const v: PbImageRef = k.value;
      return ContentBlock.parse({ kind: "image", blobPath: v.blobPath, mime: v.mime });
    }
    case "approval": {
      const v: PbApproval = k.value;
      return ContentBlock.parse({
        kind: "approval", requestId: v.requestId, method: v.method, title: v.title,
        message: v.message, options: v.options, resolved: v.resolved, answer: v.answer,
        richOptions: v.richOptions, header: v.header, progress: v.progress, multi: v.multi,
      });
    }
    case undefined:
    default:
      // Re-Zod-parse with an unknown marker → throws loudly rather than
      // silently dropping a block. Callers wrap batches in try/catch.
      return ContentBlock.parse({ kind: "text", text: "" });
  }
}

export function chatMessageFromProto(p: PbChatMessage): ChatMessage {
  return ChatMessage.parse({
    id: p.id, parentId: p.parentId, ts: p.ts, role: p.role,
    blocks: p.blocks.map(contentBlockFromProto),
  });
}

export function chatFrameFromProto(p: PbChatFrame): ChatFrame {
  return ChatFrame.parse({
    sessionId: p.sessionId,
    append: p.append.map(chatMessageFromProto),
    seq: Number(p.seq),
    reset: p.reset,
    streaming: p.streaming,
    model: p.model, modelName: p.modelName, thinkingLevel: p.thinkingLevel,
    contextPct: p.contextPct, contextTokens: p.contextTokens,
  });
}

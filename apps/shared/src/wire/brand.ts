// Branded identity types via Zod's native .brand(). The Zod brand IS
// the nominal type — no parallel hand-rolled marker needed.
// Mixing brands = compile-time error. Zero runtime cost.

import { z } from "zod";

// SHA-256 hex of worker's ed25519 pubkey (lowercase, 64 chars).
const WORKER_FP_RE = /^[0-9a-f]{64}$/;
export const WorkerFp = z.string().regex(WORKER_FP_RE).brand<"WorkerFp">();
export type WorkerFp = z.infer<typeof WorkerFp>;
export const asWorkerFp = (s: string): WorkerFp => WorkerFp.parse(s);

// uuid v4 — worker mints on session open.
const UUID_RE = /^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$/i;
export const SessionId = z.string().regex(UUID_RE).brand<"SessionId">();
export type SessionId = z.infer<typeof SessionId>;
export const asSessionId = (s: string): SessionId => SessionId.parse(s);

// Worker-local PTY id. u32; not stable across worker restarts.
export const ChannelId = z.number().int().nonnegative().brand<"ChannelId">();
export type ChannelId = z.infer<typeof ChannelId>;
export const asChannelId = (n: number): ChannelId => ChannelId.parse(n);

export const WorkspaceId = z.string().regex(UUID_RE).brand<"WorkspaceId">();
export type WorkspaceId = z.infer<typeof WorkspaceId>;
export const asWorkspaceId = (s: string): WorkspaceId => WorkspaceId.parse(s);

export const TaskId = z.string().regex(UUID_RE).brand<"TaskId">();
export type TaskId = z.infer<typeof TaskId>;

export const PermissionRuleId = z.string().regex(UUID_RE).brand<"PermissionRuleId">();
export type PermissionRuleId = z.infer<typeof PermissionRuleId>;
export const asPermissionRuleId = (s: string): PermissionRuleId => PermissionRuleId.parse(s);

export const McpRelayId = z.string().regex(UUID_RE).brand<"McpRelayId">();
export type McpRelayId = z.infer<typeof McpRelayId>;

export const WebhookTokenId = z.string().regex(UUID_RE).brand<"WebhookTokenId">();
export type WebhookTokenId = z.infer<typeof WebhookTokenId>;
export const asWebhookTokenId = (s: string): WebhookTokenId => WebhookTokenId.parse(s);

// Trace correlation id. Hex, any length ≥ 8.
export const TraceId = z.string().regex(/^[0-9a-f]{8,}$/i).brand<"TraceId">();
export type TraceId = z.infer<typeof TraceId>;
export const asTraceId = (s: string): TraceId => TraceId.parse(s);

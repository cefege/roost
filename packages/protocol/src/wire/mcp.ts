// MCP relay registry. Coord persists rows; workers subscribe to the
// /mcp.events SSE stream; payloads are opaque to coord (in-memory only,
// no DB persistence).

import { z } from "zod";
import { McpRelayId } from "./brand.ts";

export const McpRelayKind = z.enum(["stdio", "sse"]);
export type McpRelayKind = z.infer<typeof McpRelayKind>;

export const McpRelay = z.object({
  id: McpRelayId,
  label: z.string().min(1),
  kind: McpRelayKind,
  config: z.record(z.string(), z.unknown()),  // free-form JSON the worker interprets
  created_at_ms: z.number().int().positive(),
});
export type McpRelay = z.infer<typeof McpRelay>;

export const McpRelayEvent = z.object({
  relay_id: McpRelayId,
  payload: z.unknown(),
  ts: z.number().int().positive(),
});
export type McpRelayEvent = z.infer<typeof McpRelayEvent>;

// CRUD deltas on the relay registry. Carried alongside McpRelayEvent
// on the same "mcp" stream; discriminate at consumers by presence of `kind`.
export const McpRelayDelta = z.discriminatedUnion("kind", [
  z.object({ kind: z.literal("created"), relay: McpRelay }),
  z.object({ kind: z.literal("updated"), relay: McpRelay }),
  z.object({ kind: z.literal("deleted"), id: McpRelayId }),
]);
export type McpRelayDelta = z.infer<typeof McpRelayDelta>;

// Carrier type for the mcp event bus / SSE stream: either a registry
// delta (CRUD) or a published relay event (worker payload fan-out).
export type McpStreamMessage = McpRelayDelta | McpRelayEvent;

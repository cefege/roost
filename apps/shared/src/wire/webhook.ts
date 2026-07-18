// Webhook tokens — opaque bearer credentials for external triggers
// (GitHub Actions, iOS Shortcuts). Plaintext returned ONCE on mint,
// then SHA-256 hash-only in DB. Scope-gated.

import { z } from "zod";
import { WebhookTokenId } from "./brand.ts";

export const WebhookScope = z.enum(["tasks.enqueue"]);
export type WebhookScope = z.infer<typeof WebhookScope>;

// On-mint response (the ONE time plaintext is ever visible).
export const WebhookTokenMint = z.object({
  id: WebhookTokenId,
  label: z.string().min(1),
  plaintext: z.string().min(1),       // "roost_wh_<hex>"
  scopes: z.array(WebhookScope).min(1),
  created_at_ms: z.number().int().positive(),
});
export type WebhookTokenMint = z.infer<typeof WebhookTokenMint>;

// Redacted list-view row.
export const WebhookToken = z.object({
  id: WebhookTokenId,
  label: z.string().min(1),
  last4: z.string().length(4),
  scopes: z.array(WebhookScope).min(1),
  created_at_ms: z.number().int().positive(),
  last_used_at_ms: z.number().int().positive().nullable(),
});
export type WebhookToken = z.infer<typeof WebhookToken>;

export const WebhookTokenDelta = z.discriminatedUnion("kind", [
  z.object({ kind: z.literal("created"), token: WebhookToken }),
  z.object({ kind: z.literal("deleted"), id: WebhookTokenId }),
]);
export type WebhookTokenDelta = z.infer<typeof WebhookTokenDelta>;

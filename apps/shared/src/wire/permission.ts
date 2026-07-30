// PermissionRule — auto-decision policy shared by coord and clients.

import { z } from "zod";
import { PermissionRuleId } from "./brand.ts";

export const PermissionDecision = z.enum([
  "allow",
  "deny",
  "allow-and-remember",
]);
export type PermissionDecision = z.infer<typeof PermissionDecision>;

export const PermissionRule = z.object({
  id: PermissionRuleId,
  // Glob over `<tool_name>:<input.summary>` style serialized request text.
  tool_pattern: z.string().min(1),
  // Folder where the rule applies; "*" = any folder.
  folder_glob: z.string().min(1),
  decision: PermissionDecision,
  enabled: z.boolean(),
  created_at_ms: z.number().int().positive(),
});
export type PermissionRule = z.infer<typeof PermissionRule>;

export const PermissionRuleDelta = z.discriminatedUnion("kind", [
  z.object({ kind: z.literal("created"), rule: PermissionRule }),
  z.object({ kind: z.literal("updated"), rule: PermissionRule }),
  z.object({ kind: z.literal("deleted"), id: PermissionRuleId }),
]);
export type PermissionRuleDelta = z.infer<typeof PermissionRuleDelta>;

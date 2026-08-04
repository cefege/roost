import { z } from "zod";
import { SessionId } from "./brand.ts";

export const AGENT_ID_MAX_LENGTH = 32;
export const AGENT_STATUS_MESSAGE_MAX_LENGTH = 512;

const AGENT_ID_RE = /^[a-z][a-z0-9-]*$/;

export const AgentId = z.string()
  .min(1)
  .max(AGENT_ID_MAX_LENGTH)
  .regex(AGENT_ID_RE)
  .brand<"AgentId">();
export type AgentId = z.infer<typeof AgentId>;

export const AgentRuntimeState = z.enum(["working", "blocked", "idle"]);
export type AgentRuntimeState = z.infer<typeof AgentRuntimeState>;

const Revision = z.number().int().nonnegative().max(Number.MAX_SAFE_INTEGER);
const TimestampMs = z.number().int().nonnegative().max(Number.MAX_SAFE_INTEGER);

const AgentStatusFields = z.object({
  session_id: SessionId,
  agent_id: AgentId,
  state: AgentRuntimeState,
  message: z.string().max(AGENT_STATUS_MESSAGE_MAX_LENGTH).optional(),
  revision: Revision,
  completed_revision: Revision,
  updated_at: TimestampMs,
});

function completionDoesNotExceedRevision(
  value: { revision: number; completed_revision: number },
  context: z.RefinementCtx,
): void {
  if (value.completed_revision > value.revision) {
    context.addIssue({
      code: "custom",
      path: ["completed_revision"],
      message: "completed_revision must not exceed revision",
    });
  }
}

/** Current active status retained by the worker, coordinator, and browser. */
export const AgentStatus = AgentStatusFields.extend({
  active: z.literal(true),
}).superRefine(completionDoesNotExceedRevision);
export type AgentStatus = z.infer<typeof AgentStatus>;

/** Volatile update; active=false removes the retained status for a session. */
export const AgentStatusUpdate = AgentStatusFields.extend({
  active: z.boolean(),
}).superRefine(completionDoesNotExceedRevision);
export type AgentStatusUpdate = z.infer<typeof AgentStatusUpdate>;

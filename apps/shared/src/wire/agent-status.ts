// Volatile agent-state contracts shared by worker, coordinator, and browser.
// Identity is an all-or-none fencing triple; identityless values remain valid
// only for compatibility with workers deployed before durable observation.

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

export const StatusEpoch = z.string().uuid().brand<"StatusEpoch">();
export type StatusEpoch = z.infer<typeof StatusEpoch>;

export const AgentOccupantId = z.string().uuid().brand<"AgentOccupantId">();
export type AgentOccupantId = z.infer<typeof AgentOccupantId>;

export const AgentStatusSource = z.enum(["integration", "screen"]);
export type AgentStatusSource = z.infer<typeof AgentStatusSource>;

export type AgentStatusIdentity = {
  status_epoch: StatusEpoch;
  occupant_id: AgentOccupantId;
  source: AgentStatusSource;
};

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
  status_epoch: StatusEpoch.optional(),
  occupant_id: AgentOccupantId.optional(),
  source: AgentStatusSource.optional(),
  /** The occupant's last process is gone. Such a row is retained only to carry
   * a completion, so a viewer that has acknowledged that completion has no
   * agent left to show. Absent from workers deployed before exit retention. */
  occupant_exited: z.boolean().default(false),
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

function identityIsComplete(
  value: {
    status_epoch?: StatusEpoch;
    occupant_id?: AgentOccupantId;
    source?: AgentStatusSource;
  },
  context: z.RefinementCtx,
): void {
  const hasStatusEpoch = value.status_epoch !== undefined;
  const hasOccupantId = value.occupant_id !== undefined;
  const hasSource = value.source !== undefined;
  if (hasStatusEpoch === hasOccupantId && hasOccupantId === hasSource) return;
  context.addIssue({
    code: "custom",
    message: "status_epoch, occupant_id, and source must be present together or all absent",
  });
}

/** Current active status retained by the worker, coordinator, and browser. */
export const AgentStatus = AgentStatusFields.extend({
  active: z.literal(true),
}).superRefine(completionDoesNotExceedRevision).superRefine(identityIsComplete);
export type AgentStatus = z.infer<typeof AgentStatus>;

/** Volatile update; active=false removes the retained status for a session. */
export const AgentStatusUpdate = AgentStatusFields.extend({
  active: z.boolean(),
}).superRefine(completionDoesNotExceedRevision).superRefine(identityIsComplete);
export type AgentStatusUpdate = z.infer<typeof AgentStatusUpdate>;

export function isIdentifiedAgentStatus<
  Status extends {
    status_epoch?: StatusEpoch;
    occupant_id?: AgentOccupantId;
    source?: AgentStatusSource;
  },
>(status: Status): status is Status & AgentStatusIdentity {
  return status.status_epoch !== undefined
    && status.occupant_id !== undefined
    && status.source !== undefined;
}

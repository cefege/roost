// Owns the single coordinator policy separating public session events from
// private worker-recovery events. Durable queries, live publication, and frame
// construction all depend on this predicate so browser lanes cannot diverge.

import type { SessionEvent } from "@roost/shared/wire";

export const PRIVATE_SESSION_EVENT_KIND = "agent_reference" as const;
export type PublicSessionEvent = Exclude<
  SessionEvent,
  { kind: typeof PRIVATE_SESSION_EVENT_KIND }
>;

export function isPublicSessionEvent(
  event: SessionEvent,
): event is PublicSessionEvent {
  return event.kind !== PRIVATE_SESSION_EVENT_KIND;
}

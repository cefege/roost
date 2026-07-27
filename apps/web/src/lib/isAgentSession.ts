import type { Session } from "@roost/shared/wire";

export function isAgentSession(session: Session): boolean {
  return session.agent !== null;
}

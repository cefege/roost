// Cross-tab election claims for one exact agent occupant/revision notification.
// Profile-local leases include the identity fence, so a superseded revision
// cannot suppress a fresh notification. Web Locks provide atomic election.

import type { AgentNotificationDelivery } from "./agentNotificationCore.ts";
import { agentStatusOccupantKey } from "./agentStatus.ts";

const CLAIM_PREFIX = "roost.agentNotificationClaim.";
const CLAIM_MAX_AGE_MS = 7 * 24 * 60 * 60 * 1_000;

function claimStorageKey(delivery: AgentNotificationDelivery): string {
  const occupant = agentStatusOccupantKey(delivery.token) ?? "legacy";
  return `${CLAIM_PREFIX}v2.${delivery.sessionId}.${occupant}.${delivery.token.revision}.${delivery.kind}`;
}

function existingClaim(key: string, now: number): boolean {
  try {
    const raw = localStorage.getItem(key);
    if (!raw) return false;
    const timestamp = Number(raw.split(":", 1)[0]);
    if (Number.isFinite(timestamp) && now - timestamp <= CLAIM_MAX_AGE_MS) return true;
    localStorage.removeItem(key);
  } catch { /* unavailable profile storage */ }
  return false;
}

async function storageElection(key: string): Promise<boolean> {
  const now = Date.now();
  if (existingClaim(key, now)) return false;
  const token = `${now}:${crypto.randomUUID()}`;
  try {
    localStorage.setItem(key, token);
    await new Promise<void>((resolve) => setTimeout(resolve, 25));
    return localStorage.getItem(key) === token;
  } catch {
    return true;
  }
}

/** Claim one browser-profile delivery. Web Locks makes the storage check atomic;
 *  the delayed last-writer election is the fallback on browsers without locks. */
export async function claimAgentNotification(
  delivery: AgentNotificationDelivery,
): Promise<boolean> {
  const key = claimStorageKey(delivery);
  const locks = typeof navigator !== "undefined" ? navigator.locks : undefined;
  if (locks) {
    return locks.request(
      `roost-agent-notification:${key}`,
      { mode: "exclusive", ifAvailable: true },
      async (lock) => {
        if (!lock) return false;
        const now = Date.now();
        if (existingClaim(key, now)) return false;
        try { localStorage.setItem(key, `${now}:${crypto.randomUUID()}`); }
        catch { /* the lock still elects one concurrent tab */ }
        return true;
      },
    );
  }
  return storageElection(key);
}

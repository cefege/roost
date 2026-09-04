// Cross-tab election claims for one agent-state browser notification.
// Notification delivery writes profile-local leases; account boundaries erase them.
// Web Locks provide atomic election while storage remains the durable fallback.

import type { AgentNotificationKind } from "./agentNotificationCore.ts";

const CLAIM_PREFIX = "roost.agentNotificationClaim.";
const CLAIM_MAX_AGE_MS = 7 * 24 * 60 * 60 * 1_000;

function claimStorageKey(sessionId: string, revision: number, kind: AgentNotificationKind): string {
  return `${CLAIM_PREFIX}${sessionId}.${revision}.${kind}`;
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
  sessionId: string,
  revision: number,
  kind: AgentNotificationKind,
): Promise<boolean> {
  const key = claimStorageKey(sessionId, revision, kind);
  const locks = typeof navigator !== "undefined" ? navigator.locks : undefined;
  if (locks) {
    return locks.request(
      `roost-agent-notification:${sessionId}:${revision}:${kind}`,
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

export function clearAgentNotificationClaimsForAccountBoundary(): void {
  try {
    const claimKeys: string[] = [];
    for (let idx = 0; idx < localStorage.length; idx += 1) {
      const key = localStorage.key(idx);
      if (key?.startsWith(CLAIM_PREFIX)) claimKeys.push(key);
    }
    for (const key of claimKeys) localStorage.removeItem(key);
  } catch {
    // Profile storage is optional.
  }
}

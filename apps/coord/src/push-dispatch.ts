// Post-commit Web Push dispatch for agent status transitions. appendEvent calls
// firePushForTransition (fire-and-forget) after its transaction commits when an
// agent goes running → needs-input (blocked) or running → idle/done (finished).
//
// Push is per-device and unconditional EXCEPT the suppression rule: a device
// (viewer_fp) with a live viewport claim on the transitioning session is already
// looking at it, so it gets no OS push. All other subscribed devices do.

import type { KyselyDB } from "./db/connection.ts";
import { _viewersBySession } from "./connect/viewer-tracker.ts";
import { sendPushToSubscriptions } from "./push-sender.ts";
import { log } from "@roost/shared/log";

export type PushTransition = "blocked" | "done";

/** Classify an agent status change into a push kind, or null for no push.
 *  Only running → needs-input (blocked) and running → idle/done (finished)
 *  fire; needs-input → idle (you answered) and any → running stay silent. */
export function classifyPushTransition(prev: string, next: string): PushTransition | null {
  if (prev === "running" && next === "needs-input") return "blocked";
  if (prev === "running" && (next === "idle" || next === "done")) return "done";
  return null;
}

export async function firePushForTransition(
  db: KyselyDB,
  sessionId: string,
  kind: PushTransition,
): Promise<void> {
  try {
    const subs = await db.selectFrom("push_subscriptions").selectAll().execute();
    if (subs.length === 0) return;

    const row = await db
      .selectFrom("sessions")
      .select(["cwd", "custom_title"])
      .where("id", "=", sessionId)
      .executeTakeFirst();
    if (!row) {
      log.warn("push", "session_missing", { session_id: sessionId, kind });
      return;
    }

    // Suppress push to any device actively viewing THIS session (live claim).
    const viewers = _viewersBySession.get(sessionId);
    const targets = subs.filter((s) => {
      const claim = viewers?.get(s.viewer_fp);
      return !(claim && claim.cols > 0 && claim.rows > 0);
    });
    const suppressed = subs.length - targets.length;
    if (targets.length === 0) {
      log.info("push", "suppressed_all", {
        session_id: sessionId,
        kind,
        subs: subs.length,
        suppressed,
      });
      return;
    }

    const leaf = row.cwd.split("/").filter(Boolean).pop() ?? row.cwd;
    const title = row.custom_title || leaf;
    const body = kind === "blocked" ? "Needs your input" : "Finished";
    const result = await sendPushToSubscriptions(db, targets, { sessionId, kind, title, body });
    log.info("push", "dispatched", {
      session_id: sessionId,
      kind,
      subs: subs.length,
      suppressed,
      targeted: targets.length,
      delivered: result.delivered,
      expired: result.expired,
      failed: result.failed,
    });
  } catch (err) {
    log.warn("push", "dispatch_failed", { session_id: sessionId, error: String(err) });
  }
}

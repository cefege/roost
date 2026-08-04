// Per-device Web Push dispatch for delayed coding-agent transitions. A device
// actively viewing the session is suppressed; other subscribed devices receive
// one coalescing notification payload.

import { log } from "@roost/shared/log";
import type { KyselyDB } from "./db/connection.ts";
import { _viewersBySession } from "./connect/viewer-tracker.ts";
import { sendPushToSubscriptions } from "./push-sender.ts";

export type PushTransition = "blocked" | "done";
type PushSender = typeof sendPushToSubscriptions;
export async function firePushForTransition(
  db: KyselyDB,
  sessionId: string,
  kind: PushTransition,
  send: PushSender = sendPushToSubscriptions,
): Promise<void> {
  try {
    const subscriptions = await db.selectFrom("push_subscriptions").selectAll().execute();
    if (subscriptions.length === 0) return;

    const session = await db
      .selectFrom("sessions")
      .select(["cwd", "custom_title"])
      .where("id", "=", sessionId)
      .where("status", "=", "open")
      .executeTakeFirst();
    if (!session) {
      log.info("push", "session_missing", { session_id: sessionId, kind });
      return;
    }

    const viewers = _viewersBySession.get(sessionId);
    const targets = subscriptions.filter((subscription) => {
      const claim = viewers?.get(subscription.viewer_fp);
      return !(claim && claim.cols > 0 && claim.rows > 0);
    });
    const suppressed = subscriptions.length - targets.length;
    if (targets.length === 0) {
      log.info("push", "suppressed_all", {
        session_id: sessionId,
        kind,
        subscriptions: subscriptions.length,
      });
      return;
    }

    const leaf = session.cwd.split(/[\\/]/).filter(Boolean).pop() ?? session.cwd;
    const title = session.custom_title || leaf || "Roost";
    const body = kind === "blocked" ? "Needs your input" : "Finished";
    const result = await send(db, targets, {
      sessionId,
      kind,
      title,
      body,
    });
    log.info("push", "dispatched", {
      session_id: sessionId,
      kind,
      subscriptions: subscriptions.length,
      suppressed,
      targeted: targets.length,
      delivered: result.delivered,
      expired: result.expired,
      failed: result.failed,
    });
  } catch (error) {
    log.warn("push", "dispatch_failed", {
      session_id: sessionId,
      kind,
      error: String(error),
    });
  }
}

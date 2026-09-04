// Per-device Web Push dispatch for delayed coding-agent transitions. A device
// actively viewing the session is suppressed; other subscribed devices receive
// one coalescing notification payload.

import { log } from "@roost/shared/log";
import { sql } from "kysely";
import type { KyselyDB } from "./db/connection.ts";
import { activeTerminalViewerFingerprints } from "./connect/terminal-view-hub.ts";
import { sendPushToSubscriptions } from "./push-sender.ts";

export type PushTransition = "blocked" | "done";
type PushSender = typeof sendPushToSubscriptions;
export async function firePushForTransition(
  db: KyselyDB,
  sessionId: string,
  kind: PushTransition,
  allowedOrigins: readonly string[],
  send: PushSender = sendPushToSubscriptions,
  tenantRouteKey?: string,
): Promise<void> {
  try {
    if (allowedOrigins.length === 0) return;
    const allowedOriginSet = new Set(allowedOrigins);
    const session = await db
      .selectFrom("sessions")
      .select(["cwd", "custom_title", "dashboard_id"])
      .where("id", "=", sessionId)
      .where("status", "=", "open")
      .executeTakeFirst();
    if (!session) {
      log.info("push", "session_missing", { session_id: sessionId, kind });
      return;
    }

    if (session.dashboard_id === null) return;

    // account_devices is the live browser-device registry. The key FK handles
    // normal revocation, while this delete also repairs rows left by legacy
    // cleanup paths that removed only the account-device association.
    await sql`
      DELETE FROM push_subscriptions
      WHERE dashboard_id = ${session.dashboard_id}
        AND NOT EXISTS (
          SELECT 1
          FROM account_devices
          WHERE account_devices.fingerprint = push_subscriptions.viewer_fp
        )
    `.execute(db);

    const subscriptions = await db
      .selectFrom("push_subscriptions as subscription")
      .innerJoin(
        "account_devices as device",
        "device.fingerprint",
        "subscription.viewer_fp",
      )
      .innerJoin("accounts as account", "account.id", "device.account_id")
      .innerJoin("dashboard_memberships as membership", (join) =>
        join
          .onRef("membership.account_id", "=", "device.account_id")
          .onRef("membership.dashboard_id", "=", "subscription.dashboard_id"))
      .innerJoin("dashboards as dashboard", "dashboard.id", "subscription.dashboard_id")
      .select([
        "subscription.dashboard_id",
        "subscription.viewer_fp",
        "subscription.endpoint",
        "subscription.p256dh",
        "subscription.auth",
        "subscription.created_at_ms",
      ])
      .where("subscription.dashboard_id", "=", session.dashboard_id)
      .where("account.status", "=", "active")
      .where("dashboard.status", "=", "active")
      .execute();
    if (subscriptions.length === 0) return;

    const viewers = activeTerminalViewerFingerprints(sessionId);
    const targets = subscriptions.filter((subscription) => {
      if (viewers.has(subscription.viewer_fp)) return false;
      try {
        const endpoint = new URL(subscription.endpoint);
        return endpoint.protocol === "https:"
          && endpoint.username === ""
          && endpoint.password === ""
          && endpoint.hash === ""
          && allowedOriginSet.has(endpoint.origin);
      } catch {
        return false;
      }
    });
    const suppressed = subscriptions.filter(
      (subscription) => viewers.has(subscription.viewer_fp),
    ).length;
    const disallowed = subscriptions.length - targets.length - suppressed;
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
      ...(tenantRouteKey ? { routeKey: tenantRouteKey } : {}),
    });
    log.info("push", "dispatched", {
      session_id: sessionId,
      kind,
      subscriptions: subscriptions.length,
      suppressed,
      targeted: targets.length,
      disallowed,
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

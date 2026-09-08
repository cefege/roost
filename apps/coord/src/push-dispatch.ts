// Per-device Web Push preparation for delayed coding-agent transitions.
// Async database filtering revalidates the exact triggering identity before
// transport, and each payload carries its identity-derived deduplication token.

import { createHash } from "node:crypto";
import { log } from "@roost/shared/log";
import { sql } from "kysely";
import type { KyselyDB } from "./db/connection.ts";
import { activeTerminalViewerFingerprints } from "./connect/terminal-view-hub.ts";
import { sendPushToSubscriptions } from "./push-sender.ts";
import { hasUrlUserInfo } from "./url-user-info.ts";
import type { AgentOccupantId, StatusEpoch } from "@roost/shared/wire";

export type PushTransition = "blocked" | "done";
export interface AgentPushTransition {
  readonly sessionId: string;
  readonly kind: PushTransition;
  readonly statusEpoch: StatusEpoch;
  readonly occupantId: AgentOccupantId;
  /** Revision that produced the transition, retained across same-state updates. */
  readonly revision: number;
}
type PushSender = typeof sendPushToSubscriptions;

function pushDeduplicationToken(transition: AgentPushTransition): string {
  return createHash("sha256")
    .update(JSON.stringify([
      transition.sessionId,
      transition.kind,
      transition.statusEpoch,
      transition.occupantId,
      transition.revision,
    ]))
    .digest("base64url")
    .slice(0, 32);
}

export async function firePushForTransition(
  db: KyselyDB,
  transition: AgentPushTransition,
  allowedOrigins: readonly string[],
  isCurrent: () => boolean,
  send: PushSender = sendPushToSubscriptions,
): Promise<void> {
  const { sessionId, kind } = transition;
  try {
    if (allowedOrigins.length === 0 || !isCurrent()) return;
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
          && !hasUrlUserInfo(endpoint)
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
    const deduplicationToken = pushDeduplicationToken(transition);
    const payload = {
      sessionId,
      kind,
      title,
      body,
      statusEpoch: transition.statusEpoch,
      occupantId: transition.occupantId,
      revision: transition.revision,
      deduplicationToken,
    };
    if (!isCurrent()) {
      log.info("push", "status_superseded", {
        session_id: sessionId,
        kind,
        status_epoch: transition.statusEpoch,
        occupant_id: transition.occupantId,
        revision: transition.revision,
      });
      return;
    }
    const result = await send(db, targets, payload, {
      deduplicationToken,
      isCurrent,
    });
    log.info("push", "dispatched", {
      session_id: sessionId,
      kind,
      status_epoch: transition.statusEpoch,
      occupant_id: transition.occupantId,
      revision: transition.revision,
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
      status_epoch: transition.statusEpoch,
      occupant_id: transition.occupantId,
      revision: transition.revision,
      error: String(error),
    });
  }
}

// Web Push get-config/subscribe/unsubscribe RPC handlers over the
// push_subscriptions table. Provider origins are operator-owned config:
// an empty allowlist disables Push, and attacker-controlled endpoints are
// admitted only when their exact HTTPS origin is present. Subscription writes
// enforce the per-account-device cap in the same SQLite statement as the upsert.
import { Code, ConnectError, type ServiceImpl } from "@connectrpc/connect";
import { sql } from "kysely";
import { create } from "@bufbuild/protobuf";
import {
  CoordinatorService,
  PushGetConfigResponseSchema,
  PushSubscribeResponseSchema,
  PushUnsubscribeResponseSchema,
} from "@roost/shared/proto/coordinator_pb";
import { requireDashboardActor } from "./auth-interceptor.ts";
import { getVapidKeys } from "../vapid.ts";
import type { ConnectDeps } from "./router.ts";

const ENDPOINT_MAX_LENGTH = 4_096;
const KEY_MAX_LENGTH = 512;
const BASE64URL_RE = /^[A-Za-z0-9_-]+$/;
const MAX_SUBSCRIPTIONS_PER_DEVICE = 4;

type PushMethods = "pushGetConfig" | "pushSubscribe" | "pushUnsubscribe";

function requirePushEnabled(allowedOrigins: readonly string[]): void {
  if (allowedOrigins.length === 0) {
    throw new ConnectError("Push is unavailable", Code.FailedPrecondition);
  }
}

function validateEndpoint(endpoint: string, allowedOrigins: readonly string[]): void {
  if (endpoint.length === 0 || endpoint.length > ENDPOINT_MAX_LENGTH) {
    throw new ConnectError("invalid push endpoint", Code.InvalidArgument);
  }
  let url: URL;
  try {
    url = new URL(endpoint);
  } catch {
    throw new ConnectError("invalid push endpoint", Code.InvalidArgument);
  }
  if (
    url.protocol !== "https:"
    || url.username !== ""
    || url.password !== ""
    || url.hash !== ""
    || !allowedOrigins.includes(url.origin)
  ) {
    throw new ConnectError("invalid push endpoint", Code.InvalidArgument);
  }
}

function validateKey(name: string, value: string): void {
  if (
    value.length === 0
    || value.length > KEY_MAX_LENGTH
    || !BASE64URL_RE.test(value)
  ) throw new ConnectError(`invalid push ${name}`, Code.InvalidArgument);
}

export function makePushHandlers(
  deps: ConnectDeps,
): Pick<ServiceImpl<typeof CoordinatorService>, PushMethods> {
  return {
    async pushGetConfig(_request, context) {
      requireDashboardActor(context.values);
      if (deps.cfg.pushAllowedOrigins.length === 0) {
        return create(PushGetConfigResponseSchema, {
          vapidPublicKeyB64: "",
          available: false,
        });
      }
      const keys = await getVapidKeys(deps.db);
      return create(PushGetConfigResponseSchema, {
        vapidPublicKeyB64: keys.publicKey,
        available: true,
      });
    },

    async pushSubscribe(request, context) {
      const actor = requireDashboardActor(context.values);
      const allowedOrigins = deps.cfg.pushAllowedOrigins;
      requirePushEnabled(allowedOrigins);
      validateEndpoint(request.endpoint, allowedOrigins);
      validateKey("p256dh", request.p256dh);
      validateKey("auth", request.auth);
      const now = Date.now();
      const upserted = await sql`
        INSERT INTO push_subscriptions (
          dashboard_id, viewer_fp, endpoint, p256dh, auth, created_at_ms
        )
        SELECT
          ${actor.dashboardId}, ${actor.deviceFingerprint}, ${request.endpoint},
          ${request.p256dh}, ${request.auth}, ${now}
        WHERE EXISTS (
          SELECT 1
          FROM push_subscriptions
          WHERE dashboard_id IS ${actor.dashboardId}
            AND viewer_fp = ${actor.deviceFingerprint}
            AND endpoint = ${request.endpoint}
        ) OR (
          SELECT COUNT(*)
          FROM push_subscriptions
          WHERE viewer_fp = ${actor.deviceFingerprint}
        ) < ${MAX_SUBSCRIPTIONS_PER_DEVICE}
        ON CONFLICT (dashboard_id, viewer_fp, endpoint) DO UPDATE SET
          p256dh = excluded.p256dh,
          auth = excluded.auth,
          created_at_ms = excluded.created_at_ms
      `.execute(deps.db);
      if (upserted.numAffectedRows !== 1n) {
        throw new ConnectError("Push subscription limit reached", Code.ResourceExhausted);
      }
      return create(PushSubscribeResponseSchema, { ok: true });
    },

    async pushUnsubscribe(request, context) {
      const actor = requireDashboardActor(context.values);
      const allowedOrigins = deps.cfg.pushAllowedOrigins;
      requirePushEnabled(allowedOrigins);
      validateEndpoint(request.endpoint, allowedOrigins);
      await deps.db
        .deleteFrom("push_subscriptions")
        .where("dashboard_id", "=", actor.dashboardId)
        .where("viewer_fp", "=", actor.deviceFingerprint)
        .where("endpoint", "=", request.endpoint)
        .execute();
      return create(PushUnsubscribeResponseSchema, { ok: true });
    },
  };
}

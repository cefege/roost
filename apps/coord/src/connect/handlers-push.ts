// Web Push subscription RPC handlers — app_settings-backed VAPID config +
// push_subscriptions rows keyed by the caller's browser fingerprint. Registered
// via its own factory spread in router.ts::buildConnectRouter. Closes over
// ConnectDeps only (deps.db); no shared router-local state.
//
// Push is per-device: (viewer_fp, endpoint) is the subscription identity. The fp
// is the same EdDSA kid hex used for JWT auth, so a device's subscriptions and
// its viewer-presence claims (viewer-tracker) share one identity — which is what
// lets the push dispatcher suppress notifications for a device actively viewing
// the transitioning session.

import type { ServiceImpl } from "@connectrpc/connect";
import { create } from "@bufbuild/protobuf";
import {
  CoordinatorService,
  PushGetConfigResponseSchema,
  PushSubscribeResponseSchema,
  PushUnsubscribeResponseSchema,
} from "@roost/shared/proto/coordinator_pb";
import { requireAuth } from "./auth-interceptor.ts";
import { getVapidKeys } from "../vapid.ts";
import type { ConnectDeps } from "./router.ts";

type PushMethods = "pushGetConfig" | "pushSubscribe" | "pushUnsubscribe";

export function makePushHandlers(
  deps: ConnectDeps,
): Pick<ServiceImpl<typeof CoordinatorService>, PushMethods> {
  return {
    async pushGetConfig(_req, ctx) {
      requireAuth(ctx.values);
      const keys = await getVapidKeys(deps.db);
      return create(PushGetConfigResponseSchema, {
        vapidPublicKeyB64: keys.publicKey,
        available: true,
      });
    },

    async pushSubscribe(req, ctx) {
      const caller = requireAuth(ctx.values);
      const now = Date.now();
      await deps.db
        .insertInto("push_subscriptions")
        .values({
          viewer_fp: caller.fingerprint,
          endpoint: req.endpoint,
          p256dh: req.p256dh,
          auth: req.auth,
          created_at_ms: now,
        })
        .onConflict((oc) =>
          oc.columns(["viewer_fp", "endpoint"]).doUpdateSet({
            p256dh: req.p256dh,
            auth: req.auth,
            created_at_ms: now,
          }),
        )
        .execute();
      return create(PushSubscribeResponseSchema, { ok: true });
    },

    async pushUnsubscribe(req, ctx) {
      const caller = requireAuth(ctx.values);
      await deps.db
        .deleteFrom("push_subscriptions")
        .where("viewer_fp", "=", caller.fingerprint)
        .where("endpoint", "=", req.endpoint)
        .execute();
      return create(PushUnsubscribeResponseSchema, { ok: true });
    },
  };
}

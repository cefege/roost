// Web Push get-config/subscribe/unsubscribe RPC handlers over the
// push_subscriptions table, upserting on (viewer_fp, endpoint).
// Endpoint and key inputs are attacker-controlled: endpoints must parse as
// HTTPS URLs within 4 KiB and keys must be bounded base64url — all validated
// BEFORE any DB write, so a hostile subscription can never reach storage.
import { Code, ConnectError, type ServiceImpl } from "@connectrpc/connect";
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

const ENDPOINT_MAX_LENGTH = 4_096;
const KEY_MAX_LENGTH = 512;
const BASE64URL_RE = /^[A-Za-z0-9_-]+$/;

type PushMethods = "pushGetConfig" | "pushSubscribe" | "pushUnsubscribe";

function validateEndpoint(endpoint: string): void {
  if (endpoint.length === 0 || endpoint.length > ENDPOINT_MAX_LENGTH) {
    throw new ConnectError("invalid push endpoint", Code.InvalidArgument);
  }
  try {
    if (new URL(endpoint).protocol !== "https:") throw new Error("not HTTPS");
  } catch {
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
      requireAuth(context.values);
      const keys = await getVapidKeys(deps.db);
      return create(PushGetConfigResponseSchema, {
        vapidPublicKeyB64: keys.publicKey,
        available: true,
      });
    },

    async pushSubscribe(request, context) {
      const caller = requireAuth(context.values);
      validateEndpoint(request.endpoint);
      validateKey("p256dh", request.p256dh);
      validateKey("auth", request.auth);
      const now = Date.now();
      await deps.db
        .insertInto("push_subscriptions")
        .values({
          viewer_fp: caller.fingerprint,
          endpoint: request.endpoint,
          p256dh: request.p256dh,
          auth: request.auth,
          created_at_ms: now,
        })
        .onConflict((conflict) =>
          conflict.columns(["viewer_fp", "endpoint"]).doUpdateSet({
            p256dh: request.p256dh,
            auth: request.auth,
            created_at_ms: now,
          }),
        )
        .execute();
      return create(PushSubscribeResponseSchema, { ok: true });
    },

    async pushUnsubscribe(request, context) {
      const caller = requireAuth(context.values);
      validateEndpoint(request.endpoint);
      await deps.db
        .deleteFrom("push_subscriptions")
        .where("viewer_fp", "=", caller.fingerprint)
        .where("endpoint", "=", request.endpoint)
        .execute();
      return create(PushUnsubscribeResponseSchema, { ok: true });
    },
  };
}

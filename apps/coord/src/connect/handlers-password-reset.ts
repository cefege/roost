// Password reset issues opaque one-time credentials through the encrypted email
// outbox and redeems them without revealing account state. A successful reset
// revokes every browser key only after the new password commits atomically.

import { create } from "@bufbuild/protobuf";
import { Code, ConnectError, type ServiceImpl } from "@connectrpc/connect";
import type { EmailOutboxPayload } from "@roost/shared/email-payload";
import { log } from "@roost/shared/log";
import { normalizeAccountEmail } from "@roost/shared/native-credentials";
import {
  isNativePasswordLengthValid,
  NATIVE_PASSWORD_MIN_BOOTSTRAP_LENGTH,
} from "@roost/shared/native-credentials";
import {
  AuthPasswordResetRedeemResponseSchema,
  AuthPasswordResetStartResponseSchema,
  CoordinatorService,
} from "@roost/shared/proto/coordinator_pb";
import { isTenantRouteKey } from "@roost/shared/tenant-route";
import { randomBytes, randomUUID } from "node:crypto";
import type { Transaction } from "kysely";
import type { DB } from "../db/schema.ts";
import { invalidateJwtKey } from "../jwt.ts";
import { checkCustomLimit } from "../middleware/rate-limit.ts";
import {
  denyRedemption,
  hashToken,
  invalidPassword,
  ONE_TIME_TOKEN,
} from "./account-one-time-credentials.ts";
import type { ConnectDeps } from "./router.ts";

const PASSWORD_RESET_TTL_MS = 60 * 60 * 1_000;
const PASSWORD_RESET_REQUEST_WINDOW_MS = 15 * 60 * 1_000;

function emailUnavailable(): never {
  throw new ConnectError("email delivery is unavailable", Code.FailedPrecondition);
}


function escapeHtml(value: string): string {
  return value.replace(/[&<>"']/g, (char) => {
    switch (char) {
      case "&": return "&amp;";
      case "<": return "&lt;";
      case ">": return "&gt;";
      case '"': return "&quot;";
      case "'": return "&#39;";
      default: return char;
    }
  });
}

function passwordResetLink(
  webPublicUrl: string,
  tenantRouteKey: string | undefined,
  token: string,
): string {
  let url: URL;
  try {
    const path = tenantRouteKey === undefined
      ? "/reset-password"
      : `/reset-password/${tenantRouteKey}`;
    url = new URL(path, webPublicUrl);
  } catch {
    emailUnavailable();
  }
  url.hash = token;
  return url.toString();
}

function passwordResetPayload(
  webPublicUrl: string,
  tenantRouteKey: string | undefined,
  token: string,
): EmailOutboxPayload {
  const link = passwordResetLink(webPublicUrl, tenantRouteKey, token);
  return {
    subject: "Reset your Roost password",
    html: `<p>Use this link to reset your Roost password.</p><p><a href="${escapeHtml(link)}">Reset password</a></p>`,
    text: `Use this link to reset your Roost password: ${link}`,
  };
}

function configuredEmail(deps: ConnectDeps): NonNullable<ConnectDeps["email"]> {
  if (
    !deps.email
    || !deps.cfg.webPublicUrl
    || (deps.cfg.managedContainer && !isTenantRouteKey(deps.cfg.tenantRouteKey))
  ) {
    emailUnavailable();
  }
  return deps.email;
}

function encryptedResetOutbox(
  email: NonNullable<ConnectDeps["email"]>,
  recipient: string,
  payload: EmailOutboxPayload,
  now: number,
) {
  const id = randomUUID();
  const kind = "password_reset";
  return {
    id,
    kind,
    recipient,
    encrypted_payload: email.encryptPayload({ outboxId: id, kind }, payload),
    idempotency_key: id,
    state: "pending",
    attempts: 0,
    locked_until_ms: null,
    lease_token: null,
    next_attempt_ms: now,
    provider_message_id: null,
    sent_at_ms: null,
    failed_at_ms: null,
    last_error: null,
  };
}

async function verifiedAccountForEmail(
  transaction: Transaction<DB>,
  email: string,
): Promise<{ id: string } | undefined> {
  return transaction.selectFrom("accounts as account")
    .innerJoin("account_identities as identity", "identity.account_id", "account.id")
    .select("account.id as id")
    .where("account.email_normalized", "=", email)
    .where("account.status", "=", "active")
    .where("account.password_hash", "is not", null)
    .where("identity.issuer", "=", "native")
    .whereRef("identity.subject", "=", "account.id")
    .whereRef("identity.email_normalized", "=", "account.email_normalized")
    .where("identity.revoked_at_ms", "is", null)
    .executeTakeFirst();
}

type PasswordResetMethods =
  | "authPasswordResetRequest"
  | "authPasswordResetRedeem";

export function makePasswordResetHandlers(
  deps: ConnectDeps,
): Pick<ServiceImpl<typeof CoordinatorService>, PasswordResetMethods> {
  return {
    async authPasswordResetRequest(req, _ctx) {
      const email = configuredEmail(deps);
      const emailNormalized = normalizeAccountEmail(req.email);
      if (!emailNormalized) return create(AuthPasswordResetStartResponseSchema, {});
      const emailDigest = hashToken(emailNormalized);
      if (checkCustomLimit(
        emailDigest,
        "password-reset-email",
        5,
        PASSWORD_RESET_REQUEST_WINDOW_MS,
      )) {
        return create(AuthPasswordResetStartResponseSchema, {});
      }

      const now = Date.now();
      const issued = await deps.db.transaction().execute(async (trx) => {
        const account = await verifiedAccountForEmail(trx, emailNormalized);
        // Account existence deliberately does not change the acknowledgement.
        if (!account) return null;
        const token = randomBytes(32).toString("base64url");

        const outbox = encryptedResetOutbox(
          email,
          emailNormalized,
          passwordResetPayload(
            deps.cfg.webPublicUrl!,
            deps.cfg.managedContainer ? deps.cfg.tenantRouteKey : undefined,
            token,
          ),
          now,
        );
        await trx.updateTable("password_reset_tokens")
          .set({ used_at_ms: now })
          .where("account_id", "=", account.id)
          .where("used_at_ms", "is", null)
          .execute();
        await trx.insertInto("password_reset_tokens").values({
          account_id: account.id,
          email_normalized: emailNormalized,
          token_hash: hashToken(token),
          expires_at_ms: now + PASSWORD_RESET_TTL_MS,
          used_at_ms: null,
        }).execute();
        await trx.insertInto("email_outbox").values(outbox).execute();
        return { accountId: account.id, outboxId: outbox.id };
      });
      if (issued) {
        log.info("auth.password_reset", "issued", {
          account_id: issued.accountId,
          outbox_id: issued.outboxId,
        });
      }
      return create(AuthPasswordResetStartResponseSchema, {});
    },

    async authPasswordResetRedeem(req, _ctx) {
      if (!ONE_TIME_TOKEN.test(req.token)) denyRedemption();
      if (!isNativePasswordLengthValid(req.newPassword, NATIVE_PASSWORD_MIN_BOOTSTRAP_LENGTH)) {
        invalidPassword();
      }

      // Native password material never reaches logs or SQLite un-hashed.
      const passwordHash = await deps.passwordWorkGate.hash(req.newPassword);
      const now = Date.now();
      const tokenHash = hashToken(req.token);
      const revoked = await deps.db.transaction().execute(async (trx) => {
        const reset = await trx.updateTable("password_reset_tokens")
          .set({ used_at_ms: now })
          .where("token_hash", "=", tokenHash)
          .where("used_at_ms", "is", null)
          .where("expires_at_ms", ">", now)
          .returning(["account_id", "email_normalized"])
          .executeTakeFirst();
        if (!reset) denyRedemption();

        const account = await trx.selectFrom("accounts as account")
          .innerJoin("account_identities as identity", "identity.account_id", "account.id")
          .select(["account.id as id", "account.email_normalized as email_normalized"])
          .where("account.id", "=", reset.account_id)
          .where("account.email_normalized", "=", reset.email_normalized)
          .where("account.status", "=", "active")
          .where("account.password_hash", "is not", null)
          .where("identity.issuer", "=", "native")
          .whereRef("identity.subject", "=", "account.id")
          .whereRef("identity.email_normalized", "=", "account.email_normalized")
          .where("identity.revoked_at_ms", "is", null)
          .executeTakeFirst();
        if (!account) denyRedemption();

        const [devices, memberships] = await Promise.all([
          trx.selectFrom("account_devices")
            .select("fingerprint")
            .where("account_id", "=", account.id)
            .execute(),
          trx.selectFrom("dashboard_memberships")
            .select("dashboard_id")
            .where("account_id", "=", account.id)
            .execute(),
        ]);
        const fingerprints = devices.map((device) => device.fingerprint);
        await trx.updateTable("accounts")
          .set({ password_hash: passwordHash, password_changed_at_ms: now })
          .where("id", "=", account.id)
          .execute();

        if (fingerprints.length > 0) {
          await trx.insertInto("authorized_key_revocations").values(fingerprints.map((fingerprint) => ({
            fingerprint,
            revoked_at_ms: now,
            revoked_by_fp: "password-reset",
            reason: "password-reset",
          }))).onConflict((conflict) => conflict.column("fingerprint").doNothing()).execute();
          await trx.deleteFrom("bootstrap_tokens")
            .where("used_at_ms", "is", null)
            .where("minted_by_fp", "in", fingerprints)
            .execute();
          await trx.deleteFrom("push_subscriptions")
            .where("viewer_fp", "in", fingerprints)
            .execute();
          await trx.deleteFrom("account_devices")
            .where("account_id", "=", account.id)
            .execute();
          await trx.deleteFrom("authorized_keys")
            .where("fingerprint", "in", fingerprints)
            .execute();
        }
        return {
          accountId: account.id,
          fingerprints,
          dashboardIds: memberships.map((membership) => membership.dashboard_id),
        };
      });

      for (const fingerprint of revoked.fingerprints) {
        invalidateJwtKey(deps.jwtCache, fingerprint);
        deps.onKeyRevoked?.(fingerprint);
        for (const dashboardId of revoked.dashboardIds) {
          deps.onDashboardRevoked?.(dashboardId, fingerprint);
        }
      }
      log.info("auth.password_reset", "redeemed", {
        account_id: revoked.accountId,
        revoked_device_count: revoked.fingerprints.length,
      });
      return create(AuthPasswordResetRedeemResponseSchema, { ok: true });
    },
  };
}

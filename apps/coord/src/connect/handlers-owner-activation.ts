// Managed owner activation creates the first account, organization, dashboard,
// and browser device in one transaction. Keeping the topology write together
// prevents a consumed bootstrap credential from exposing partial authority.

import { create } from "@bufbuild/protobuf";
import { Code, ConnectError, type ServiceImpl } from "@connectrpc/connect";
import { fingerprintOf } from "@roost/shared/fingerprint";
import { log } from "@roost/shared/log";
import {
  isNativePasswordLengthValid,
  NATIVE_PASSWORD_MIN_BOOTSTRAP_LENGTH,
  normalizeAccountEmail,
} from "@roost/shared/native-credentials";
import {
  AuthOwnerActivateResponseSchema,
  CoordinatorService,
} from "@roost/shared/proto/coordinator_pb";
import { decodeEd25519Pubkey } from "../authorized-keys.ts";
import { refreshJwtKey } from "../jwt.ts";
import {
  AccountDeviceEnrollmentError,
  enrollAccountDevice,
} from "./account-device-enrollment.ts";
import {
  denyRedemption,
  hashToken,
  invalidPassword,
  ONE_TIME_TOKEN,
} from "./account-one-time-credentials.ts";
import { callerKey } from "./auth-interceptor.ts";
import type { ConnectDeps } from "./router.ts";

const MAX_DEVICE_LABEL_LENGTH = 200;

function invalidActivationDetails(): never {
  throw new ConnectError("invalid activation details", Code.InvalidArgument);
}
function isActivationConstraintRace(error: unknown): boolean {
  const message = error instanceof Error ? error.message : String(error);
  return /(?:UNIQUE|FOREIGN KEY|CHECK|NOT NULL) constraint failed/i.test(message);
}

type OwnerActivationMethods = "authOwnerActivate";

export function makeOwnerActivationHandlers(
  deps: ConnectDeps,
): Pick<ServiceImpl<typeof CoordinatorService>, OwnerActivationMethods> {
  return {
    async authOwnerActivate(req, ctx) {
      if (
        !deps.cfg.managedContainer
        || !deps.cfg.instanceId
        || ctx.values.get(callerKey) !== null
        || !ONE_TIME_TOKEN.test(req.token)
      ) {
        denyRedemption();
      }
      if (!isNativePasswordLengthValid(req.newPassword, NATIVE_PASSWORD_MIN_BOOTSTRAP_LENGTH)) {
        invalidPassword();
      }

      const label = req.label.trim();
      if (
        label.length === 0
        || label.length > MAX_DEVICE_LABEL_LENGTH
        || req.sshPubkeyB64.length > 1_024
      ) {
        invalidActivationDetails();
      }
      const publicKey = decodeEd25519Pubkey(req.sshPubkeyB64);
      if (!publicKey) invalidActivationDetails();
      const fingerprint = await fingerprintOf(publicKey);

      // Memory-hard work must finish before SQLite holds the write transaction.
      const passwordHash = await deps.passwordWorkGate.hash(req.newPassword);
      const now = Date.now();
      const tokenHash = hashToken(req.token);

      let activated: { accountId: string; dashboardId: string };
      try {
        activated = await deps.db.transaction().execute(async (trx) => {
          const activation = await trx.updateTable("owner_activation_tokens")
            .set({ accepted_at_ms: now })
            .where("token_hash", "=", tokenHash)
            .where("accepted_at_ms", "is", null)
            .where("revoked_at_ms", "is", null)
            .where("expires_at_ms", ">", now)
            .returning(["account_id", "coordinator_id", "email_normalized"])
            .executeTakeFirst();
          if (
            !activation
            || activation.coordinator_id !== deps.cfg.instanceId
            || !activation.account_id
            || normalizeAccountEmail(activation.email_normalized) !== activation.email_normalized
          ) {
            denyRedemption();
          }

          const existingAccount = await trx.selectFrom("accounts")
            .select("id")
            .limit(1)
            .executeTakeFirst();
          if (existingAccount) denyRedemption();


          await trx.insertInto("accounts").values({
            id: activation.account_id,
            email_normalized: activation.email_normalized,
            password_hash: passwordHash,
            status: "active",
            created_at_ms: now,
            password_changed_at_ms: now,
          }).execute();
          await trx.insertInto("account_identities").values({
            account_id: activation.account_id,
            issuer: "native",
            subject: activation.account_id,
            email_normalized: activation.email_normalized,
            linked_at_ms: now,
            last_authenticated_at_ms: null,
            revoked_at_ms: null,
          }).execute();
          await trx.insertInto("organizations").values({
            id: activation.account_id,
            slug: "personal",
            name: activation.email_normalized,
            status: "active",
            created_at_ms: now,
          }).execute();
          await trx.insertInto("organization_memberships").values({
            organization_id: activation.account_id,
            account_id: activation.account_id,
            role: "owner",
            created_at_ms: now,
          }).execute();
          await trx.insertInto("dashboards").values({
            id: activation.coordinator_id,
            organization_id: activation.account_id,
            slug: "default",
            name: "Personal",
            status: "active",
            created_at_ms: now,
          }).execute();
          await trx.insertInto("dashboard_memberships").values({
            dashboard_id: activation.coordinator_id,
            account_id: activation.account_id,
            role: "admin",
            created_at_ms: now,
          }).execute();
          const dashboardId = await enrollAccountDevice(trx, {
            accountId: activation.account_id,
            fingerprint,
            publicKey,
            label,
            now,
            expectedDashboardId: activation.coordinator_id,
          });
          if (dashboardId !== activation.coordinator_id) denyRedemption();

          return {
            accountId: activation.account_id,
            dashboardId: activation.coordinator_id,
          };
        });
      } catch (error) {
        if (error instanceof ConnectError) throw error;
        if (error instanceof AccountDeviceEnrollmentError || isActivationConstraintRace(error)) {
          denyRedemption();
        }
        throw error;
      }

      // A browser key is admitted to JWT verification only after every owner
      // topology row and the token consumption have committed together.
      refreshJwtKey(deps.jwtCache, fingerprint);
      log.info("auth.owner_activation", "accepted", {
        account_id: activated.accountId,
        dashboard_id: activated.dashboardId,
        fingerprint,
      });
      return create(AuthOwnerActivateResponseSchema, {
        dashboardId: activated.dashboardId,
      });
    },
  };
}

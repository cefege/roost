// Federated auth handlers bind externally signed identity claims to an enrolled
// account device. Link tickets and password transitions stay in this boundary so
// public failures remain uniform and account authority changes atomically.

import { create } from "@bufbuild/protobuf";
import { Code, ConnectError, type HandlerContext, type ServiceImpl } from "@connectrpc/connect";
import { randomUUID } from "node:crypto";
import { fingerprintOf } from "@roost/shared/fingerprint";
import { log } from "@roost/shared/log";
import {
  isNativePasswordLengthValid,
  NATIVE_PASSWORD_MIN_BOOTSTRAP_LENGTH,
} from "@roost/shared/native-credentials";
import {
  AuthCredentialsGetResponseSchema,
  AuthFederatedContinueResponseSchema,
  AuthFederatedLinkBeginResponseSchema,
  AuthFederatedLinkResponseSchema,
  AuthPasswordAddResponseSchema,
  CoordinatorService,
} from "@roost/shared/proto/coordinator_pb";
import type { Transaction } from "kysely";
import { decodeEd25519Pubkey } from "../authorized-keys.ts";
import type { DB } from "../db/schema.ts";
import { refreshJwtKey } from "../jwt.ts";
import {
  AccountDeviceEnrollmentError,
  enrollAccountDevice,
} from "./account-device-enrollment.ts";
import { callerKey, requireAccountDevice } from "./auth-interceptor.ts";
import {
  createFederatedAssertionVerifier,
  GOOGLE_IDENTITY_ISSUER,
  IDENTITY_LINK_TICKET_AUDIENCE,
  type FederatedAssertionClaims,
  type FederatedAssertionPurpose,
  type VerifyFederatedAssertion,
} from "./federated-assertion.ts";
import type { ConnectDeps } from "./router.ts";

const MAX_DEVICE_LABEL_LENGTH = 200;

type FederatedAuthMethods =
  | "authFederatedContinue"
  | "authCredentialsGet"
  | "authPasswordAdd"
  | "authFederatedLinkBegin"
  | "authFederatedLink";

export type FederatedAuthHandlers = Pick<ServiceImpl<typeof CoordinatorService>, FederatedAuthMethods>;

export interface FederatedAuthHandlerOptions {
  now?: () => number;
  randomUuid?: () => string;
  verifyAssertion?: VerifyFederatedAssertion;
}

function invalidFederatedCredentials(): never {
  throw new ConnectError("invalid federated credentials", Code.Unauthenticated);
}

function googleIdentityUnavailable(): never {
  throw new ConnectError("Google identity unavailable", Code.FailedPrecondition);
}

function passwordAlreadySet(): never {
  throw new ConnectError("password is already set", Code.FailedPrecondition);
}

function invalidPassword(): never {
  throw new ConnectError("invalid password", Code.InvalidArgument);
}

function isConstraintFailure(error: unknown): boolean {
  const message = error instanceof Error ? error.message : String(error);
  return /(?:UNIQUE|FOREIGN KEY|CHECK|NOT NULL) constraint failed/i.test(message);
}

function managedCaller(deps: ConnectDeps, ctx: HandlerContext) {
  const caller = requireAccountDevice(ctx.values);
  if (
    !deps.cfg.managedContainer
    || !deps.cfg.instanceId
    || !deps.cfg.tenantRouteKey
    || caller.kind !== "account-device"
  ) invalidFederatedCredentials();
  return caller;
}

async function recordAssertionRedemption(
  trx: Transaction<DB>,
  claims: FederatedAssertionClaims,
  redeemedAtMs: number,
): Promise<void> {
  await trx.insertInto("federated_assertion_redemptions").values({
    jti: claims.jti,
    purpose: claims.purpose,
    device_fp: claims.device_fp,
    redeemed_at_ms: redeemedAtMs,
    expires_at_ms: claims.exp * 1_000,
  }).onConflict((conflict) => conflict.column("jti").doNothing()).execute();
  const redemption = await trx.selectFrom("federated_assertion_redemptions")
    .select(["purpose", "device_fp", "expires_at_ms"])
    .where("jti", "=", claims.jti)
    .executeTakeFirst();
  if (
    !redemption
    || redemption.purpose !== claims.purpose
    || redemption.device_fp !== claims.device_fp
    || redemption.expires_at_ms !== claims.exp * 1_000
  ) invalidFederatedCredentials();
}

function assertionMatchesTenant(
  deps: ConnectDeps,
  claims: FederatedAssertionClaims,
  purpose: FederatedAssertionPurpose,
  accountId?: string,
  fingerprint?: string,
): boolean {
  return claims.purpose === purpose
    && claims.coordinator_id === deps.cfg.instanceId
    && claims.route_key === deps.cfg.tenantRouteKey
    && (accountId === undefined || claims.account_id === accountId)
    && (fingerprint === undefined || claims.device_fp === fingerprint);
}

export function makeFederatedAuthHandlers(
  deps: ConnectDeps,
  options: FederatedAuthHandlerOptions = {},
): FederatedAuthHandlers {
  const now = options.now ?? Date.now;
  const newUuid = options.randomUuid ?? randomUUID;
  const verifyAssertion = options.verifyAssertion
    ?? (deps.cfg.saasAuthVerifyKeyPath
      ? createFederatedAssertionVerifier(deps.cfg.saasAuthVerifyKeyPath)
      : undefined);

  const verify = async (assertion: string, purpose: FederatedAssertionPurpose) => {
    if (!verifyAssertion) invalidFederatedCredentials();
    try {
      return await verifyAssertion(assertion, purpose, now());
    } catch {
      invalidFederatedCredentials();
    }
  };

  return {
    async authFederatedContinue(req, _ctx) {
      if (
        !deps.cfg.managedContainer
        || !deps.cfg.instanceId
        || !deps.cfg.tenantRouteKey
        || !verifyAssertion
      ) invalidFederatedCredentials();
      const label = req.label.trim();
      if (label.length === 0 || label.length > MAX_DEVICE_LABEL_LENGTH || req.sshPubkeyB64.length > 1_024) {
        invalidFederatedCredentials();
      }
      const publicKey = decodeEd25519Pubkey(req.sshPubkeyB64);
      if (!publicKey) invalidFederatedCredentials();
      const fingerprint = await fingerprintOf(publicKey);
      const claims = await verify(req.assertion, "continue");
      if (!assertionMatchesTenant(deps, claims, "continue", undefined, fingerprint)) {
        invalidFederatedCredentials();
      }

      const timestamp = now();
      let dashboardId: string;
      try {
        dashboardId = await deps.db.transaction().execute(async (trx) => {
          const identity = await trx.selectFrom("account_identities as identity")
            .innerJoin("accounts as account", "account.id", "identity.account_id")
            .select("identity.account_id as accountId")
            .where("identity.issuer", "=", GOOGLE_IDENTITY_ISSUER)
            .where("identity.subject", "=", claims.identity_subject)
            .where("identity.account_id", "=", claims.account_id)
            .where("identity.revoked_at_ms", "is", null)
            .where("account.status", "=", "active")
            .executeTakeFirst();
          if (!identity) invalidFederatedCredentials();
          await recordAssertionRedemption(trx, claims, timestamp);
          const selectedDashboard = await enrollAccountDevice(trx, {
            accountId: identity.accountId,
            fingerprint,
            publicKey,
            label,
            now: timestamp,
            expectedDashboardId: deps.cfg.instanceId,
          });
          const updated = await trx.updateTable("account_identities")
            .set({
              email_normalized: claims.email_normalized,
              last_authenticated_at_ms: timestamp,
            })
            .where("issuer", "=", GOOGLE_IDENTITY_ISSUER)
            .where("subject", "=", claims.identity_subject)
            .where("account_id", "=", claims.account_id)
            .where("revoked_at_ms", "is", null)
            .executeTakeFirst();
          if (Number(updated.numUpdatedRows) !== 1) invalidFederatedCredentials();
          return selectedDashboard;
        });
      } catch (error) {
        if (error instanceof ConnectError || error instanceof AccountDeviceEnrollmentError || isConstraintFailure(error)) {
          invalidFederatedCredentials();
        }
        throw error;
      }
      refreshJwtKey(deps.jwtCache, fingerprint);
      log.info("auth.federated", "device_bound", {
        account_id: claims.account_id,
        dashboard_id: dashboardId,
        fingerprint,
      });
      return create(AuthFederatedContinueResponseSchema, { dashboardId });
    },

    async authCredentialsGet(_req, ctx) {
      const caller = managedCaller(deps, ctx);
      const account = await deps.db.selectFrom("accounts as account")
        .innerJoin("account_devices as device", "device.account_id", "account.id")
        .select(["account.email_normalized as email", "account.password_hash as passwordHash"])
        .where("account.id", "=", caller.accountId)
        .where("device.fingerprint", "=", caller.fingerprint)
        .where("account.status", "=", "active")
        .executeTakeFirst();
      if (!account) invalidFederatedCredentials();
      const google = await deps.db.selectFrom("account_identities")
        .select("subject")
        .where("account_id", "=", caller.accountId)
        .where("issuer", "=", GOOGLE_IDENTITY_ISSUER)
        .where("revoked_at_ms", "is", null)
        .executeTakeFirst();
      return create(AuthCredentialsGetResponseSchema, {
        email: account.email,
        hasPassword: account.passwordHash !== null,
        googleLinked: Boolean(google),
      });
    },

    async authPasswordAdd(req, ctx) {
      const caller = managedCaller(deps, ctx);
      if (!isNativePasswordLengthValid(req.newPassword, NATIVE_PASSWORD_MIN_BOOTSTRAP_LENGTH)) {
        invalidPassword();
      }
      const eligible = await deps.db.selectFrom("accounts as account")
        .innerJoin("account_devices as device", "device.account_id", "account.id")
        .innerJoin("account_identities as identity", "identity.account_id", "account.id")
        .select(["account.id", "account.email_normalized as email"])
        .where("account.id", "=", caller.accountId)
        .where("device.fingerprint", "=", caller.fingerprint)
        .where("account.status", "=", "active")
        .where("account.password_hash", "is", null)
        .where("identity.issuer", "=", GOOGLE_IDENTITY_ISSUER)
        .where("identity.revoked_at_ms", "is", null)
        .executeTakeFirst();
      if (!eligible) passwordAlreadySet();

      const passwordHash = await deps.passwordWorkGate.hash(req.newPassword);
      const timestamp = now();
      try {
        await deps.db.transaction().execute(async (trx) => {
          const google = await trx.selectFrom("account_identities")
            .select("subject")
            .where("account_id", "=", caller.accountId)
            .where("issuer", "=", GOOGLE_IDENTITY_ISSUER)
            .where("revoked_at_ms", "is", null)
            .executeTakeFirst();
          if (!google) passwordAlreadySet();
          const changed = await trx.updateTable("accounts")
            .set({ password_hash: passwordHash, password_changed_at_ms: timestamp })
            .where("id", "=", caller.accountId)
            .where("email_normalized", "=", eligible.email)
            .where("status", "=", "active")
            .where("password_hash", "is", null)
            .returning("id")
            .executeTakeFirst();
          if (!changed) passwordAlreadySet();
          const native = await trx.selectFrom("account_identities")
            .select("account_id")
            .where("issuer", "=", "native")
            .where("subject", "=", caller.accountId)
            .executeTakeFirst();
          if (native && native.account_id !== caller.accountId) passwordAlreadySet();
          await trx.insertInto("account_identities").values({
            account_id: caller.accountId,
            issuer: "native",
            subject: caller.accountId,
            email_normalized: eligible.email,
            linked_at_ms: timestamp,
            last_authenticated_at_ms: null,
            revoked_at_ms: null,
          }).onConflict((conflict) => conflict.columns(["issuer", "subject"]).doUpdateSet({
            email_normalized: eligible.email,
            linked_at_ms: timestamp,
            last_authenticated_at_ms: null,
            revoked_at_ms: null,
          })).execute();
        });
      } catch (error) {
        if (error instanceof ConnectError || isConstraintFailure(error)) passwordAlreadySet();
        throw error;
      }
      return create(AuthPasswordAddResponseSchema, { ok: true });
    },

    async authFederatedLinkBegin(_req, ctx) {
      const caller = managedCaller(deps, ctx);
      const account = await deps.db.selectFrom("account_devices as device")
        .innerJoin("accounts as account", "account.id", "device.account_id")
        .select("account.id")
        .where("device.fingerprint", "=", caller.fingerprint)
        .where("account.id", "=", caller.accountId)
        .where("account.status", "=", "active")
        .executeTakeFirst();
      if (!account) invalidFederatedCredentials();
      const issuedAt = Math.floor(now() / 1_000);
      const linkTicket = await deps.coordKey.sign({
        aud: IDENTITY_LINK_TICKET_AUDIENCE,
        sub: caller.fingerprint,
        iat: issuedAt,
        exp: issuedAt + 300,
        account_id: caller.accountId,
        coordinator_id: deps.cfg.instanceId,
        route_key: deps.cfg.tenantRouteKey,
        device_fp: caller.fingerprint,
        jti: newUuid(),
      });
      return create(AuthFederatedLinkBeginResponseSchema, { linkTicket });
    },

    async authFederatedLink(req, ctx) {
      const caller = managedCaller(deps, ctx);
      const claims = await verify(req.assertion, "link");
      if (!assertionMatchesTenant(deps, claims, "link", caller.accountId, caller.fingerprint)) {
        invalidFederatedCredentials();
      }
      const timestamp = now();
      try {
        await deps.db.transaction().execute(async (trx) => {
          const account = await trx.selectFrom("accounts as account")
            .innerJoin("account_devices as device", "device.account_id", "account.id")
            .select("account.email_normalized as email")
            .where("account.id", "=", caller.accountId)
            .where("device.fingerprint", "=", caller.fingerprint)
            .where("account.status", "=", "active")
            .executeTakeFirst();
          if (!account) invalidFederatedCredentials();
          if (claims.email_normalized !== account.email) googleIdentityUnavailable();
          await recordAssertionRedemption(trx, claims, timestamp);
          const identity = await trx.selectFrom("account_identities")
            .select("account_id")
            .where("issuer", "=", GOOGLE_IDENTITY_ISSUER)
            .where("subject", "=", claims.identity_subject)
            .executeTakeFirst();
          if (identity && identity.account_id !== caller.accountId) googleIdentityUnavailable();
          await trx.insertInto("account_identities").values({
            account_id: caller.accountId,
            issuer: GOOGLE_IDENTITY_ISSUER,
            subject: claims.identity_subject,
            email_normalized: claims.email_normalized,
            linked_at_ms: timestamp,
            last_authenticated_at_ms: timestamp,
            revoked_at_ms: null,
          }).onConflict((conflict) => conflict.columns(["issuer", "subject"]).doUpdateSet({
            email_normalized: claims.email_normalized,
            last_authenticated_at_ms: timestamp,
            revoked_at_ms: null,
          })).execute();
        });
      } catch (error) {
        if (error instanceof ConnectError) throw error;
        if (isConstraintFailure(error)) googleIdentityUnavailable();
        throw error;
      }
      return create(AuthFederatedLinkResponseSchema, { ok: true });
    },
  };
}

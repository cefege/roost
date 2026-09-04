// Managed native-account login. Password auth is used only to bind a browser's
// non-extractable Ed25519 key; all subsequent requests use the existing
// browser-signed device JWT.

import { create } from "@bufbuild/protobuf";
import { Code, ConnectError, type ServiceImpl } from "@connectrpc/connect";
import { randomBytes } from "node:crypto";
import { fingerprintOf } from "@roost/shared/fingerprint";
import { log } from "@roost/shared/log";
import {
  isNativePasswordLengthValid,
  normalizeAccountEmail,
} from "@roost/shared/native-credentials";
import {
  AuthPasswordLoginResponseSchema,
  CoordinatorService,
} from "@roost/shared/proto/coordinator_pb";
import { decodeEd25519Pubkey } from "../authorized-keys.ts";
import { refreshJwtKey } from "../jwt.ts";
import type { ConnectDeps } from "./router.ts";
import type { PasswordWorkGate } from "./password-work-gate.ts";
import {
  AccountDeviceEnrollmentError,
  enrollAccountDevice,
} from "./account-device-enrollment.ts";

type NativeAuthMethods = "authPasswordLogin";

export type NativeAuthHandlers = Pick<ServiceImpl<typeof CoordinatorService>, NativeAuthMethods>;

export interface NativeAuthHandlerOptions {
  /** Test seam for forcing a password-change race after verification. */
  verifyPassword?: (password: string, passwordHash: string) => Promise<boolean>;
}

const MAX_DEVICE_LABEL_LENGTH = 200;
const dummyPasswordHashPromises = new WeakMap<PasswordWorkGate, Promise<string>>();

/** A real Argon2id verification for unknown/disabled/passwordless accounts
 * keeps public login timing from becoming a useful account-existence oracle. */
function dummyPasswordHash(gate: PasswordWorkGate): Promise<string> {
  const existing = dummyPasswordHashPromises.get(gate);
  if (existing) return existing;

  const randomPassword = randomBytes(32).toString("base64url");
  const pending = gate.hash(randomPassword).catch((error) => {
    if (dummyPasswordHashPromises.get(gate) === pending) {
      dummyPasswordHashPromises.delete(gate);
    }
    throw error;
  });
  dummyPasswordHashPromises.set(gate, pending);
  return pending;
}

export async function prepareNativeAuthDummyHash(gate: PasswordWorkGate): Promise<void> {
  await dummyPasswordHash(gate);
}

function invalidCredentials(): never {
  throw new ConnectError("invalid credentials", Code.Unauthenticated);
}

function isCredentialRace(error: unknown): boolean {
  const message = error instanceof Error ? error.message : String(error);
  return error instanceof AccountDeviceEnrollmentError
    || /(?:UNIQUE|FOREIGN KEY|CHECK|NOT NULL) constraint failed/i.test(message);
}

export function makeNativeAuthHandlers(
  deps: ConnectDeps,
  options: NativeAuthHandlerOptions = {},
): NativeAuthHandlers {
  return {
    async authPasswordLogin(req, _ctx) {
      // Self-hosted pairing/on-host authorization is intentionally unchanged.
      if (!deps.cfg.saasMode) invalidCredentials();

      const emailNormalized = normalizeAccountEmail(req.email);
      const passwordLengthValid = isNativePasswordLengthValid(req.password);
      const account = emailNormalized
        ? await deps.db.selectFrom("accounts as account")
          .innerJoin("account_identities as identity", "identity.account_id", "account.id")
          .select(["account.id as id", "account.password_hash as passwordHash"])
          .where("account.email_normalized", "=", emailNormalized)
          .where("account.status", "=", "active")
          .where("identity.issuer", "=", "native")
          .whereRef("identity.subject", "=", "account.id")
          .whereRef("identity.email_normalized", "=", "account.email_normalized")
          .where("identity.revoked_at_ms", "is", null)
          .executeTakeFirst()
        : undefined;

      const passwordHash = account?.passwordHash
        ?? await dummyPasswordHash(deps.passwordWorkGate);
      let passwordMatches = false;
      try {
        // Never hand an attacker-controlled oversized value to Argon2id.
        const password = passwordLengthValid ? req.password : "";
        passwordMatches = options.verifyPassword
          ? await deps.passwordWorkGate.run(
            () => options.verifyPassword!(password, passwordHash),
          )
          : await deps.passwordWorkGate.verify(password, passwordHash);
      } catch (error) {
        if (error instanceof ConnectError && error.code === Code.ResourceExhausted) {
          throw error;
        }
        // A missing/corrupt stored hash has the same public result as every
        // other credential failure.
      }
      if (!emailNormalized || !passwordLengthValid || !account?.passwordHash || !passwordMatches) {
        invalidCredentials();
      }

      const label = req.label.trim();
      if (label.length === 0 || label.length > MAX_DEVICE_LABEL_LENGTH || req.sshPubkeyB64.length > 1_024) {
        invalidCredentials();
      }
      const publicKey = decodeEd25519Pubkey(req.sshPubkeyB64);
      if (!publicKey) invalidCredentials();
      const fingerprint = await fingerprintOf(publicKey);
      const now = Date.now();

      let dashboardId: string;
      try {
        dashboardId = await deps.db.transaction().execute(async (trx) => {
          // Password hashing is deliberately outside the write transaction,
          // but its exact account and native-identity state is fenced here.
          const currentAccount = await trx.selectFrom("accounts as account")
            .innerJoin("account_identities as identity", "identity.account_id", "account.id")
            .select("account.id")
            .where("account.id", "=", account.id)
            .where("account.email_normalized", "=", emailNormalized)
            .where("account.password_hash", "=", account.passwordHash)
            .where("account.status", "=", "active")
            .where("identity.issuer", "=", "native")
            .whereRef("identity.subject", "=", "account.id")
            .whereRef("identity.email_normalized", "=", "account.email_normalized")
            .where("identity.revoked_at_ms", "is", null)
            .executeTakeFirst();
          if (!currentAccount) invalidCredentials();

          const selectedDashboard = await enrollAccountDevice(trx, {
            accountId: account.id,
            fingerprint,
            publicKey,
            label,
            now,
            expectedDashboardId: deps.cfg.managedContainer ? deps.cfg.instanceId : undefined,
          });
          const authenticated = await trx.updateTable("account_identities")
            .set({ last_authenticated_at_ms: now })
            .where("issuer", "=", "native")
            .where("subject", "=", account.id)
            .where("account_id", "=", account.id)
            .where("revoked_at_ms", "is", null)
            .executeTakeFirst();
          if (Number(authenticated.numUpdatedRows) !== 1) invalidCredentials();
          return selectedDashboard;
        });
      } catch (error) {
        if (error instanceof ConnectError || isCredentialRace(error)) invalidCredentials();
        throw error;
      }

      // Only committed keys are admitted to JWT verification.
      refreshJwtKey(deps.jwtCache, fingerprint);
      log.info("auth.native", "device_bound", {
        account_id: account.id,
        dashboard_id: dashboardId,
        fingerprint,
      });
      return create(AuthPasswordLoginResponseSchema, { dashboardId });
    },
  };
}

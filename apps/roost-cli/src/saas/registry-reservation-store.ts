// Owns account reservations and federated-identity state changes.
// Provisioning callers use this store before creating durable work records.
// Immediate transactions preserve one account, route, and ordinal-one coordinator.
import { normalizeAccountEmail } from "@roost/shared/native-credentials";
import { RegistryCoordinatorStore } from "./registry-coordinator-store.ts";
import { allocateTenantRouteKey, immediate } from "./registry-schema.ts";
import {
  type AccountReservation,
  type GoogleSignupReservation,
  type RegistryFederatedIdentity,
  type ReserveGoogleSignupOptions,
} from "./registry-model.ts";
import type {
  RawAccount,
  RawCoordinator,
  RawFederatedIdentity,
} from "./registry-row-types.ts";
import {
  mapAccount,
  mapCoordinator,
  mapFederatedIdentity,
} from "./registry-row-mappers.ts";
import {
  SaasRegistryError,
  assertCanonicalGoogleIssuer,
  assertCanonicalUuid,
  assertGoogleIdentitySubject,
  assertImmutableImageDigest,
  assertNormalizedEmail,
  assertSafeTimestamp,
  checkedNow,
  coordinatorContainerName,
  coordinatorDataDir,
  coordinatorHostname,
} from "./registry-validation.ts";

export class RegistryReservationStore extends RegistryCoordinatorStore {
  reserveAccount(emailRaw: string, imageDigestRaw: string): AccountReservation {
    const email = normalizeAccountEmail(emailRaw);
    if (!email) throw new SaasRegistryError("invalid account email", "invalid");
    const imageDigest = assertImmutableImageDigest(imageDigestRaw);
    return immediate(this.sqlite, () => {
      const existingRaw = this.sqlite.query(
        "SELECT * FROM accounts WHERE email_normalized = ?",
      ).get(email) as RawAccount | null;
      if (existingRaw) {
        const account = mapAccount(existingRaw);
        if (account.state === "active") {
          throw new SaasRegistryError(`account ${email} is already active`, "conflict");
        }
        if (account.state === "disabled") {
          throw new SaasRegistryError(`account ${email} is disabled`, "conflict");
        }
        const coordinatorRaw = this.sqlite.query(`
          SELECT c.*, a.route_key
          FROM coordinators c
          JOIN accounts a ON a.id = c.account_id
          WHERE c.account_id = ? AND c.ordinal = 1
        `).get(account.id) as RawCoordinator | null;
        if (!coordinatorRaw) throw new SaasRegistryError("pending account has no reserved coordinator", "corrupt");
        return { account, coordinator: mapCoordinator(coordinatorRaw, this.rootDir), resumed: true };
      }

      const accountId = assertCanonicalUuid(this.createId(), "generated account id");
      const coordinatorId = assertCanonicalUuid(this.createId(), "generated coordinator id");
      if (accountId === coordinatorId) throw new SaasRegistryError("generated IDs collided", "invalid");
      const routeKey = allocateTenantRouteKey(this.sqlite, this.createRouteKey);
      const timestamp = checkedNow(this.now);
      this.sqlite.query(`
        INSERT INTO accounts (
          id, email_normalized, route_key, state, created_at_ms, activated_at_ms, disabled_at_ms
        ) VALUES (?, ?, ?, 'pending', ?, NULL, NULL)
      `).run(accountId, email, routeKey, timestamp);
      this.sqlite.query(`
        INSERT INTO coordinators (
          id, account_id, ordinal, hostname, container_name, data_dir,
          image_digest, state, created_at_ms, seeded_at_ms, running_at_ms,
          routed_at_ms, invited_at_ms, activated_at_ms, disabled_at_ms,
          failed_at_ms, updated_at_ms, last_error
        ) VALUES (?, ?, 1, ?, ?, ?, ?, 'reserved', ?, NULL, NULL, NULL, NULL, NULL, NULL, NULL, ?, NULL)
      `).run(
        coordinatorId,
        accountId,
        coordinatorHostname(coordinatorId),
        coordinatorContainerName(coordinatorId),
        coordinatorDataDir(this.rootDir, coordinatorId),
        imageDigest,
        timestamp,
        timestamp,
      );
      return {
        account: this.getAccount(accountId),
        coordinator: this.getCoordinator(coordinatorId),
        resumed: false,
      };
    });
  }

  reserveGoogleSignup(options: ReserveGoogleSignupOptions): GoogleSignupReservation {
    const issuer = assertCanonicalGoogleIssuer(options.issuer);
    const subject = assertGoogleIdentitySubject(options.subject);
    const emailNormalized = assertNormalizedEmail(options.emailNormalized);
    const verifiedAtMs = assertSafeTimestamp(options.verifiedAtMs, "verifiedAtMs");
    const imageDigest = assertImmutableImageDigest(options.imageDigest);
    return immediate(this.sqlite, () => {
      const timestamp = checkedNow(this.now);
      if (verifiedAtMs > timestamp) {
        throw new SaasRegistryError("verifiedAtMs cannot be in the future", "invalid");
      }
      const existingIdentityRaw = this.sqlite.query(`
        SELECT * FROM federated_identities WHERE issuer = ? AND subject = ?
      `).get(issuer, subject) as RawFederatedIdentity | null;
      if (existingIdentityRaw) {
        this.sqlite.query(`
          UPDATE federated_identities
          SET email_normalized = ?, verified_at_ms = ?, updated_at_ms = ?
          WHERE issuer = ? AND subject = ?
        `).run(emailNormalized, verifiedAtMs, timestamp, issuer, subject);
        const identity = this.getFederatedIdentity(issuer, subject)!;
        const account = this.getAccount(identity.accountId);
        const coordinatorRaw = this.sqlite.query(`
          SELECT c.*, a.route_key
          FROM coordinators c
          JOIN accounts a ON a.id = c.account_id
          WHERE c.account_id = ? AND c.ordinal = 1
        `).get(account.id) as RawCoordinator | null;
        if (!coordinatorRaw) {
          throw new SaasRegistryError("federated account has no ordinal-one coordinator", "corrupt");
        }
        return {
          outcome: "existing",
          account,
          coordinator: mapCoordinator(coordinatorRaw, this.rootDir),
          identity,
          resumed: true,
        };
      }

      if (this.sqlite.query("SELECT 1 FROM accounts WHERE email_normalized = ?").get(emailNormalized)) {
        return { outcome: "proof-required" };
      }

      const accountId = assertCanonicalUuid(this.createId(), "generated account id");
      const coordinatorId = assertCanonicalUuid(this.createId(), "generated coordinator id");
      if (accountId === coordinatorId) throw new SaasRegistryError("generated IDs collided", "invalid");
      const routeKey = allocateTenantRouteKey(this.sqlite, this.createRouteKey);
      this.sqlite.query(`
        INSERT INTO accounts (
          id, email_normalized, route_key, state, created_at_ms, activated_at_ms, disabled_at_ms
        ) VALUES (?, ?, ?, 'pending', ?, NULL, NULL)
      `).run(accountId, emailNormalized, routeKey, timestamp);
      this.sqlite.query(`
        INSERT INTO coordinators (
          id, account_id, ordinal, hostname, container_name, data_dir,
          image_digest, state, created_at_ms, seeded_at_ms, running_at_ms,
          routed_at_ms, invited_at_ms, activated_at_ms, disabled_at_ms,
          failed_at_ms, updated_at_ms, last_error
        ) VALUES (?, ?, 1, ?, ?, ?, ?, 'reserved', ?, NULL, NULL, NULL, NULL, NULL, NULL, NULL, ?, NULL)
      `).run(
        coordinatorId,
        accountId,
        coordinatorHostname(coordinatorId),
        coordinatorContainerName(coordinatorId),
        coordinatorDataDir(this.rootDir, coordinatorId),
        imageDigest,
        timestamp,
        timestamp,
      );
      this.sqlite.query(`
        INSERT INTO federated_identities (
          issuer, subject, account_id, email_normalized, state,
          created_at_ms, updated_at_ms, verified_at_ms
        ) VALUES (?, ?, ?, ?, 'reserved', ?, ?, ?)
      `).run(issuer, subject, accountId, emailNormalized, timestamp, timestamp, verifiedAtMs);
      return {
        outcome: "reserved",
        account: this.getAccount(accountId),
        coordinator: this.getCoordinator(coordinatorId),
        identity: this.getFederatedIdentity(issuer, subject)!,
        resumed: false,
      };
    });
  }

  getFederatedIdentity(issuerRaw: string, subjectRaw: string): RegistryFederatedIdentity | null {
    const issuer = assertCanonicalGoogleIssuer(issuerRaw);
    const subject = assertGoogleIdentitySubject(subjectRaw);
    const raw = this.sqlite.query(`
      SELECT * FROM federated_identities WHERE issuer = ? AND subject = ?
    `).get(issuer, subject) as RawFederatedIdentity | null;
    return raw ? mapFederatedIdentity(raw) : null;
  }

  activateFederatedIdentity(
    issuerRaw: string,
    subjectRaw: string,
    accountId: string,
  ): RegistryFederatedIdentity {
    const issuer = assertCanonicalGoogleIssuer(issuerRaw);
    const subject = assertGoogleIdentitySubject(subjectRaw);
    assertCanonicalUuid(accountId, "identity account id");
    return immediate(this.sqlite, () => {
      const identity = this.getFederatedIdentity(issuer, subject);
      if (!identity) throw new SaasRegistryError("federated identity not found", "not-found");
      if (identity.accountId !== accountId) {
        throw new SaasRegistryError("federated identity belongs to another account", "conflict");
      }
      if (identity.state === "revoked") {
        throw new SaasRegistryError("federated identity is permanently revoked", "conflict");
      }
      if (identity.state === "active") return identity;
      const timestamp = checkedNow(this.now);
      const result = this.sqlite.query(`
        UPDATE federated_identities
        SET state = 'active', updated_at_ms = ?
        WHERE issuer = ? AND subject = ? AND account_id = ? AND state = 'reserved'
      `).run(timestamp, issuer, subject, accountId);
      if (result.changes !== 1) {
        throw new SaasRegistryError("federated identity state changed concurrently", "conflict");
      }
      return this.getFederatedIdentity(issuer, subject)!;
    });
  }

  revokeFederatedIdentity(
    issuerRaw: string,
    subjectRaw: string,
    accountId: string,
  ): RegistryFederatedIdentity {
    const issuer = assertCanonicalGoogleIssuer(issuerRaw);
    const subject = assertGoogleIdentitySubject(subjectRaw);
    assertCanonicalUuid(accountId, "identity account id");
    return immediate(this.sqlite, () => {
      const identity = this.getFederatedIdentity(issuer, subject);
      if (!identity) throw new SaasRegistryError("federated identity not found", "not-found");
      if (identity.accountId !== accountId) {
        throw new SaasRegistryError("federated identity belongs to another account", "conflict");
      }
      if (identity.state === "revoked") return identity;
      const timestamp = checkedNow(this.now);
      const result = this.sqlite.query(`
        UPDATE federated_identities
        SET state = 'revoked', updated_at_ms = ?
        WHERE issuer = ? AND subject = ? AND account_id = ? AND state IN ('reserved','active')
      `).run(timestamp, issuer, subject, accountId);
      if (result.changes !== 1) {
        throw new SaasRegistryError("federated identity state changed concurrently", "conflict");
      }
      return this.getFederatedIdentity(issuer, subject)!;
    });
  }
}

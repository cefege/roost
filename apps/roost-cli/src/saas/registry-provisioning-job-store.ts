// Owns durable provisioning-job insertion, claiming, and completion state.
// The provisioning worker uses these compare-and-set mutations under leases.
// Idempotency and assertion bindings remain checked inside registry transactions.
import { randomUUID } from "node:crypto";
import { RegistryReservationStore } from "./registry-reservation-store.ts";
import { immediate } from "./registry-schema.ts";
import {
  GOOGLE_IDENTITY_ISSUER,
  type ClaimedProvisioningJob,
  type ClaimDueProvisioningJobsOptions,
  type InsertProvisioningJobOptions,
  type ProvisioningAssertionInput,
  type ProvisioningJobInsertion,
  type RegistryProvisioningJob,
} from "./registry-model.ts";
import type { RawProvisioningJob } from "./registry-row-types.ts";
import { mapProvisioningJob } from "./registry-row-mappers.ts";
import {
  MAX_LEASE_DURATION_MS,
  MAX_PROVISIONING_CLAIM_LIMIT,
  SaasRegistryError,
  assertAssertionInput,
  assertCanonicalGoogleIssuer,
  assertCanonicalUuid,
  assertGoogleIdentitySubject,
  assertNormalizedEmail,
  assertProvisioningKind,
  assertRedactedError,
  assertSafeTimestamp,
  assertSha256Hex,
  checkedNow,
  corrupt,
} from "./registry-validation.ts";

export class RegistryProvisioningJobStore extends RegistryReservationStore {
  insertProvisioningJob(options: InsertProvisioningJobOptions): ProvisioningJobInsertion {
    const idempotencyKeyHash = assertSha256Hex(options.idempotencyKeyHash, "idempotency key hash");
    const kind = assertProvisioningKind(options.kind);
    const emailNormalized = assertNormalizedEmail(options.emailNormalized);
    const verifiedAtMs = assertSafeTimestamp(options.verifiedAtMs, "verifiedAtMs");
    const accountId = assertCanonicalUuid(options.accountId, "provisioning account id");
    const coordinatorId = assertCanonicalUuid(options.coordinatorId, "provisioning coordinator id");
    let identityIssuer: typeof GOOGLE_IDENTITY_ISSUER | null = null;
    let identitySubject: string | null = null;
    let activationTokenHash: string | null = null;
    if (kind === "verified-email") {
      if (options.identityIssuer != null || options.identitySubject != null || options.activationTokenHash == null) {
        throw new SaasRegistryError("verified-email job has inconsistent claims", "invalid");
      }
      activationTokenHash = assertSha256Hex(options.activationTokenHash, "activation token hash");
    } else {
      if (options.identityIssuer == null || options.identitySubject == null || options.activationTokenHash != null) {
        throw new SaasRegistryError("Google job has inconsistent claims", "invalid");
      }
      identityIssuer = assertCanonicalGoogleIssuer(options.identityIssuer);
      identitySubject = assertGoogleIdentitySubject(options.identitySubject);
    }
    return immediate(this.sqlite, () => {
      const timestamp = checkedNow(this.now);
      if (verifiedAtMs > timestamp) {
        throw new SaasRegistryError("verifiedAtMs cannot be in the future", "invalid");
      }
      const nextAttemptAtMs = options.nextAttemptAtMs === undefined
        ? timestamp
        : assertSafeTimestamp(options.nextAttemptAtMs, "nextAttemptAtMs");
      const account = this.getAccount(accountId);
      const coordinator = this.getCoordinator(coordinatorId);
      if (coordinator.accountId !== accountId) {
        throw new SaasRegistryError("provisioning coordinator belongs to another account", "conflict");
      }
      if (kind === "verified-email") {
        if (account.emailNormalized !== emailNormalized) {
          throw new SaasRegistryError("verified email does not match account", "conflict");
        }
      } else {
        const identity = this.getFederatedIdentity(identityIssuer!, identitySubject!);
        if (!identity || identity.accountId !== accountId || identity.state === "revoked") {
          throw new SaasRegistryError("Google identity is unavailable", "conflict");
        }
        if (kind === "google-login" && identity.state !== "active") {
          throw new SaasRegistryError("Google identity is not active", "conflict");
        }
        if (identity.emailNormalized !== emailNormalized) {
          throw new SaasRegistryError("verified Google email is stale", "conflict");
        }
      }
      const assertionInput = options.assertionInput == null
        ? null
        : assertAssertionInput(options.assertionInput, kind, account.routeKey);
      const existingRaw = this.sqlite.query(`
        SELECT * FROM provisioning_jobs WHERE idempotency_key_hash = ?
      `).get(idempotencyKeyHash) as RawProvisioningJob | null;
      if (existingRaw) {
        const existing = mapProvisioningJob(existingRaw);
        const sameAssertion = assertionInput === null || (
          existing.assertionInput !== null
          && assertionInput !== null
          && existing.assertionInput.purpose === assertionInput.purpose
          && existing.assertionInput.routeKey === assertionInput.routeKey
          && existing.assertionInput.deviceFingerprint === assertionInput.deviceFingerprint
          && existing.assertionInput.jti === assertionInput.jti
          && existing.assertionInput.issuedAtMs === assertionInput.issuedAtMs
          && existing.assertionInput.expiresAtMs === assertionInput.expiresAtMs
        );
        if (
          existing.kind !== kind
          || existing.emailNormalized !== emailNormalized
          || existing.identityIssuer !== identityIssuer
          || existing.identitySubject !== identitySubject
          || existing.activationTokenHash !== activationTokenHash
          || existing.verifiedAtMs !== verifiedAtMs
          || existing.accountId !== accountId
          || existing.coordinatorId !== coordinatorId
          || !sameAssertion
        ) {
          throw new SaasRegistryError("idempotency key hash was reused for different provisioning semantics", "conflict");
        }
        return { job: existing, inserted: false };
      }
      const id = assertCanonicalUuid(this.createId(), "generated provisioning job id");
      this.sqlite.query(`
        INSERT INTO provisioning_jobs (
          id, idempotency_key_hash, kind, email_normalized,
          identity_issuer, identity_subject, activation_token_hash, verified_at_ms,
          account_id, coordinator_id, state, attempts, next_attempt_at_ms,
          locked_until_ms, lease_token, last_error,
          assertion_purpose, assertion_route_key, assertion_device_fp,
          assertion_jti, assertion_issued_at_ms, assertion_expires_at_ms,
          created_at_ms, updated_at_ms, succeeded_at_ms, failed_at_ms
        ) VALUES (
          ?, ?, ?, ?, ?, ?, ?, ?, ?, ?,
          'pending', 0, ?, NULL, NULL, NULL,
          ?, ?, ?, ?, ?, ?, ?, ?, NULL, NULL
        )
      `).run(
        id,
        idempotencyKeyHash,
        kind,
        emailNormalized,
        identityIssuer,
        identitySubject,
        activationTokenHash,
        verifiedAtMs,
        accountId,
        coordinatorId,
        nextAttemptAtMs,
        assertionInput?.purpose ?? null,
        assertionInput?.routeKey ?? null,
        assertionInput?.deviceFingerprint ?? null,
        assertionInput?.jti ?? null,
        assertionInput?.issuedAtMs ?? null,
        assertionInput?.expiresAtMs ?? null,
        timestamp,
        timestamp,
      );
      return { job: this.getProvisioningJob(id), inserted: true };
    });
  }

  getProvisioningJob(id: string): RegistryProvisioningJob {
    assertCanonicalUuid(id, "provisioning job id");
    const raw = this.sqlite.query("SELECT * FROM provisioning_jobs WHERE id = ?")
      .get(id) as RawProvisioningJob | null;
    if (!raw) throw new SaasRegistryError("provisioning job not found", "not-found");
    return mapProvisioningJob(raw);
  }

  getProvisioningJobByIdempotencyKeyHash(hashRaw: string): RegistryProvisioningJob | null {
    const hash = assertSha256Hex(hashRaw, "idempotency key hash");
    const raw = this.sqlite.query("SELECT * FROM provisioning_jobs WHERE idempotency_key_hash = ?")
      .get(hash) as RawProvisioningJob | null;
    return raw ? mapProvisioningJob(raw) : null;
  }

  claimDueProvisioningJobs(options: ClaimDueProvisioningJobsOptions): ClaimedProvisioningJob[] {
    if (
      !Number.isSafeInteger(options.leaseDurationMs)
      || options.leaseDurationMs < 1
      || options.leaseDurationMs > MAX_LEASE_DURATION_MS
    ) throw new SaasRegistryError("invalid provisioning lease duration", "invalid");
    if (
      !Number.isSafeInteger(options.limit)
      || options.limit < 1
      || options.limit > MAX_PROVISIONING_CLAIM_LIMIT
    ) throw new SaasRegistryError("invalid provisioning claim limit", "invalid");
    const timestamp = checkedNow(this.now);
    const lockedUntilMs = timestamp + options.leaseDurationMs;
    if (!Number.isSafeInteger(lockedUntilMs)) {
      throw new SaasRegistryError("provisioning lease expiry overflow", "invalid");
    }
    const leaseToken = assertCanonicalUuid(randomUUID(), "generated provisioning lease token");
    const rows = this.sqlite.query(`
      UPDATE provisioning_jobs
      SET state = 'running',
          attempts = attempts + 1,
          locked_until_ms = ?,
          lease_token = ?,
          updated_at_ms = ?
      WHERE id IN (
        SELECT id
        FROM provisioning_jobs
        WHERE (state = 'pending' AND next_attempt_at_ms <= ?)
          OR (state = 'running' AND locked_until_ms <= ?)
        ORDER BY next_attempt_at_ms, id
        LIMIT ?
      )
      RETURNING *
    `).all(
      lockedUntilMs,
      leaseToken,
      timestamp,
      timestamp,
      timestamp,
      options.limit,
    ) as RawProvisioningJob[];
    return rows.map((raw) => {
      const job = mapProvisioningJob(raw);
      if (job.state !== "running" || job.lockedUntilMs === null || job.leaseToken === null) {
        return corrupt("claimed provisioning job has no durable lease");
      }
      return job as ClaimedProvisioningJob;
    });
  }

  markProvisioningJobSucceeded(
    id: string,
    leaseToken: string,
    assertionInput: ProvisioningAssertionInput | null = null,
  ): boolean {
    assertCanonicalUuid(id, "provisioning job id");
    assertCanonicalUuid(leaseToken, "provisioning lease token");
    const timestamp = checkedNow(this.now);
    const assignments = [
      "state = 'succeeded'",
      "locked_until_ms = NULL",
      "lease_token = NULL",
      "last_error = NULL",
      "succeeded_at_ms = ?",
      "failed_at_ms = NULL",
      "updated_at_ms = ?",
    ];
    const bindings: Array<string | number | null> = [timestamp, timestamp];
    if (assertionInput !== null) {
      const current = this.getProvisioningJob(id);
      const account = this.getAccount(current.accountId);
      const checked = assertAssertionInput(assertionInput, current.kind, account.routeKey);
      assignments.push(
        "assertion_purpose = ?",
        "assertion_route_key = ?",
        "assertion_device_fp = ?",
        "assertion_jti = ?",
        "assertion_issued_at_ms = ?",
        "assertion_expires_at_ms = ?",
      );
      bindings.push(
        checked.purpose,
        checked.routeKey,
        checked.deviceFingerprint,
        checked.jti,
        checked.issuedAtMs,
        checked.expiresAtMs,
      );
    }
    bindings.push(id, leaseToken);
    const result = this.sqlite.query(`
      UPDATE provisioning_jobs
      SET ${assignments.join(", ")}
      WHERE id = ? AND state = 'running' AND lease_token = ?
    `).run(...bindings);
    return result.changes === 1;
  }

  rescheduleProvisioningJob(
    id: string,
    leaseToken: string,
    nextAttemptAtMsRaw: number,
    lastErrorRaw: string,
  ): boolean {
    assertCanonicalUuid(id, "provisioning job id");
    assertCanonicalUuid(leaseToken, "provisioning lease token");
    const nextAttemptAtMs = assertSafeTimestamp(nextAttemptAtMsRaw, "nextAttemptAtMs");
    const lastError = assertRedactedError(lastErrorRaw);
    const timestamp = checkedNow(this.now);
    if (nextAttemptAtMs < timestamp) {
      throw new SaasRegistryError("nextAttemptAtMs cannot be in the past", "invalid");
    }
    const result = this.sqlite.query(`
      UPDATE provisioning_jobs
      SET state = 'pending',
          next_attempt_at_ms = ?,
          locked_until_ms = NULL,
          lease_token = NULL,
          last_error = ?,
          updated_at_ms = ?
      WHERE id = ? AND state = 'running' AND lease_token = ?
    `).run(nextAttemptAtMs, lastError, timestamp, id, leaseToken);
    return result.changes === 1;
  }

  markProvisioningJobFailed(id: string, leaseToken: string, lastErrorRaw: string): boolean {
    assertCanonicalUuid(id, "provisioning job id");
    assertCanonicalUuid(leaseToken, "provisioning lease token");
    const lastError = assertRedactedError(lastErrorRaw);
    const timestamp = checkedNow(this.now);
    const result = this.sqlite.query(`
      UPDATE provisioning_jobs
      SET state = 'failed',
          locked_until_ms = NULL,
          lease_token = NULL,
          last_error = ?,
          failed_at_ms = ?,
          succeeded_at_ms = NULL,
          updated_at_ms = ?
      WHERE id = ? AND state = 'running' AND lease_token = ?
    `).run(lastError, timestamp, timestamp, id, leaseToken);
    return result.changes === 1;
  }

  nextProvisioningJobWakeAtMs(): number | null {
    const row = this.sqlite.query(`
      SELECT MIN(
        CASE state
          WHEN 'pending' THEN next_attempt_at_ms
          WHEN 'running' THEN locked_until_ms
        END
      ) AS wake_at_ms
      FROM provisioning_jobs
      WHERE state IN ('pending', 'running')
    `).get() as { wake_at_ms: unknown } | null;
    const wakeAtMs = row?.wake_at_ms;
    if (wakeAtMs === null || wakeAtMs === undefined) return null;
    if (typeof wakeAtMs !== "number" || !Number.isSafeInteger(wakeAtMs) || wakeAtMs < 0) {
      return corrupt("provisioning job wake time is invalid");
    }
    return wakeAtMs;
  }
}

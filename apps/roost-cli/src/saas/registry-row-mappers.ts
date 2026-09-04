// Maps untrusted SQLite rows into validated SaaS registry records.
// Every registry query uses these mappers before returning durable state.
// Strict mapping surfaces database corruption instead of normalizing it away.
import { resolve } from "node:path";
import { normalizeAccountEmail } from "@roost/shared/native-credentials";
import {
  FEDERATED_IDENTITY_STATES,
  GOOGLE_IDENTITY_ISSUER,
  LINK_TICKET_REDEMPTION_STATES,
  PROVISIONING_JOB_STATES,
  type FederatedIdentityState,
  type LinkTicketRedemptionState,
  type RegistryAccount,
  type RegistryCoordinator,
  type RegistryFederatedIdentity,
  type RegistryGlobalLease,
  type RegistryLease,
  type RegistryLinkTicketRedemption,
  type RegistryProvisioningJob,
  type ProvisioningAssertionInput,
  type ProvisioningAssertionPurpose,
  type ProvisioningJobKind,
  type ProvisioningJobState,
} from "./registry-model.ts";
import type {
  RawAccount,
  RawCoordinator,
  RawFederatedIdentity,
  RawGlobalLease,
  RawLease,
  RawLinkTicketRedemption,
  RawProvisioningJob,
} from "./registry-row-types.ts";
import {
  MAX_ERROR_BYTES,
  ROUTE_KEY_RE,
  SAFE_LEASE_VALUE_RE,
  SaasRegistryError,
  assertAssertionInput,
  assertCanonicalGoogleIssuer,
  assertCanonicalUuid,
  assertGoogleIdentitySubject,
  assertImmutableImageDigest,
  assertNormalizedEmail,
  assertProvisioningKind,
  assertRedactedError,
  assertSha256Hex,
  checkedTimestamp,
  coordinatorContainerName,
  coordinatorDataDir,
  coordinatorHostname,
  corrupt,
  isAccountState,
  isCoordinatorState,
} from "./registry-validation.ts";

export function mapAccount(raw: RawAccount): RegistryAccount {
  assertCanonicalUuid(raw.id, "account id");
  const email = normalizeAccountEmail(raw.email_normalized);
  if (!email || email !== raw.email_normalized) {
    throw new SaasRegistryError("registry row has invalid normalized email", "corrupt");
  }
  if (typeof raw.route_key !== "string" || !ROUTE_KEY_RE.test(raw.route_key)) {
    throw new SaasRegistryError("registry row has invalid tenant route key", "corrupt");
  }
  if (!isAccountState(raw.state)) throw new SaasRegistryError("registry row has invalid account state", "corrupt");
  return {
    id: raw.id,
    emailNormalized: email,
    routeKey: raw.route_key,
    state: raw.state,
    createdAtMs: checkedTimestamp(raw.created_at_ms, "created_at_ms")!,
    activatedAtMs: checkedTimestamp(raw.activated_at_ms, "activated_at_ms"),
    disabledAtMs: checkedTimestamp(raw.disabled_at_ms, "disabled_at_ms"),
  };
}

export function mapCoordinator(raw: RawCoordinator, rootDir: string): RegistryCoordinator {
  assertCanonicalUuid(raw.id, "coordinator id");
  assertCanonicalUuid(raw.account_id, "coordinator account id");
  if (typeof raw.route_key !== "string" || !ROUTE_KEY_RE.test(raw.route_key)) {
    throw new SaasRegistryError("registry row has invalid tenant route key", "corrupt");
  }
  if (!Number.isSafeInteger(raw.ordinal) || raw.ordinal < 1) {
    throw new SaasRegistryError("registry row has invalid coordinator ordinal", "corrupt");
  }
  if (raw.hostname !== coordinatorHostname(raw.id)) {
    throw new SaasRegistryError("registry row has mismatched coordinator hostname", "corrupt");
  }
  if (raw.container_name !== coordinatorContainerName(raw.id)) {
    throw new SaasRegistryError("registry row has mismatched container name", "corrupt");
  }
  if (resolve(raw.data_dir) !== resolve(coordinatorDataDir(rootDir, raw.id))) {
    throw new SaasRegistryError("registry row has mismatched data directory", "corrupt");
  }
  try {
    assertImmutableImageDigest(raw.image_digest);
  } catch {
    throw new SaasRegistryError("registry row has invalid immutable image digest", "corrupt");
  }
  if (!isCoordinatorState(raw.state)) {
    throw new SaasRegistryError("registry row has invalid coordinator state", "corrupt");
  }
  if (raw.last_error !== null && Buffer.byteLength(raw.last_error, "utf8") > MAX_ERROR_BYTES) {
    throw new SaasRegistryError("registry row has oversized last error", "corrupt");
  }
  return {
    id: raw.id,
    accountId: raw.account_id,
    routeKey: raw.route_key,
    ordinal: raw.ordinal,
    hostname: raw.hostname,
    containerName: raw.container_name,
    dataDir: raw.data_dir,
    imageDigest: raw.image_digest,
    state: raw.state,
    createdAtMs: checkedTimestamp(raw.created_at_ms, "created_at_ms")!,
    seededAtMs: checkedTimestamp(raw.seeded_at_ms, "seeded_at_ms"),
    runningAtMs: checkedTimestamp(raw.running_at_ms, "running_at_ms"),
    routedAtMs: checkedTimestamp(raw.routed_at_ms, "routed_at_ms"),
    invitedAtMs: checkedTimestamp(raw.invited_at_ms, "invited_at_ms"),
    activatedAtMs: checkedTimestamp(raw.activated_at_ms, "activated_at_ms"),
    disabledAtMs: checkedTimestamp(raw.disabled_at_ms, "disabled_at_ms"),
    failedAtMs: checkedTimestamp(raw.failed_at_ms, "failed_at_ms"),
    updatedAtMs: checkedTimestamp(raw.updated_at_ms, "updated_at_ms")!,
    lastError: raw.last_error,
  };
}

export function mapLease(raw: RawLease): RegistryLease {
  assertCanonicalUuid(raw.coordinator_id, "lease coordinator id");
  if (!SAFE_LEASE_VALUE_RE.test(raw.operation) || !SAFE_LEASE_VALUE_RE.test(raw.owner)) {
    throw new SaasRegistryError("registry row has invalid lease identity", "corrupt");
  }
  const acquiredAtMs = checkedTimestamp(raw.acquired_at_ms, "lease acquired_at_ms")!;
  const expiresAtMs = checkedTimestamp(raw.expires_at_ms, "lease expires_at_ms")!;
  if (expiresAtMs <= acquiredAtMs) throw new SaasRegistryError("registry row has invalid lease expiry", "corrupt");
  return {
    coordinatorId: raw.coordinator_id,
    operation: raw.operation,
    owner: raw.owner,
    acquiredAtMs,
    expiresAtMs,
  };
}

export function mapGlobalLease(raw: RawGlobalLease): RegistryGlobalLease {
  if (!SAFE_LEASE_VALUE_RE.test(raw.resource)
    || !SAFE_LEASE_VALUE_RE.test(raw.operation)
    || !SAFE_LEASE_VALUE_RE.test(raw.owner)) {
    throw new SaasRegistryError("registry row has invalid global lease identity", "corrupt");
  }
  const acquiredAtMs = checkedTimestamp(raw.acquired_at_ms, "global lease acquired_at_ms")!;
  const expiresAtMs = checkedTimestamp(raw.expires_at_ms, "global lease expires_at_ms")!;
  if (expiresAtMs <= acquiredAtMs) throw new SaasRegistryError("registry row has invalid global lease expiry", "corrupt");
  return { resource: raw.resource, operation: raw.operation, owner: raw.owner, acquiredAtMs, expiresAtMs };
}

export function mapFederatedIdentity(raw: RawFederatedIdentity): RegistryFederatedIdentity {
  try {
    const issuer = assertCanonicalGoogleIssuer(raw.issuer);
    const subject = assertGoogleIdentitySubject(raw.subject);
    assertCanonicalUuid(raw.account_id, "federated identity account id");
    const emailNormalized = assertNormalizedEmail(raw.email_normalized);
    if (!(FEDERATED_IDENTITY_STATES as readonly string[]).includes(raw.state)) {
      return corrupt("registry row has invalid federated identity state");
    }
    const createdAtMs = checkedTimestamp(raw.created_at_ms, "identity created_at_ms")!;
    const updatedAtMs = checkedTimestamp(raw.updated_at_ms, "identity updated_at_ms")!;
    const verifiedAtMs = checkedTimestamp(raw.verified_at_ms, "identity verified_at_ms")!;
    if (updatedAtMs < createdAtMs || verifiedAtMs > updatedAtMs) {
      return corrupt("registry row has inconsistent federated identity timestamps");
    }
    return {
      issuer,
      subject,
      accountId: raw.account_id,
      emailNormalized,
      state: raw.state as FederatedIdentityState,
      createdAtMs,
      updatedAtMs,
      verifiedAtMs,
    };
  } catch (error) {
    if (error instanceof SaasRegistryError && error.code === "corrupt") throw error;
    return corrupt("registry row has invalid federated identity");
  }
}

function rawAssertionInput(
  raw: RawProvisioningJob,
  kind: ProvisioningJobKind,
): ProvisioningAssertionInput | null {
  const values = [
    raw.assertion_purpose,
    raw.assertion_route_key,
    raw.assertion_device_fp,
    raw.assertion_jti,
    raw.assertion_issued_at_ms,
    raw.assertion_expires_at_ms,
  ];
  if (values.every((value) => value === null)) return null;
  if (values.some((value) => value === null)) return corrupt("registry row has partial assertion input");
  try {
    return assertAssertionInput({
      purpose: raw.assertion_purpose as ProvisioningAssertionPurpose,
      routeKey: raw.assertion_route_key!,
      deviceFingerprint: raw.assertion_device_fp!,
      jti: raw.assertion_jti!,
      issuedAtMs: raw.assertion_issued_at_ms!,
      expiresAtMs: raw.assertion_expires_at_ms!,
    }, kind);
  } catch {
    return corrupt("registry row has invalid assertion input");
  }
}

export function mapProvisioningJob(raw: RawProvisioningJob): RegistryProvisioningJob {
  try {
    assertCanonicalUuid(raw.id, "provisioning job id");
    assertSha256Hex(raw.idempotency_key_hash, "idempotency key hash");
    const kind = assertProvisioningKind(raw.kind);
    const emailNormalized = assertNormalizedEmail(raw.email_normalized);
    assertCanonicalUuid(raw.account_id, "provisioning account id");
    assertCanonicalUuid(raw.coordinator_id, "provisioning coordinator id");
    const verifiedAtMs = checkedTimestamp(raw.verified_at_ms, "job verified_at_ms")!;
    let identityIssuer: typeof GOOGLE_IDENTITY_ISSUER | null = null;
    let identitySubject: string | null = null;
    let activationTokenHash: string | null = null;
    if (kind === "verified-email") {
      if (raw.identity_issuer !== null || raw.identity_subject !== null || raw.activation_token_hash === null) {
        return corrupt("registry row has inconsistent verified-email claims");
      }
      activationTokenHash = assertSha256Hex(raw.activation_token_hash, "activation token hash");
    } else {
      if (raw.identity_issuer === null || raw.identity_subject === null || raw.activation_token_hash !== null) {
        return corrupt("registry row has inconsistent Google claims");
      }
      identityIssuer = assertCanonicalGoogleIssuer(raw.identity_issuer);
      identitySubject = assertGoogleIdentitySubject(raw.identity_subject);
    }
    if (!(PROVISIONING_JOB_STATES as readonly string[]).includes(raw.state)) {
      return corrupt("registry row has invalid provisioning job state");
    }
    if (!Number.isSafeInteger(raw.attempts) || raw.attempts < 0) {
      return corrupt("registry row has invalid provisioning attempts");
    }
    const nextAttemptAtMs = checkedTimestamp(raw.next_attempt_at_ms, "job next_attempt_at_ms")!;
    const lockedUntilMs = checkedTimestamp(raw.locked_until_ms, "job locked_until_ms");
    if (raw.state === "running") {
      if (lockedUntilMs === null || raw.lease_token === null) {
        return corrupt("registry running job has no lease");
      }
      assertCanonicalUuid(raw.lease_token, "provisioning lease token");
    } else if (lockedUntilMs !== null || raw.lease_token !== null) {
      return corrupt("registry non-running job has a lease");
    }
    if (raw.last_error !== null) assertRedactedError(raw.last_error);
    const createdAtMs = checkedTimestamp(raw.created_at_ms, "job created_at_ms")!;
    const updatedAtMs = checkedTimestamp(raw.updated_at_ms, "job updated_at_ms")!;
    const succeededAtMs = checkedTimestamp(raw.succeeded_at_ms, "job succeeded_at_ms");
    const failedAtMs = checkedTimestamp(raw.failed_at_ms, "job failed_at_ms");
    if (
      updatedAtMs < createdAtMs
      || verifiedAtMs > createdAtMs
      || (raw.state === "succeeded") !== (succeededAtMs !== null)
      || (raw.state === "failed") !== (failedAtMs !== null)
    ) return corrupt("registry row has inconsistent provisioning timestamps");
    return {
      id: raw.id,
      idempotencyKeyHash: raw.idempotency_key_hash,
      kind,
      emailNormalized,
      identityIssuer,
      identitySubject,
      activationTokenHash,
      verifiedAtMs,
      accountId: raw.account_id,
      coordinatorId: raw.coordinator_id,
      state: raw.state as ProvisioningJobState,
      attempts: raw.attempts,
      nextAttemptAtMs,
      lockedUntilMs,
      leaseToken: raw.lease_token,
      lastError: raw.last_error,
      assertionInput: rawAssertionInput(raw, kind),
      createdAtMs,
      updatedAtMs,
      succeededAtMs,
      failedAtMs,
    };
  } catch (error) {
    if (error instanceof SaasRegistryError && error.code === "corrupt") throw error;
    return corrupt("registry row has invalid provisioning job");
  }
}

export function mapLinkTicketRedemption(raw: RawLinkTicketRedemption): RegistryLinkTicketRedemption {
  try {
    const ticketJti = assertCanonicalUuid(raw.ticket_jti, "link ticket jti");
    assertCanonicalUuid(raw.account_id, "link ticket account id");
    assertCanonicalUuid(raw.coordinator_id, "link ticket coordinator id");
    const deviceFingerprint = assertSha256Hex(raw.device_fp, "device fingerprint");
    const identityIssuer = assertCanonicalGoogleIssuer(raw.identity_issuer);
    const identitySubject = assertGoogleIdentitySubject(raw.identity_subject);
    if (!(LINK_TICKET_REDEMPTION_STATES as readonly string[]).includes(raw.state)) {
      return corrupt("registry row has invalid link ticket state");
    }
    return {
      ticketJti,
      accountId: raw.account_id,
      coordinatorId: raw.coordinator_id,
      deviceFingerprint,
      identityIssuer,
      identitySubject,
      state: raw.state as LinkTicketRedemptionState,
      expiresAtMs: checkedTimestamp(raw.expires_at_ms, "link ticket expires_at_ms")!,
    };
  } catch (error) {
    if (error instanceof SaasRegistryError && error.code === "corrupt") throw error;
    return corrupt("registry row has invalid link ticket redemption");
  }
}

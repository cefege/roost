// Accepts verified provisioning submissions and projects durable job status.
// The private IPC operation calls this class while its base owns job execution.
// Reservations and identity bindings are committed before asynchronous work begins.
import {
  HostCapacityError,
} from "./host.ts";
import {
  SaasRegistryError,
  assertCanonicalGoogleIssuer,
  assertCanonicalUuid,
  assertGoogleIdentitySubject,
  assertSha256Hex,
  type AccountReservation,
  type ProvisioningAssertionInput,
  type RegistryProvisioningJob,
} from "./registry.ts";
import {
  MAX_EMAIL_PROOF_LIFETIME_MS,
  MAX_GOOGLE_PROOF_LIFETIME_MS,
  assertFreshProof,
  checkedEmail,
  checkedTimestamp,
  idempotencyHash,
  ordinalOneReservation,
  type CanonicalAssertionInputs,
  type GoogleLinkSubmission,
  type GoogleSubmission,
  type ProvisioningStatus,
  type ProvisioningSubmission,
  type ProvisioningSubmitResult,
  type VerifiedEmailSubmission,
} from "./provisioning-contract.ts";
import { ProvisioningJobLoop } from "./provisioning-job-loop.ts";

export class ProvisioningWorker extends ProvisioningJobLoop {
  private existingSubmission(hash: string): ProvisioningSubmitResult | null {
    const existing = this.registry.getProvisioningJobByIdempotencyKeyHash(hash);
    if (!existing) return null;
    if (existing.state === "failed") return { state: "failed" };
    return { state: "pending", jobId: existing.id, retryAfterMs: this.retryDelayMs };
  }

  private inserted(job: RegistryProvisioningJob): ProvisioningSubmitResult {
    this.kick();
    return { state: "pending", jobId: job.id, retryAfterMs: this.retryDelayMs };
  }

  async submit(input: ProvisioningSubmission): Promise<ProvisioningSubmitResult> {
    if (input.kind === "verified-email") return this.submitVerifiedEmail(input);
    if (input.kind === "google-link") return this.submitGoogleLink(input);
    return this.submitGoogle(input);
  }

  private async submitVerifiedEmail(input: VerifiedEmailSubmission): Promise<ProvisioningSubmitResult> {
    const timestamp = checkedTimestamp(this.now(), "current time");
    const emailNormalized = checkedEmail(input.emailNormalized);
    const activationTokenHash = assertSha256Hex(input.activationTokenHash, "activation token hash");
    assertCanonicalUuid(input.challengeId, "challenge id");
    const { verifiedAtMs } = assertFreshProof(
      timestamp,
      input.verifiedAtMs,
      input.expiresAtMs,
      MAX_EMAIL_PROOF_LIFETIME_MS,
    );
    const hash = idempotencyHash(input.idempotencyKey);
    const existing = this.existingSubmission(hash);
    if (existing) return existing;

    let reservation: AccountReservation;
    try {
      reservation = await this.admission.assertBeforeReservation(
        () => this.registry.reserveAccount(emailNormalized, this.imageDigest),
      );
    } catch (error) {
      if (error instanceof HostCapacityError) return { state: "capacity" };
      if (error instanceof SaasRegistryError && error.code === "conflict") {
        return { state: "proof-required" };
      }
      throw error;
    }
    const insertion = this.registry.insertProvisioningJob({
      idempotencyKeyHash: hash,
      kind: "verified-email",
      emailNormalized,
      activationTokenHash,
      verifiedAtMs,
      accountId: reservation.account.id,
      coordinatorId: reservation.coordinator.id,
    });
    return this.inserted(insertion.job);
  }

  private async submitGoogle(input: GoogleSubmission): Promise<ProvisioningSubmitResult> {
    const timestamp = checkedTimestamp(this.now(), "current time");
    const issuer = assertCanonicalGoogleIssuer(input.issuer);
    const subject = assertGoogleIdentitySubject(input.subject);
    const emailNormalized = checkedEmail(input.emailNormalized);
    const { verifiedAtMs } = assertFreshProof(
      timestamp,
      input.verifiedAtMs,
      input.expiresAtMs,
      MAX_GOOGLE_PROOF_LIFETIME_MS,
    );
    const hash = idempotencyHash(input.idempotencyKey);
    const existingJob = this.existingSubmission(hash);
    if (existingJob) return existingJob;

    const mapped = this.registry.getFederatedIdentity(issuer, subject);
    let reservation: AccountReservation;
    let jobKind: "google-signup" | "google-login" = input.kind;
    if (input.kind === "google-login") {
      if (!mapped || mapped.state !== "active") return { state: "failed" };
      const coordinator = this.registry.listCoordinators()
        .find((row) => row.accountId === mapped.accountId && row.ordinal === 1);
      if (!coordinator) return { state: "failed" };
      reservation = ordinalOneReservation(this.registry, mapped.accountId, coordinator.id, true);
      if (reservation.account.state !== "active" || reservation.coordinator.state !== "active") {
        return { state: "failed" };
      }
    } else if (mapped) {
      if (mapped.state === "revoked") return { state: "proof-required" };
      const coordinator = this.registry.listCoordinators()
        .find((row) => row.accountId === mapped.accountId && row.ordinal === 1);
      if (!coordinator) throw new Error("federated account has no ordinal-one coordinator");
      reservation = ordinalOneReservation(this.registry, mapped.accountId, coordinator.id, true);
      if (mapped.state === "active" && reservation.account.state === "active"
        && reservation.coordinator.state === "active") {
        jobKind = "google-login";
      } else if (mapped.state !== "reserved" || reservation.account.state !== "pending") {
        return { state: "proof-required" };
      }
    } else {
      try {
        const reserved = await this.admission.assertBeforeReservation(
          () => this.registry.reserveGoogleSignup({
            issuer,
            subject,
            emailNormalized,
            verifiedAtMs,
            imageDigest: this.imageDigest,
          }),
        );
        if (reserved.outcome === "proof-required") return { state: "proof-required" };
        reservation = {
          account: reserved.account,
          coordinator: reserved.coordinator,
          resumed: reserved.resumed,
        };
        if (reserved.outcome === "existing" && reserved.identity.state === "active"
          && reserved.account.state === "active" && reserved.coordinator.state === "active") {
          jobKind = "google-login";
        }
      } catch (error) {
        if (error instanceof HostCapacityError) return { state: "capacity" };
        throw error;
      }
    }

    const insertion = this.registry.insertProvisioningJob({
      idempotencyKeyHash: hash,
      kind: jobKind,
      emailNormalized: mapped?.emailNormalized ?? emailNormalized,
      identityIssuer: issuer,
      identitySubject: subject,
      verifiedAtMs,
      accountId: reservation.account.id,
      coordinatorId: reservation.coordinator.id,
    });
    return this.inserted(insertion.job);
  }

  private async submitGoogleLink(input: GoogleLinkSubmission): Promise<ProvisioningSubmitResult> {
    const timestamp = checkedTimestamp(this.now(), "current time");
    const issuer = assertCanonicalGoogleIssuer(input.issuer);
    const subject = assertGoogleIdentitySubject(input.subject);
    const emailNormalized = checkedEmail(input.emailNormalized);
    const { verifiedAtMs } = assertFreshProof(
      timestamp,
      input.verifiedAtMs,
      input.expiresAtMs,
      MAX_GOOGLE_PROOF_LIFETIME_MS,
    );
    const hash = idempotencyHash(input.idempotencyKey);
    const existing = this.existingSubmission(hash);
    if (existing) return existing;

    const ticket = input.ticket;
    assertCanonicalUuid(ticket.ticketJti, "link ticket jti");
    assertCanonicalUuid(ticket.accountId, "link ticket account id");
    assertCanonicalUuid(ticket.coordinatorId, "link ticket coordinator id");
    const issuedAtMs = checkedTimestamp(ticket.issuedAtMs, "link ticket issuedAtMs");
    const expiresAtMs = checkedTimestamp(ticket.expiresAtMs, "link ticket expiresAtMs");
    if (issuedAtMs > timestamp || timestamp >= expiresAtMs
      || expiresAtMs - issuedAtMs > 5 * 60_000) throw new Error("link ticket is stale");
    const account = this.registry.getAccount(ticket.accountId);
    const coordinator = this.registry.getCoordinator(ticket.coordinatorId);
    if (account.state !== "active" || coordinator.state !== "active"
      || coordinator.accountId !== account.id || coordinator.routeKey !== account.routeKey
      || ticket.routeKey !== account.routeKey || account.emailNormalized !== emailNormalized) {
      return { state: "failed" };
    }
    const assertionInput: ProvisioningAssertionInput = {
      purpose: "link",
      routeKey: account.routeKey,
      deviceFingerprint: ticket.deviceFingerprint,
      jti: ticket.ticketJti,
      issuedAtMs,
      expiresAtMs,
    };
    this.registry.reserveLinkTicketRedemption({
      ticketJti: ticket.ticketJti,
      accountId: account.id,
      coordinatorId: coordinator.id,
      deviceFingerprint: ticket.deviceFingerprint,
      identityIssuer: issuer,
      identitySubject: subject,
      emailNormalized,
      verifiedAtMs,
      expiresAtMs,
    });
    const insertion = this.registry.insertProvisioningJob({
      idempotencyKeyHash: hash,
      kind: "google-link",
      emailNormalized,
      identityIssuer: issuer,
      identitySubject: subject,
      verifiedAtMs,
      accountId: account.id,
      coordinatorId: coordinator.id,
      assertionInput,
    });
    return this.inserted(insertion.job);
  }

  private assertionInputs(job: RegistryProvisioningJob): CanonicalAssertionInputs {
    if (!job.identityIssuer || !job.identitySubject) {
      throw new Error("Google job has no canonical identity claims");
    }
    const account = this.registry.getAccount(job.accountId);
    const base: CanonicalAssertionInputs = {
      purpose: job.kind === "google-link" ? "link" : "continue",
      accountId: account.id,
      coordinatorId: job.coordinatorId,
      routeKey: account.routeKey,
      identityIssuer: job.identityIssuer,
      identitySubject: job.identitySubject,
      emailNormalized: job.emailNormalized,
    };
    if (job.assertionInput) {
      base.deviceFingerprint = job.assertionInput.deviceFingerprint;
      base.jti = job.assertionInput.jti;
      base.issuedAtMs = job.assertionInput.issuedAtMs;
      base.expiresAtMs = job.assertionInput.expiresAtMs;
    }
    return base;
  }

  status(jobIdRaw: string): ProvisioningStatus {
    const job = this.registry.getProvisioningJob(assertCanonicalUuid(jobIdRaw, "provisioning job id"));
    if (job.state === "failed") return { state: "failed" };
    if (job.state !== "succeeded") {
      this.kick();
      return { state: "pending", retryAfterMs: this.retryDelayMs };
    }
    const routeKey = this.registry.getAccount(job.accountId).routeKey;
    if (job.kind === "verified-email") return { state: "ready", routeKey };
    const assertionInputs = this.assertionInputs(job);
    return {
      state: job.kind === "google-link" ? "ready" : "awaiting-device",
      routeKey,
      assertionInputs,
    };
  }

  async finalizeLink(jobIdRaw: string): Promise<ProvisioningStatus> {
    const job = this.registry.getProvisioningJob(assertCanonicalUuid(jobIdRaw, "provisioning job id"));
    if (job.kind !== "google-link" || job.state !== "succeeded" || !job.assertionInput
      || !job.identityIssuer || !job.identitySubject) return { state: "failed" };
    await this.lifecycle.proveActive(job.accountId, job.coordinatorId, ["active-linked"]);
    this.registry.consumeLinkTicketRedemption({
      ticketJti: job.assertionInput.jti,
      accountId: job.accountId,
      coordinatorId: job.coordinatorId,
      deviceFingerprint: job.assertionInput.deviceFingerprint,
      identityIssuer: job.identityIssuer,
      identitySubject: job.identitySubject,
    });
    return this.status(job.id);
  }

}

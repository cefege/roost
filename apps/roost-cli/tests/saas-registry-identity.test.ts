// This suite protects federated identity ownership, provisioning job leases, and link-ticket redemption.
// These onboarding workflows share one deterministic registry fixture but retain isolated cleanup ownership.
// They are split from the core registry suite so both files remain small without changing test behavior.

import { afterEach, describe, expect, test } from "bun:test";
import { Database } from "bun:sqlite";
import {
  GOOGLE_IDENTITY_ISSUER,
  SaasRegistry,
  SaasRegistryError,
} from "../src/saas/registry.ts";
import {
  ACCOUNT_ID,
  DEVICE_FP,
  GOOGLE_SUBJECT,
  IMAGE,
  JOB_ID,
  SECOND_TICKET_JTI,
  TICKET_JTI,
  WRONG_LEASE_TOKEN,
  createSaasRegistryFixtureScope,
} from "./saas-registry-fixtures.ts";

const { cleanup, fixture } = createSaasRegistryFixtureScope();
afterEach(cleanup);

describe("SaaS registry", () => {
  test("reserves Google subjects before considering email and never auto-links a new subject", () => {
    const opened = fixture();
    const second = new SaasRegistry({
      rootDir: opened.root,
      path: opened.path,
      now: () => opened.nowRef.value,
    });
    try {
      const first = opened.registry.reserveGoogleSignup({
        issuer: GOOGLE_IDENTITY_ISSUER,
        subject: GOOGLE_SUBJECT,
        emailNormalized: "owner@example.com",
        verifiedAtMs: 1_000,
        imageDigest: IMAGE,
      });
      if (first.outcome === "proof-required") throw new Error("expected a Google reservation");
      expect(first.outcome).toBe("reserved");
      expect(first.resumed).toBe(false);

      const retried = second.reserveGoogleSignup({
        issuer: GOOGLE_IDENTITY_ISSUER,
        subject: GOOGLE_SUBJECT,
        emailNormalized: "renamed@example.com",
        verifiedAtMs: 1_000,
        imageDigest: `sha256:${"b".repeat(64)}`,
      });
      if (retried.outcome === "proof-required") throw new Error("expected subject convergence");
      expect(retried.outcome).toBe("existing");
      expect(retried.account.id).toBe(first.account.id);
      expect(retried.coordinator.id).toBe(first.coordinator.id);
      expect(retried.account.routeKey).toBe(first.account.routeKey);
      expect(retried.identity.emailNormalized).toBe("renamed@example.com");
      expect(opened.registry.listAccounts()).toHaveLength(1);

      expect(second.reserveGoogleSignup({
        issuer: GOOGLE_IDENTITY_ISSUER,
        subject: "a-new-google-subject",
        emailNormalized: "owner@example.com",
        verifiedAtMs: 1_000,
        imageDigest: IMAGE,
      })).toEqual({ outcome: "proof-required" });
      expect(second.getFederatedIdentity(GOOGLE_IDENTITY_ISSUER, "a-new-google-subject")).toBeNull();
    } finally {
      second.close();
      opened.registry.close();
    }
  });

  test("keeps revoked Google subjects as permanent owner-bound tombstones", () => {
    const opened = fixture();
    try {
      const reserved = opened.registry.reserveGoogleSignup({
        issuer: GOOGLE_IDENTITY_ISSUER,
        subject: GOOGLE_SUBJECT,
        emailNormalized: "owner@example.com",
        verifiedAtMs: 1_000,
        imageDigest: IMAGE,
      });
      if (reserved.outcome === "proof-required") throw new Error("expected a Google reservation");
      opened.registry.activateFederatedIdentity(GOOGLE_IDENTITY_ISSUER, GOOGLE_SUBJECT, ACCOUNT_ID);
      expect(opened.registry.revokeFederatedIdentity(
        GOOGLE_IDENTITY_ISSUER,
        GOOGLE_SUBJECT,
        ACCOUNT_ID,
      ).state).toBe("revoked");
      expect(() => opened.registry.activateFederatedIdentity(
        GOOGLE_IDENTITY_ISSUER,
        GOOGLE_SUBJECT,
        ACCOUNT_ID,
      )).toThrow("permanently revoked");
      const replay = opened.registry.reserveGoogleSignup({
        issuer: GOOGLE_IDENTITY_ISSUER,
        subject: GOOGLE_SUBJECT,
        emailNormalized: "owner@example.com",
        verifiedAtMs: 1_000,
        imageDigest: IMAGE,
      });
      if (replay.outcome === "proof-required") throw new Error("tombstoned subject must retain ownership");
      expect(replay.identity.state).toBe("revoked");
      expect(replay.account.id).toBe(ACCOUNT_ID);

      const sqlite = new Database(opened.path);
      try {
        expect(() => sqlite.query(`
          DELETE FROM federated_identities WHERE issuer = ? AND subject = ?
        `).run(GOOGLE_IDENTITY_ISSUER, GOOGLE_SUBJECT)).toThrow("cannot be deleted");
      } finally {
        sqlite.close(true);
      }
    } finally {
      opened.registry.close();
    }
  });

  test("claims and reclaims jobs atomically and CASes every lease mutation", () => {
    const opened = fixture();
    const competing = new SaasRegistry({
      rootDir: opened.root,
      path: opened.path,
      now: () => opened.nowRef.value,
    });
    try {
      const reservation = opened.registry.reserveGoogleSignup({
        issuer: GOOGLE_IDENTITY_ISSUER,
        subject: GOOGLE_SUBJECT,
        emailNormalized: "owner@example.com",
        verifiedAtMs: 1_000,
        imageDigest: IMAGE,
      });
      if (reservation.outcome === "proof-required") throw new Error("expected a Google reservation");
      const input = {
        idempotencyKeyHash: "1".repeat(64),
        kind: "google-signup" as const,
        emailNormalized: "owner@example.com",
        identityIssuer: GOOGLE_IDENTITY_ISSUER,
        identitySubject: GOOGLE_SUBJECT,
        verifiedAtMs: 1_000,
        accountId: reservation.account.id,
        coordinatorId: reservation.coordinator.id,
      };
      const inserted = opened.registry.insertProvisioningJob(input);
      expect(inserted.inserted).toBe(true);
      expect(opened.registry.insertProvisioningJob(input)).toEqual({
        job: inserted.job,
        inserted: false,
      });
      expect(() => opened.registry.insertProvisioningJob({
        ...input,
        emailNormalized: "other@example.com",
      })).toThrow(SaasRegistryError);

      const [firstLease] = opened.registry.claimDueProvisioningJobs({
        leaseDurationMs: 100,
        limit: 1,
      });
      if (!firstLease) throw new Error("expected first provisioning lease");
      expect(firstLease).toEqual(expect.objectContaining({ id: JOB_ID, attempts: 1, state: "running" }));
      expect(competing.claimDueProvisioningJobs({ leaseDurationMs: 100, limit: 1 })).toEqual([]);
      opened.nowRef.value = 1_100;
      const [reclaimed] = competing.claimDueProvisioningJobs({ leaseDurationMs: 100, limit: 1 });
      if (!reclaimed) throw new Error("expected expired provisioning lease recovery");
      expect(reclaimed.attempts).toBe(2);
      expect(reclaimed.leaseToken).not.toBe(firstLease.leaseToken);
      expect(opened.registry.rescheduleProvisioningJob(
        JOB_ID,
        firstLease.leaseToken,
        1_200,
        "stale worker",
      )).toBe(false);
      expect(competing.rescheduleProvisioningJob(
        JOB_ID,
        reclaimed.leaseToken,
        1_200,
        "provider unavailable",
      )).toBe(true);
      opened.nowRef.value = 1_200;
      const [thirdLease] = opened.registry.claimDueProvisioningJobs({
        leaseDurationMs: 100,
        limit: 1,
      });
      if (!thirdLease) throw new Error("expected rescheduled provisioning lease");
      expect(opened.registry.markProvisioningJobSucceeded(JOB_ID, WRONG_LEASE_TOKEN)).toBe(false);
      expect(opened.registry.markProvisioningJobSucceeded(JOB_ID, thirdLease.leaseToken)).toBe(true);
      expect(opened.registry.getProvisioningJob(JOB_ID)).toEqual(expect.objectContaining({
        state: "succeeded",
        attempts: 3,
        leaseToken: null,
      }));
      expect(opened.registry.insertProvisioningJob(input).job.id).toBe(JOB_ID);

      const failedJob = opened.registry.insertProvisioningJob({
        ...input,
        idempotencyKeyHash: "2".repeat(64),
      }).job;
      const [failureLease] = opened.registry.claimDueProvisioningJobs({
        leaseDurationMs: 100,
        limit: 1,
      });
      if (!failureLease) throw new Error("expected failure provisioning lease");
      expect(failureLease.id).toBe(failedJob.id);
      expect(opened.registry.markProvisioningJobFailed(
        failedJob.id,
        WRONG_LEASE_TOKEN,
        "wrong owner",
      )).toBe(false);
      expect(opened.registry.markProvisioningJobFailed(
        failedJob.id,
        failureLease.leaseToken,
        "retry exhausted",
      )).toBe(true);
    } finally {
      competing.close();
      opened.registry.close();
    }
  });

  test("reserves and consumes exact link-ticket identity bindings idempotently", () => {
    const opened = fixture();
    try {
      const account = opened.registry.reserveAccount("owner@example.com", IMAGE);
      const input = {
        ticketJti: TICKET_JTI,
        accountId: account.account.id,
        coordinatorId: account.coordinator.id,
        deviceFingerprint: DEVICE_FP,
        identityIssuer: GOOGLE_IDENTITY_ISSUER,
        identitySubject: GOOGLE_SUBJECT,
        emailNormalized: "owner@example.com",
        verifiedAtMs: 1_000,
        expiresAtMs: 301_000,
      };
      const first = opened.registry.reserveLinkTicketRedemption(input);
      expect(first.resumed).toBe(false);
      expect(first.identity.state).toBe("reserved");
      expect(opened.registry.reserveLinkTicketRedemption(input).resumed).toBe(true);
      expect(() => opened.registry.reserveLinkTicketRedemption({
        ...input,
        deviceFingerprint: "e".repeat(64),
      })).toThrow("different identity binding");
      expect(() => opened.registry.reserveLinkTicketRedemption({
        ...input,
        ticketJti: SECOND_TICKET_JTI,
        identitySubject: "another-google-subject",
      })).toThrow("already has a Google identity");

      const consumed = opened.registry.consumeLinkTicketRedemption({
        ticketJti: input.ticketJti,
        accountId: input.accountId,
        coordinatorId: input.coordinatorId,
        deviceFingerprint: input.deviceFingerprint,
        identityIssuer: input.identityIssuer,
        identitySubject: input.identitySubject,
      });
      expect(consumed.redemption.state).toBe("consumed");
      expect(consumed.identity.state).toBe("active");
      expect(consumed.resumed).toBe(false);
      expect(opened.registry.consumeLinkTicketRedemption({
        ticketJti: input.ticketJti,
        accountId: input.accountId,
        coordinatorId: input.coordinatorId,
        deviceFingerprint: input.deviceFingerprint,
        identityIssuer: input.identityIssuer,
        identitySubject: input.identitySubject,
      }).resumed).toBe(true);
    } finally {
      opened.registry.close();
    }
  });
});

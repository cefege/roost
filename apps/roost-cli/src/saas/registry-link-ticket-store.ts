// Owns reservation and consumption of single-use identity-link tickets.
// Provisioning uses these transactions to bind tickets to accounts and identities.
// Durable redemption rows make retries idempotent without weakening expiry checks.
import { RegistryProvisioningJobStore } from "./registry-provisioning-job-store.ts";
import { immediate } from "./registry-schema.ts";
import type {
  ConsumeLinkTicketRedemptionOptions,
  LinkTicketReservation,
  RegistryLinkTicketRedemption,
  ReserveLinkTicketRedemptionOptions,
} from "./registry-model.ts";
import type { RawLinkTicketRedemption } from "./registry-row-types.ts";
import { mapLinkTicketRedemption } from "./registry-row-mappers.ts";
import {
  MAX_LINK_TICKET_LIFETIME_MS,
  SaasRegistryError,
  assertCanonicalGoogleIssuer,
  assertCanonicalUuid,
  assertGoogleIdentitySubject,
  assertNormalizedEmail,
  assertSafeTimestamp,
  assertSha256Hex,
  checkedNow,
  corrupt,
} from "./registry-validation.ts";

export class RegistryLinkTicketStore extends RegistryProvisioningJobStore {
  reserveLinkTicketRedemption(options: ReserveLinkTicketRedemptionOptions): LinkTicketReservation {
    const ticketJti = assertCanonicalUuid(options.ticketJti, "link ticket jti");
    const accountId = assertCanonicalUuid(options.accountId, "link ticket account id");
    const coordinatorId = assertCanonicalUuid(options.coordinatorId, "link ticket coordinator id");
    const deviceFingerprint = assertSha256Hex(options.deviceFingerprint, "device fingerprint");
    const identityIssuer = assertCanonicalGoogleIssuer(options.identityIssuer);
    const identitySubject = assertGoogleIdentitySubject(options.identitySubject);
    const emailNormalized = assertNormalizedEmail(options.emailNormalized);
    const verifiedAtMs = assertSafeTimestamp(options.verifiedAtMs, "verifiedAtMs");
    const expiresAtMs = assertSafeTimestamp(options.expiresAtMs, "expiresAtMs");
    return immediate(this.sqlite, () => {
      const timestamp = checkedNow(this.now);
      if (verifiedAtMs > timestamp) {
        throw new SaasRegistryError("verifiedAtMs cannot be in the future", "invalid");
      }
      if (
        expiresAtMs <= timestamp
        || expiresAtMs - timestamp > MAX_LINK_TICKET_LIFETIME_MS
      ) throw new SaasRegistryError("link ticket expiry is invalid", "invalid");
      const account = this.getAccount(accountId);
      const coordinator = this.getCoordinator(coordinatorId);
      if (coordinator.accountId !== accountId) {
        throw new SaasRegistryError("link ticket coordinator belongs to another account", "conflict");
      }
      if (account.emailNormalized !== emailNormalized) {
        throw new SaasRegistryError("link ticket Google email does not match account", "conflict");
      }
      const existingRedemption = this.getLinkTicketRedemption(ticketJti);
      if (existingRedemption) {
        if (
          existingRedemption.accountId !== accountId
          || existingRedemption.coordinatorId !== coordinatorId
          || existingRedemption.deviceFingerprint !== deviceFingerprint
          || existingRedemption.identityIssuer !== identityIssuer
          || existingRedemption.identitySubject !== identitySubject
          || existingRedemption.expiresAtMs !== expiresAtMs
        ) {
          throw new SaasRegistryError("link ticket was reused with different identity binding", "conflict");
        }
        const identity = this.getFederatedIdentity(identityIssuer, identitySubject);
        if (!identity || identity.accountId !== accountId || identity.state === "revoked") {
          throw new SaasRegistryError("reserved link identity is unavailable", "conflict");
        }
        return { redemption: existingRedemption, identity, resumed: true };
      }

      let identity = this.getFederatedIdentity(identityIssuer, identitySubject);
      if (identity) {
        if (identity.accountId !== accountId || identity.state === "revoked") {
          throw new SaasRegistryError("Google identity is unavailable", "conflict");
        }
        this.sqlite.query(`
          UPDATE federated_identities
          SET email_normalized = ?, verified_at_ms = ?, updated_at_ms = ?
          WHERE issuer = ? AND subject = ?
        `).run(emailNormalized, verifiedAtMs, timestamp, identityIssuer, identitySubject);
      } else {
        const live = this.sqlite.query(`
          SELECT 1 FROM federated_identities
          WHERE account_id = ? AND issuer = ? AND state IN ('reserved','active')
        `).get(accountId, identityIssuer);
        if (live) throw new SaasRegistryError("account already has a Google identity", "conflict");
        this.sqlite.query(`
          INSERT INTO federated_identities (
            issuer, subject, account_id, email_normalized, state,
            created_at_ms, updated_at_ms, verified_at_ms
          ) VALUES (?, ?, ?, ?, 'reserved', ?, ?, ?)
        `).run(
          identityIssuer,
          identitySubject,
          accountId,
          emailNormalized,
          timestamp,
          timestamp,
          verifiedAtMs,
        );
      }
      identity = this.getFederatedIdentity(identityIssuer, identitySubject);
      if (!identity) return corrupt("link identity reservation was not persisted");
      this.sqlite.query(`
        INSERT INTO link_ticket_redemptions (
          ticket_jti, account_id, coordinator_id, device_fp,
          identity_issuer, identity_subject, state, expires_at_ms
        ) VALUES (?, ?, ?, ?, ?, ?, 'reserved', ?)
      `).run(
        ticketJti,
        accountId,
        coordinatorId,
        deviceFingerprint,
        identityIssuer,
        identitySubject,
        expiresAtMs,
      );
      return {
        redemption: this.getLinkTicketRedemption(ticketJti)!,
        identity,
        resumed: false,
      };
    });
  }

  getLinkTicketRedemption(ticketJtiRaw: string): RegistryLinkTicketRedemption | null {
    const ticketJti = assertCanonicalUuid(ticketJtiRaw, "link ticket jti");
    const raw = this.sqlite.query(`
      SELECT * FROM link_ticket_redemptions WHERE ticket_jti = ?
    `).get(ticketJti) as RawLinkTicketRedemption | null;
    return raw ? mapLinkTicketRedemption(raw) : null;
  }

  consumeLinkTicketRedemption(options: ConsumeLinkTicketRedemptionOptions): LinkTicketReservation {
    const ticketJti = assertCanonicalUuid(options.ticketJti, "link ticket jti");
    const accountId = assertCanonicalUuid(options.accountId, "link ticket account id");
    const coordinatorId = assertCanonicalUuid(options.coordinatorId, "link ticket coordinator id");
    const deviceFingerprint = assertSha256Hex(options.deviceFingerprint, "device fingerprint");
    const identityIssuer = assertCanonicalGoogleIssuer(options.identityIssuer);
    const identitySubject = assertGoogleIdentitySubject(options.identitySubject);
    return immediate(this.sqlite, () => {
      const redemption = this.getLinkTicketRedemption(ticketJti);
      if (!redemption) throw new SaasRegistryError("link ticket redemption not found", "not-found");
      if (
        redemption.accountId !== accountId
        || redemption.coordinatorId !== coordinatorId
        || redemption.deviceFingerprint !== deviceFingerprint
        || redemption.identityIssuer !== identityIssuer
        || redemption.identitySubject !== identitySubject
      ) throw new SaasRegistryError("link ticket redemption binding mismatch", "conflict");
      const identity = this.getFederatedIdentity(identityIssuer, identitySubject);
      if (!identity || identity.accountId !== accountId) {
        throw new SaasRegistryError("link identity reservation is inconsistent", "corrupt");
      }
      if (redemption.state === "consumed") {
        if (identity.state !== "active") {
          throw new SaasRegistryError("consumed link ticket has no active identity", "corrupt");
        }
        return { redemption, identity, resumed: true };
      }
      const timestamp = checkedNow(this.now);
      if (redemption.expiresAtMs <= timestamp) {
        throw new SaasRegistryError("link ticket redemption expired", "conflict");
      }
      if (identity.state === "revoked") {
        throw new SaasRegistryError("federated identity is permanently revoked", "conflict");
      }
      if (identity.state === "reserved") {
        const identityResult = this.sqlite.query(`
          UPDATE federated_identities
          SET state = 'active', updated_at_ms = ?
          WHERE issuer = ? AND subject = ? AND account_id = ? AND state = 'reserved'
        `).run(timestamp, identityIssuer, identitySubject, accountId);
        if (identityResult.changes !== 1) {
          throw new SaasRegistryError("link identity state changed concurrently", "conflict");
        }
      }
      const ticketResult = this.sqlite.query(`
        UPDATE link_ticket_redemptions
        SET state = 'consumed'
        WHERE ticket_jti = ? AND state = 'reserved'
      `).run(ticketJti);
      if (ticketResult.changes !== 1) {
        throw new SaasRegistryError("link ticket state changed concurrently", "conflict");
      }
      return {
        redemption: this.getLinkTicketRedemption(ticketJti)!,
        identity: this.getFederatedIdentity(identityIssuer, identitySubject)!,
        resumed: false,
      };
    });
  }
}

/**
 * Signs short-lived federated assertions consumed by a newly provisioned coordinator.
 * Result delivery calls this signer only after a device has bound its provisioning receipt.
 * Stable issuer, audience, and claim validation prevent assertions crossing trust boundaries.
 */

import { randomUUID, type KeyObject } from "node:crypto";
import { SignJWT } from "jose";
import type { CentralAssertionInputs } from "./provisioner-client.ts";

const ISSUER = "https://dashboard.roosttt.com/__roost/auth";
const AUDIENCE = "roost-federated-identity";

export interface SignFederatedAssertionOptions {
  signingKey: KeyObject | CryptoKey;
  now?: () => number;
  createId?: () => string;
}

export class FederatedAssertionSigner {
  private readonly now: () => number;
  private readonly createId: () => string;

  constructor(private readonly options: SignFederatedAssertionOptions) {
    this.now = options.now ?? Date.now;
    this.createId = options.createId ?? randomUUID;
  }

  async sign(inputs: CentralAssertionInputs, deviceFingerprint: string): Promise<string> {
    const nowMs = this.now();
    const issuedAtMs = inputs.issuedAtMs ?? nowMs;
    const expiresAtMs = inputs.expiresAtMs ?? (issuedAtMs + 5 * 60_000);
    const jti = inputs.jti ?? this.createId();
    if (inputs.deviceFingerprint !== undefined && inputs.deviceFingerprint !== deviceFingerprint) {
      throw new Error("assertion device binding mismatch");
    }
    const iat = Math.floor(issuedAtMs / 1_000);
    const exp = Math.floor(expiresAtMs / 1_000);
    if (exp <= iat || exp > iat + 300 || expiresAtMs <= nowMs) throw new Error("assertion lifetime is invalid");
    return new SignJWT({
      purpose: inputs.purpose,
      account_id: inputs.accountId,
      coordinator_id: inputs.coordinatorId,
      route_key: inputs.routeKey,
      identity_issuer: inputs.identityIssuer,
      identity_subject: inputs.identitySubject,
      email_normalized: inputs.emailNormalized,
      device_fp: deviceFingerprint,
    })
      .setProtectedHeader({ alg: "EdDSA", typ: "JWT" })
      .setIssuer(ISSUER)
      .setAudience(AUDIENCE)
      .setJti(jti)
      .setIssuedAt(iat)
      .setExpirationTime(exp)
      .sign(this.options.signingKey);
  }
}

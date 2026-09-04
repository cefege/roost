/**
 * Defines the hidden managed-instance commands and their database-facing results.
 * Parsing, seeding, inspection, and dispatch share these contracts without sharing implementation.
 * The original saas-instance entry point re-exports them for existing callers.
 */

import type { ManagedCredentialTopology } from "../../coord/src/managed-container-invariant.ts";

export interface SeedOwnerIdentityInput {
  accountId: string;
  coordinatorId: string;
}

export interface SeedOwnerActivationInput extends SeedOwnerIdentityInput {
  email: string;
}

export interface SeedGoogleOwnerPayload {
  subject: string;
  emailNormalized: string;
}

export type SaasInstanceCommand =
  | { action: "seed-owner-activation"; input: SeedOwnerActivationInput }
  | { action: "seed-signup-gateway-owner-activation"; input: SeedOwnerActivationInput }
  | { action: "seed-google-owner"; input: SeedOwnerIdentityInput }
  | { action: "release-owner-activation-email" }
  | { action: "activation-status" }
  | { action: "health" };

export interface SeedOwnerActivationOptions {
  emailOutboxKey: string;
  webPublicUrl: string;
  tenantRouteKey: string;
  now?: () => number;
  createId?: () => string;
  createTokenBytes?: () => Uint8Array;
}

export interface SeedSignupGatewayOwnerActivationOptions {
  now?: () => number;
}

export interface SeedGoogleOwnerOptions {
  now?: () => number;
}

export interface OwnerActivationIdentity {
  accountId: string;
  coordinatorId: string;
}

export interface SeedOwnerActivationResult extends OwnerActivationIdentity {
  expiresAtMs: number;
}

export interface OwnerActivationStatus extends OwnerActivationIdentity {
  activated: boolean;
  expiresAtMs: number;
  topology: ManagedCredentialTopology;
}

// Defines the durable registry records shared by SaaS control-plane operations.
// Registry stores and lifecycle callers use these shapes at the SQLite boundary.
// Keeping wire-neutral state names here prevents storage modules from drifting.
export const ACCOUNT_STATES = ["pending", "active", "disabled"] as const;
export type AccountState = typeof ACCOUNT_STATES[number];

export const COORDINATOR_STATES = [
  "reserved",
  "seeded",
  "running",
  "routed",
  "invited",
  "active",
  "disabled",
  "failed",
] as const;
export type CoordinatorState = typeof COORDINATOR_STATES[number];

export const GOOGLE_IDENTITY_ISSUER = "https://accounts.google.com" as const;
export const FEDERATED_IDENTITY_STATES = ["reserved", "active", "revoked"] as const;
export type FederatedIdentityState = typeof FEDERATED_IDENTITY_STATES[number];
export const PROVISIONING_JOB_KINDS = [
  "verified-email",
  "google-signup",
  "google-login",
  "google-link",
] as const;
export type ProvisioningJobKind = typeof PROVISIONING_JOB_KINDS[number];
export const PROVISIONING_JOB_STATES = ["pending", "running", "succeeded", "failed"] as const;
export type ProvisioningJobState = typeof PROVISIONING_JOB_STATES[number];
export const PROVISIONING_ASSERTION_PURPOSES = ["continue", "link"] as const;
export type ProvisioningAssertionPurpose = typeof PROVISIONING_ASSERTION_PURPOSES[number];
export const LINK_TICKET_REDEMPTION_STATES = ["reserved", "consumed"] as const;
export type LinkTicketRedemptionState = typeof LINK_TICKET_REDEMPTION_STATES[number];

export interface RegistryAccount {
  id: string;
  emailNormalized: string;
  routeKey: string;
  state: AccountState;
  createdAtMs: number;
  activatedAtMs: number | null;
  disabledAtMs: number | null;
}

export interface RegistryCoordinator {
  id: string;
  accountId: string;
  ordinal: number;
  routeKey: string;
  hostname: string;
  containerName: string;
  dataDir: string;
  imageDigest: string;
  state: CoordinatorState;
  createdAtMs: number;
  seededAtMs: number | null;
  runningAtMs: number | null;
  routedAtMs: number | null;
  invitedAtMs: number | null;
  activatedAtMs: number | null;
  disabledAtMs: number | null;
  failedAtMs: number | null;
  updatedAtMs: number;
  lastError: string | null;
}

export interface RegistryLease {
  coordinatorId: string;
  operation: string;
  owner: string;
  acquiredAtMs: number;
  expiresAtMs: number;
}

export interface RegistryGlobalLease {
  resource: string;
  operation: string;
  owner: string;
  acquiredAtMs: number;
  expiresAtMs: number;
}

export interface AccountReservation {
  account: RegistryAccount;
  coordinator: RegistryCoordinator;
  resumed: boolean;
}

export interface RegistryFederatedIdentity {
  issuer: typeof GOOGLE_IDENTITY_ISSUER;
  subject: string;
  accountId: string;
  emailNormalized: string;
  state: FederatedIdentityState;
  createdAtMs: number;
  updatedAtMs: number;
  verifiedAtMs: number;
}

export interface ReserveGoogleSignupOptions {
  issuer: string;
  subject: string;
  emailNormalized: string;
  verifiedAtMs: number;
  imageDigest: string;
}

export type GoogleSignupReservation =
  | {
    outcome: "reserved" | "existing";
    account: RegistryAccount;
    coordinator: RegistryCoordinator;
    identity: RegistryFederatedIdentity;
    resumed: boolean;
  }
  | { outcome: "proof-required" };

export interface ProvisioningAssertionInput {
  purpose: ProvisioningAssertionPurpose;
  routeKey: string;
  deviceFingerprint: string;
  jti: string;
  issuedAtMs: number;
  expiresAtMs: number;
}

export interface RegistryProvisioningJob {
  id: string;
  idempotencyKeyHash: string;
  kind: ProvisioningJobKind;
  emailNormalized: string;
  identityIssuer: typeof GOOGLE_IDENTITY_ISSUER | null;
  identitySubject: string | null;
  activationTokenHash: string | null;
  verifiedAtMs: number;
  accountId: string;
  coordinatorId: string;
  state: ProvisioningJobState;
  attempts: number;
  nextAttemptAtMs: number;
  lockedUntilMs: number | null;
  leaseToken: string | null;
  lastError: string | null;
  assertionInput: ProvisioningAssertionInput | null;
  createdAtMs: number;
  updatedAtMs: number;
  succeededAtMs: number | null;
  failedAtMs: number | null;
}

export interface ClaimedProvisioningJob extends RegistryProvisioningJob {
  state: "running";
  lockedUntilMs: number;
  leaseToken: string;
}

export interface InsertProvisioningJobOptions {
  idempotencyKeyHash: string;
  kind: ProvisioningJobKind;
  emailNormalized: string;
  identityIssuer?: string | null;
  identitySubject?: string | null;
  activationTokenHash?: string | null;
  verifiedAtMs: number;
  accountId: string;
  coordinatorId: string;
  nextAttemptAtMs?: number;
  assertionInput?: ProvisioningAssertionInput | null;
}

export interface ProvisioningJobInsertion {
  job: RegistryProvisioningJob;
  inserted: boolean;
}

export interface ClaimDueProvisioningJobsOptions {
  leaseDurationMs: number;
  limit: number;
}

export interface RegistryLinkTicketRedemption {
  ticketJti: string;
  accountId: string;
  coordinatorId: string;
  deviceFingerprint: string;
  identityIssuer: typeof GOOGLE_IDENTITY_ISSUER;
  identitySubject: string;
  state: LinkTicketRedemptionState;
  expiresAtMs: number;
}

export interface ReserveLinkTicketRedemptionOptions {
  ticketJti: string;
  accountId: string;
  coordinatorId: string;
  deviceFingerprint: string;
  identityIssuer: string;
  identitySubject: string;
  emailNormalized: string;
  verifiedAtMs: number;
  expiresAtMs: number;
}

export interface ConsumeLinkTicketRedemptionOptions {
  ticketJti: string;
  accountId: string;
  coordinatorId: string;
  deviceFingerprint: string;
  identityIssuer: string;
  identitySubject: string;
}

export interface LinkTicketReservation {
  redemption: RegistryLinkTicketRedemption;
  identity: RegistryFederatedIdentity;
  resumed: boolean;
}

export interface OpenSaasRegistryOptions {
  path?: string;
  rootDir?: string;
  now?: () => number;
  createId?: () => string;
  createRouteKey?: () => string;
}

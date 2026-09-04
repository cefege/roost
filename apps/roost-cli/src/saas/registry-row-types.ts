// Describes the raw SQLite rows read from the SaaS registry.
// Registry query modules pass these records through strict row mappers.
// The snake-case fields intentionally mirror the durable database schema.
export interface RawAccount {
  id: string;
  email_normalized: string;
  route_key: unknown;
  state: string;
  created_at_ms: number;
  activated_at_ms: number | null;
  disabled_at_ms: number | null;
}

export interface RawCoordinator {
  id: string;
  account_id: string;
  ordinal: number;
  route_key: unknown;
  hostname: string;
  container_name: string;
  data_dir: string;
  image_digest: string;
  state: string;
  created_at_ms: number;
  seeded_at_ms: number | null;
  running_at_ms: number | null;
  routed_at_ms: number | null;
  invited_at_ms: number | null;
  activated_at_ms: number | null;
  disabled_at_ms: number | null;
  failed_at_ms: number | null;
  updated_at_ms: number;
  last_error: string | null;
}

export interface RawLease {
  coordinator_id: string;
  operation: string;
  owner: string;
  acquired_at_ms: number;
  expires_at_ms: number;
}

export interface RawGlobalLease {
  resource: string;
  operation: string;
  owner: string;
  acquired_at_ms: number;
  expires_at_ms: number;
}

export interface RawFederatedIdentity {
  issuer: string;
  subject: string;
  account_id: string;
  email_normalized: string;
  state: string;
  created_at_ms: number;
  updated_at_ms: number;
  verified_at_ms: number;
}

export interface RawProvisioningJob {
  id: string;
  idempotency_key_hash: string;
  kind: string;
  email_normalized: string;
  identity_issuer: string | null;
  identity_subject: string | null;
  activation_token_hash: string | null;
  verified_at_ms: number;
  account_id: string;
  coordinator_id: string;
  state: string;
  attempts: number;
  next_attempt_at_ms: number;
  locked_until_ms: number | null;
  lease_token: string | null;
  last_error: string | null;
  assertion_purpose: string | null;
  assertion_route_key: string | null;
  assertion_device_fp: string | null;
  assertion_jti: string | null;
  assertion_issued_at_ms: number | null;
  assertion_expires_at_ms: number | null;
  created_at_ms: number;
  updated_at_ms: number;
  succeeded_at_ms: number | null;
  failed_at_ms: number | null;
}

export interface RawLinkTicketRedemption {
  ticket_jti: string;
  account_id: string;
  coordinator_id: string;
  device_fp: string;
  identity_issuer: string;
  identity_subject: string;
  state: string;
  expires_at_ms: number;
}

// Pairing durable state owns request creation and trusted approval transitions.
// Confirmation calls the account checks below before it creates browser authority;
// handlers validate wire inputs and publish only after these transactions commit.

import { Code, ConnectError } from "@connectrpc/connect";
import { fingerprintOf } from "@roost/shared/fingerprint";
import { log } from "@roost/shared/log";
import { sql } from "kysely";
import type { KyselyDB } from "../db/connection.ts";
import type { PairRequestProvenance } from "./pair-request-provenance.ts";
import {
  PAIRING_CLIENT_RELOAD_MESSAGE,
  PAIRING_CEREMONY_VERSION,
} from "./pairing-secrets.ts";

export const MAX_PENDING_PAIR_REQUESTS = 32;

export interface PairRequestCreateInput {
  ephemeralId: string;
  requesterTokenHash: string;
  publicKey: Uint8Array;
  label: string;
  now: number;
  expiresAtMs: number;
  provenance: PairRequestProvenance;
  edgeIdentityProvider: string | null;
  edgeIdentity: string | null;
  edgeIdentityVerified: number;
}

export type PairRequestCreateResult =
  | { kind: "created"; expiredIds: string[] }
  | { kind: "retry"; expiredIds: string[] }
  | { kind: "expired"; expiredIds: string[] };

export interface PairApprovalInput {
  ephemeralId: string;
  verificationCodeHash: string;
  approverFingerprint: string | null;
  now: number;
}

export type PairApprovalResult =
  | { kind: "approved"; requesterFingerprint: string }
  | { kind: "retry"; requesterFingerprint: string }
  | { kind: "expired"; requesterFingerprint: string };

export async function createPairRequest(
  db: KyselyDB,
  input: PairRequestCreateInput,
): Promise<PairRequestCreateResult> {
  return db.transaction().execute(async (trx): Promise<PairRequestCreateResult> => {
    const existing = await trx.selectFrom("pair_requests")
      .select([
        "ceremony_version",
        "expires_at_ms",
        "label",
        "public_key",
        "requester_token_hash",
        "status",
      ])
      .where("ephemeral_id", "=", input.ephemeralId)
      .executeTakeFirst();
    if (existing) {
      if (existing.status !== "pending" && existing.status !== "verification_required") {
        throw new ConnectError("pair request is already terminal", Code.FailedPrecondition);
      }
      const existingPublicKey = existing.public_key instanceof Uint8Array
        ? existing.public_key
        : new Uint8Array(existing.public_key);
      if (
        existing.ceremony_version !== PAIRING_CEREMONY_VERSION
        || existing.label !== input.label
        || existing.requester_token_hash !== input.requesterTokenHash
        || !samePublicKey(existingPublicKey, input.publicKey)
      ) {
        throw new ConnectError("pair request id already exists", Code.AlreadyExists);
      }
      if (existing.expires_at_ms > input.now) {
        return { kind: "retry", expiredIds: [] };
      }
      const expired = await trx.updateTable("pair_requests")
        .set({
          status: "expired",
          decided_at_ms: input.now,
          verification_code_hash: null,
        })
        .where("ephemeral_id", "=", input.ephemeralId)
        .where("status", "in", ["pending", "verification_required"])
        .returning("ephemeral_id")
        .execute();
      return { kind: "expired", expiredIds: expired.map((row) => row.ephemeral_id) };
    }

    const requesterFingerprint = await fingerprintOf(input.publicKey);
    const requesterRevocation = await trx.selectFrom("authorized_key_revocations")
      .select("fingerprint")
      .where("fingerprint", "=", requesterFingerprint)
      .executeTakeFirst();
    if (requesterRevocation) {
      throw new ConnectError("authorized key was revoked", Code.PermissionDenied);
    }
    const expiredRows = await trx.updateTable("pair_requests")
      .set({
        status: "expired",
        decided_at_ms: input.now,
        verification_code_hash: null,
      })
      .where("status", "in", ["pending", "verification_required"])
      .where("expires_at_ms", "<=", input.now)
      .returning("ephemeral_id")
      .execute();
    const replacedRows = await trx.updateTable("pair_requests")
      .set({
        status: "expired",
        decided_at_ms: input.now,
        verification_code_hash: null,
      })
      .where("public_key", "=", input.publicKey)
      .where("status", "in", ["pending", "verification_required"])
      .returning("ephemeral_id")
      .execute();
    const liveRow = await trx.selectFrom("pair_requests")
      .select(trx.fn.countAll<number>().as("live"))
      .where("status", "in", ["pending", "verification_required"])
      .executeTakeFirst();
    const live = Number(liveRow?.live ?? 0);
    if (live >= MAX_PENDING_PAIR_REQUESTS) {
      log.warn("pair.connect", "create_refused_at_cap", {
        live,
        client_ip: input.provenance.sourceIp,
      });
      throw new ConnectError("too many live pair requests", Code.ResourceExhausted);
    }
    const inserted = await sql`
      INSERT INTO pair_requests (
        id, ephemeral_id, public_key, label, status, created_at_ms, decided_at_ms,
        ceremony_version, requester_token_hash, verification_code_hash,
        verification_attempts, approved_by_fp, approved_account_id,
        user_agent, client_browser, client_os, client_device_type, source_ip,
        country_code, region, city, edge_identity_provider, edge_identity,
        edge_identity_verified, expires_at_ms
      )
      SELECT ${input.ephemeralId}, ${input.ephemeralId}, ${input.publicKey}, ${input.label},
        'pending', ${input.now}, NULL, ${PAIRING_CEREMONY_VERSION},
        ${input.requesterTokenHash}, NULL, 0, NULL, NULL, ${input.provenance.userAgent},
        ${input.provenance.clientBrowser}, ${input.provenance.clientOs},
        ${input.provenance.clientDeviceType}, ${input.provenance.sourceIp},
        ${input.provenance.countryCode}, ${input.provenance.region}, ${input.provenance.city},
        ${input.edgeIdentityProvider}, ${input.edgeIdentity}, ${input.edgeIdentityVerified},
        ${input.expiresAtMs}
      WHERE NOT EXISTS (
        SELECT 1 FROM authorized_key_revocations WHERE fingerprint = ${requesterFingerprint}
      )
    `.execute(trx);
    if (inserted.numAffectedRows !== 1n) {
      throw new ConnectError("authorized key was revoked", Code.PermissionDenied);
    }
    return {
      kind: "created",
      expiredIds: [
        ...expiredRows.map((row) => row.ephemeral_id),
        ...replacedRows.map((row) => row.ephemeral_id),
      ],
    };
  });
}

export async function approvePairRequest(
  db: KyselyDB,
  input: PairApprovalInput,
): Promise<PairApprovalResult> {
  return db.transaction().execute(async (trx): Promise<PairApprovalResult> => {
    const accountId = await pairedBrowserAccountId(
      trx,
      input.approverFingerprint ?? undefined,
    );
    if (accountId === null) {
      throw new ConnectError("pairing account is unavailable", Code.FailedPrecondition);
    }
    if (!await pairingApprovalRemainsValid(trx, input.approverFingerprint, accountId)) {
      throw new ConnectError("approver is no longer authorized", Code.PermissionDenied);
    }
    const row = await trx.selectFrom("pair_requests")
      .select([
        "approved_account_id",
        "approved_by_fp",
        "ceremony_version",
        "expires_at_ms",
        "public_key",
        "status",
        "verification_code_hash",
      ])
      .where("ephemeral_id", "=", input.ephemeralId)
      .executeTakeFirst();
    if (!row) throw new ConnectError("not found", Code.NotFound);
    if (row.ceremony_version !== PAIRING_CEREMONY_VERSION) {
      throw new ConnectError(PAIRING_CLIENT_RELOAD_MESSAGE, Code.FailedPrecondition);
    }
    const publicKey = row.public_key instanceof Uint8Array
      ? row.public_key
      : new Uint8Array(row.public_key);
    const requesterFingerprint = await fingerprintOf(publicKey);
    if (
      (row.status === "pending" || row.status === "verification_required")
      && row.expires_at_ms <= input.now
    ) {
      await trx.updateTable("pair_requests")
        .set({
          status: "expired",
          decided_at_ms: input.now,
          verification_code_hash: null,
        })
        .where("ephemeral_id", "=", input.ephemeralId)
        .where("status", "in", ["pending", "verification_required"])
        .executeTakeFirstOrThrow();
      return { kind: "expired", requesterFingerprint };
    }
    if (row.status === "verification_required") {
      if (
        row.verification_code_hash === input.verificationCodeHash
        && row.approved_by_fp === input.approverFingerprint
        && row.approved_account_id === accountId
      ) {
        return { kind: "retry", requesterFingerprint };
      }
      throw new ConnectError("pair request was already approved", Code.FailedPrecondition);
    }
    if (row.status !== "pending") {
      throw new ConnectError("pair request is not pending", Code.FailedPrecondition);
    }
    const requesterRevocation = await trx.selectFrom("authorized_key_revocations")
      .select("fingerprint")
      .where("fingerprint", "=", requesterFingerprint)
      .executeTakeFirst();
    if (requesterRevocation) {
      throw new ConnectError("authorized key was revoked", Code.PermissionDenied);
    }
    const updated = await trx.updateTable("pair_requests")
      .set({
        status: "verification_required",
        ceremony_version: PAIRING_CEREMONY_VERSION,
        verification_code_hash: input.verificationCodeHash,
        verification_attempts: 0,
        approved_by_fp: input.approverFingerprint,
        approved_account_id: accountId,
      })
      .where("ephemeral_id", "=", input.ephemeralId)
      .where("status", "=", "pending")
      .where("expires_at_ms", ">", input.now)
      .where("ceremony_version", "=", PAIRING_CEREMONY_VERSION)
      .executeTakeFirst();
    if (updated.numUpdatedRows !== 1n) {
      throw new ConnectError("pair request is not pending", Code.FailedPrecondition);
    }
    return { kind: "approved", requesterFingerprint };
  });
}

export async function pairedBrowserAccountId(
  db: KyselyDB,
  authorityFingerprint?: string,
): Promise<string | null> {
  if (authorityFingerprint) {
    const device = await db.selectFrom("account_devices")
      .select("account_id")
      .where("fingerprint", "=", authorityFingerprint)
      .executeTakeFirst();
    if (device) return device.account_id;
  }
  const accounts = await db.selectFrom("accounts")
    .select(["id", "status"])
    .limit(2)
    .execute();
  return accounts.length === 1 && accounts[0]?.status === "active"
    ? accounts[0].id
    : null;
}

export async function pairingApprovalRemainsValid(
  db: KyselyDB,
  approvedByFingerprint: string | null,
  approvedAccountId: string | null,
): Promise<boolean> {
  if (approvedAccountId === null) return false;
  const account = await db.selectFrom("accounts")
    .select("status")
    .where("id", "=", approvedAccountId)
    .executeTakeFirst();
  if (account?.status !== "active") return false;
  if (approvedByFingerprint === null) return true;

  const approver = await db.selectFrom("authorized_keys as key")
    .leftJoin(
      "authorized_key_revocations as revocation",
      "revocation.fingerprint",
      "key.fingerprint",
    )
    .leftJoin("account_devices as device", "device.fingerprint", "key.fingerprint")
    .select([
      "key.fingerprint as fingerprint",
      "revocation.fingerprint as revokedFingerprint",
      "device.account_id as accountId",
    ])
    .where("key.fingerprint", "=", approvedByFingerprint)
    .executeTakeFirst();
  if (!approver || approver.revokedFingerprint !== null) return false;
  return approver.accountId === approvedAccountId;
}

export type PairedBrowserAssociationConflict = "worker_key" | "other_account";

export async function pairedBrowserAssociationConflict(
  db: KyselyDB,
  fingerprint: string,
  accountId: string,
): Promise<PairedBrowserAssociationConflict | null> {
  const worker = await db.selectFrom("workers").select("fp")
    .where("fp", "=", fingerprint).executeTakeFirst();
  if (worker) return "worker_key";
  const device = await db.selectFrom("account_devices").select("account_id")
    .where("fingerprint", "=", fingerprint).executeTakeFirst();
  return device && device.account_id !== accountId ? "other_account" : null;
}

export async function associatePairedBrowser(
  db: KyselyDB,
  fingerprint: string,
  accountId: string,
  now: number,
): Promise<void> {
  const conflict = await pairedBrowserAssociationConflict(db, fingerprint, accountId);
  if (conflict === "worker_key") {
    throw new ConnectError("device key is already in use by a worker", Code.AlreadyExists);
  }
  if (conflict === "other_account") {
    throw new ConnectError("device already belongs to another account", Code.AlreadyExists);
  }
  await db.insertInto("account_devices").values({
    fingerprint,
    account_id: accountId,
    added_at_ms: now,
    last_seen_at_ms: now,
  }).onConflict((conflict) => conflict.column("fingerprint").doUpdateSet({
    last_seen_at_ms: now,
  })).execute();
}

function samePublicKey(left: Uint8Array, right: Uint8Array): boolean {
  if (left.byteLength !== right.byteLength) return false;
  for (let index = 0; index < left.byteLength; index += 1) {
    if (left[index] !== right[index]) return false;
  }
  return true;
}

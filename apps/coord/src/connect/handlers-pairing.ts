// Pairing handlers isolate the public request flow from authenticated approval.
// Approval persists the key and its account association atomically so a pending
// device cannot gain browser authority before an authorized decision commits.
// The association helpers below are private for that reason: they must only run
// inside the approval transaction, never from another enrollment path.

import { create } from "@bufbuild/protobuf";
import { Code, ConnectError, type ServiceImpl } from "@connectrpc/connect";
import { randomUUID } from "node:crypto";
import { fingerprintOf } from "@roost/shared/fingerprint";
import { log } from "@roost/shared/log";
import {
  CoordinatorService,
  PairApproveResponseSchema,
  PairCreateResponseSchema,
  PairDenyResponseSchema,
  PairListResponseSchema,
  PairPollResponseSchema,
} from "@roost/shared/proto/coordinator_pb";
import { PairRequestSchema } from "@roost/shared/proto/wire_pb";
import { sql } from "kysely";
import { decodeEd25519Pubkey, isAuthorizedKeyRevoked } from "../authorized-keys.ts";
import type { KyselyDB } from "../db/connection.ts";
import { pairBus } from "../buses.ts";
import { refreshJwtKey } from "../jwt.ts";
import { assertOnHost } from "../middleware/caller-origin.ts";
import {
  callerOrigin,
  optionalAccountDevice,
} from "./auth-interceptor.ts";
import { capturePairRequestProvenance } from "./pair-request-provenance.ts";
import type { ConnectDeps } from "./router.ts";

export const PAIR_REQUEST_TTL_MS = 10 * 60_000;
export const MAX_PENDING_PAIR_REQUESTS = 32;

/** The account a newly approved browser joins. An approver's own account wins;
 * otherwise the single active account is the only unambiguous answer. */
async function pairedBrowserAccountId(
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

async function associatePairedBrowser(
  db: KyselyDB,
  fingerprint: string,
  accountId: string | null,
  now: number,
): Promise<void> {
  if (accountId === null) return;
  const worker = await db.selectFrom("workers").select("fp")
    .where("fp", "=", fingerprint).executeTakeFirst();
  const device = await db.selectFrom("account_devices").select("account_id")
    .where("fingerprint", "=", fingerprint).executeTakeFirst();
  if (worker) {
    throw new ConnectError("device key is already in use by a worker", Code.AlreadyExists);
  }
  if (device && device.account_id !== accountId) {
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

type PairingMethods =
  | "pairCreate"
  | "pairPoll"
  | "pairList"
  | "pairApprove"
  | "pairDeny";

export function makePairingHandlers(
  deps: ConnectDeps,
): Pick<ServiceImpl<typeof CoordinatorService>, PairingMethods> {
  return {
    // ─── pair ──────────────────────────────────────────────────────────
    async pairCreate(req, ctx) {
      const origin = callerOrigin(ctx.values);
      const requestHeaders = ctx.requestHeader ?? new Headers();
      let accessIdentity: { email: string } | null = null;
      if (deps.cfAccess && !origin.onHost) {
        accessIdentity = await deps.cfAccess.verify(requestHeaders);
        if (accessIdentity === null) {
          throw new ConnectError("pairing requires front-door sign-in", Code.Unauthenticated);
        }
      }

      const provenance = capturePairRequestProvenance(requestHeaders, origin);
      const pubkey = decodeEd25519Pubkey(req.sshPubkeyB64);
      if (!pubkey) throw new ConnectError("invalid ssh_pubkey_b64", Code.InvalidArgument);
      const proposedFp = await fingerprintOf(pubkey);
      if (await isAuthorizedKeyRevoked(deps.db, proposedFp)) {
        throw new ConnectError("authorized key was revoked", Code.PermissionDenied);
      }
      const id = randomUUID();
      const ephBuf = new Uint8Array(16);
      crypto.getRandomValues(ephBuf);
      const ephemeral_id = Array.from(ephBuf).map(b => b.toString(16).padStart(2, "0")).join("");
      const now = Date.now();
      const expiresAtMs = now + PAIR_REQUEST_TTL_MS;
      const edgeIdentityProvider = accessIdentity === null ? null : "cloudflare-access";
      const edgeIdentity = accessIdentity?.email ?? null;
      const edgeIdentityVerified = accessIdentity === null ? 0 : 1;
      const expiredIds = await deps.db.transaction().execute(async (trx) => {
        const expiredRows = await trx.updateTable("pair_requests")
          .set({ status: "expired", decided_at_ms: now })
          .where("status", "=", "pending")
          .where("expires_at_ms", "<=", now)
          .returning("ephemeral_id")
          .execute();
        const pendingRow = await trx.selectFrom("pair_requests")
          .select(trx.fn.countAll<number>().as("pending"))
          .where("status", "=", "pending")
          .executeTakeFirst();
        const pending = Number(pendingRow?.pending ?? 0);
        if (pending >= MAX_PENDING_PAIR_REQUESTS) {
          log.warn("pair.connect", "create_refused_at_cap", {
            pending,
            client_ip: provenance.sourceIp,
          });
          throw new ConnectError("too many pending pair requests", Code.ResourceExhausted);
        }
        const inserted = await sql`
          INSERT INTO pair_requests (
            id, ephemeral_id, public_key, label, status, created_at_ms, decided_at_ms,
            user_agent, client_browser, client_os, client_device_type, source_ip,
            country_code, region, city, edge_identity_provider, edge_identity,
            edge_identity_verified, expires_at_ms
          )
          SELECT ${id}, ${ephemeral_id}, ${pubkey}, ${req.label}, 'pending', ${now}, NULL,
            ${provenance.userAgent}, ${provenance.clientBrowser}, ${provenance.clientOs},
            ${provenance.clientDeviceType}, ${provenance.sourceIp}, ${provenance.countryCode},
            ${provenance.region}, ${provenance.city}, ${edgeIdentityProvider},
            ${edgeIdentity}, ${edgeIdentityVerified}, ${expiresAtMs}
          WHERE NOT EXISTS (
            SELECT 1 FROM authorized_key_revocations WHERE fingerprint = ${proposedFp}
          )
        `.execute(trx);
        if (inserted.numAffectedRows !== 1n) {
          throw new ConnectError("authorized key was revoked", Code.PermissionDenied);
        }
        return expiredRows.map((row) => row.ephemeral_id);
      });

      for (const expiredId of expiredIds) {
        pairBus.publish({ kind: "removed", ephemeral_id: expiredId });
      }
      pairBus.publish({
        kind: "pending",
        ephemeral_id,
        label: req.label,
        created_at_ms: now,
        user_agent: provenance.userAgent ?? "",
        client_browser: provenance.clientBrowser ?? "",
        client_os: provenance.clientOs ?? "",
        client_device_type: provenance.clientDeviceType ?? "",
        source_ip: provenance.sourceIp,
        country_code: provenance.countryCode ?? "",
        region: provenance.region ?? "",
        city: provenance.city ?? "",
        edge_identity_provider: edgeIdentityProvider ?? "",
        edge_identity: edgeIdentity ?? "",
        edge_identity_verified: Boolean(edgeIdentityVerified),
        expires_at_ms: expiresAtMs,
      });
      log.info("pair.connect", "created", {
        ephemeral_id,
        label: req.label,
        client_ip: provenance.sourceIp,
        country_code: provenance.countryCode,
        edge_identity_verified: edgeIdentityVerified,
      });
      return create(PairCreateResponseSchema, { ephemeralId: ephemeral_id });
    },

    async pairPoll(req, _ctx) {
      const row = await deps.db.selectFrom("pair_requests")
        .select(["status", "expires_at_ms"])
        .where("ephemeral_id", "=", req.ephemeralId)
        .executeTakeFirst();
      if (!row) throw new ConnectError("not found", Code.NotFound);
      const status = row.status === "pending" && row.expires_at_ms <= Date.now()
        ? "expired"
        : row.status;
      return create(PairPollResponseSchema, {
        status,
        expiresAtMs: BigInt(row.expires_at_ms),
      });
    },

    // pairList/pairApprove/pairDeny: an authenticated browser (notifier click)
    // or a direct on-host caller. A proxied source address never grants
    // authority; otherwise a pending device could approve itself.
    async pairList(_req, ctx) {
      if (!optionalAccountDevice(ctx.values)) assertOnHost(callerOrigin(ctx.values));
      const now = Date.now();
      const rows = await deps.db.selectFrom("pair_requests")
        .select([
          "ephemeral_id",
          "label",
          "created_at_ms",
          "user_agent",
          "client_browser",
          "client_os",
          "client_device_type",
          "source_ip",
          "country_code",
          "region",
          "city",
          "edge_identity_provider",
          "edge_identity",
          "edge_identity_verified",
          "expires_at_ms",
        ])
        .where("status", "=", "pending")
        .where("expires_at_ms", ">", now)
        .orderBy("created_at_ms", "desc")
        .execute();
      return create(PairListResponseSchema, {
        requests: rows.map(r => create(PairRequestSchema, {
          ephemeralId: r.ephemeral_id,
          label: r.label,
          createdAtMs: BigInt(r.created_at_ms),
          userAgent: r.user_agent ?? "",
          clientBrowser: r.client_browser ?? "",
          clientOs: r.client_os ?? "",
          clientDeviceType: r.client_device_type ?? "",
          sourceIp: r.source_ip ?? "",
          countryCode: r.country_code ?? "",
          region: r.region ?? "",
          city: r.city ?? "",
          edgeIdentityProvider: r.edge_identity_provider ?? "",
          edgeIdentity: r.edge_identity ?? "",
          edgeIdentityVerified: r.edge_identity_verified === 1,
          expiresAtMs: BigInt(r.expires_at_ms),
        })),
      });
    },

    async pairApprove(req, ctx) {
      const caller = optionalAccountDevice(ctx.values);
      if (!caller) assertOnHost(callerOrigin(ctx.values));
      const now = Date.now();
      let approvedFp = "";
      await deps.db.transaction().execute(async (trx) => {
        let update = trx.updateTable("pair_requests")
          .set({ status: "approved", decided_at_ms: now })
          .where("ephemeral_id", "=", req.ephemeralId)
          .where("status", "=", "pending")
          .where("expires_at_ms", ">", now);
        if (caller) {
          update = update
            .where(sql<boolean>`EXISTS (
              SELECT 1 FROM authorized_keys WHERE fingerprint = ${caller.fingerprint}
            )`)
            .where(sql<boolean>`NOT EXISTS (
              SELECT 1 FROM authorized_key_revocations WHERE fingerprint = ${caller.fingerprint}
            )`);
        }
        const row = await update.returningAll().executeTakeFirst();
        if (!row) throw new ConnectError("not found or approver revoked", Code.NotFound);
        const pubkey = row.public_key instanceof Uint8Array
          ? row.public_key
          : new Uint8Array(row.public_key);
        const fp = await fingerprintOf(pubkey);
        const revoked = await trx.selectFrom("authorized_key_revocations")
          .select("fingerprint").where("fingerprint", "=", fp).executeTakeFirst();
        if (revoked) throw new ConnectError("authorized key was revoked", Code.PermissionDenied);
        await trx.insertInto("authorized_keys").values({
          fingerprint: fp,
          public_key: pubkey,
          label: row.label,
          added_at: now,
          paired_from_ip: row.source_ip,
          paired_country: row.country_code,
          paired_user_agent: row.user_agent,
          paired_edge_identity: row.edge_identity,
        }).onConflict((oc) => oc.column("fingerprint").doUpdateSet({
          label: row.label,
          paired_from_ip: row.source_ip,
          paired_country: row.country_code,
          paired_user_agent: row.user_agent,
          paired_edge_identity: row.edge_identity,
        })).execute();
        const accountId = caller?.kind === "account-device"
          ? caller.accountId
          : await pairedBrowserAccountId(trx, caller?.fingerprint);
        await associatePairedBrowser(trx, fp, accountId, now);
        approvedFp = fp;
      });
      refreshJwtKey(deps.jwtCache, approvedFp);
      pairBus.publish({ kind: "removed", ephemeral_id: req.ephemeralId });
      log.info("pair.connect", "approved", { ephemeral_id: req.ephemeralId, fp: approvedFp });
      return create(PairApproveResponseSchema, { ok: true });
    },

    async pairDeny(req, ctx) {
      if (!optionalAccountDevice(ctx.values)) assertOnHost(callerOrigin(ctx.values));
      const result = await deps.db.updateTable("pair_requests")
        .set({ status: "denied", decided_at_ms: Date.now() })
        .where("ephemeral_id", "=", req.ephemeralId)
        .where("status", "=", "pending").returningAll().executeTakeFirst();
      if (!result) throw new ConnectError("not found", Code.NotFound);
      pairBus.publish({ kind: "removed", ephemeral_id: req.ephemeralId });
      log.info("pair.connect", "denied", { ephemeral_id: req.ephemeralId });
      return create(PairDenyResponseSchema, { ok: true });
    },
  };
}

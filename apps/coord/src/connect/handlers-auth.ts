// Auth + device-pairing RPC handlers: coord identity, browser/worker key
// authorization, bootstrap-token mint/redeem, and the pairing flow
// (create/poll/list/approve/deny). Coordinator-relocation RPCs live in
// handlers-relocation.ts. Spread into router.ts's single router.service()
// literal.

import type { ServiceImpl } from "@connectrpc/connect";
import { Code, ConnectError } from "@connectrpc/connect";
import { create } from "@bufbuild/protobuf";
import { sql } from "kysely";
import { randomUUID } from "node:crypto";
import { log } from "@roost/shared/log";
import { isSupportedHostPlatform } from "@roost/shared/platform";
import {
  CoordinatorService,
  AuthCoordIdentityResponseSchema,
  AuthAuthorizeBrowserResponseSchema,
  AuthMintBootstrapResponseSchema, AuthRedeemWorkerResponseSchema, AuthRedeemBrowserResponseSchema,
  PairCreateResponseSchema, PairPollResponseSchema, PairListResponseSchema,
  PairApproveResponseSchema, PairDenyResponseSchema,
  DeviceRowSchema, DevicesListResponseSchema, DevicesRevokeResponseSchema,
  DevicesRotateCurrentResponseSchema,
} from "@roost/shared/proto/coordinator_pb";
import { PairRequestSchema } from "@roost/shared/proto/wire_pb";
import { callerOrigin, requireAuth, optionalAuth } from "./auth-interceptor.ts";
import { fingerprintOf } from "@roost/shared/fingerprint";
import { invalidateJwtKey, refreshJwtKey } from "../jwt.ts";
import { decodeEd25519Pubkey, isAuthorizedKeyRevoked } from "../authorized-keys.ts";
import { assertOnHost, assertOnHostOrTailnet } from "../middleware/caller-origin.ts";
import { COORD_GIT_SHA } from "../git-sha.ts";
import { randomToken } from "./router-helpers.ts";
import { pairBus } from "../buses.ts";
import type { ConnectDeps } from "./router.ts";

type AuthMethods =
  | "authCoordIdentity" | "authAuthorizeBrowser" | "authMintBootstrap"
  | "authRedeemWorker" | "authRedeemBrowser"
  | "pairCreate" | "pairPoll" | "pairList" | "pairApprove" | "pairDeny"
  | "devicesList" | "devicesRevoke" | "devicesRotateCurrent";

// Coordinator-relocation RPCs live in handlers-relocation.ts (self-contained
// domain: follower-chain walk + mint/redeem).
export function makeAuthHandlers(
  deps: ConnectDeps,
): Pick<ServiceImpl<typeof CoordinatorService>, AuthMethods> {
  return {
    // ─── auth ──────────────────────────────────────────────────────────
    async authCoordIdentity(_req, ctx) {
      // public
      const handoff = deps.move?.current();
      return create(AuthCoordIdentityResponseSchema, {
        fingerprintHex: deps.coordKey.verifyingKeyKid(),
        gitSha: COORD_GIT_SHA,
        publicUrl: deps.cfg.publicUrl ?? "",
        relocatedToUrl: handoff?.role === "SOURCE" && handoff.phase === "COMMITTED" ? handoff.target_url : undefined,
        handoffId: handoff?.handoff_id,
        publicListener: callerOrigin(ctx.values).listener === "public-edge",
      });
    },

    async authAuthorizeBrowser(req, ctx) {
      // public, loopback OR tailnet peer (the tailnet is the trust boundary —
      // a fresh phone browser self-registers over the FQDN with no prior cred).
      assertOnHostOrTailnet(callerOrigin(ctx.values));
      const pubkey = decodeEd25519Pubkey(req.sshPubkeyB64);
      if (!pubkey) throw new ConnectError("invalid ssh_pubkey_b64", Code.InvalidArgument);
      const fp = await fingerprintOf(pubkey);
      if (await isAuthorizedKeyRevoked(deps.db, fp)) {
        throw new ConnectError("authorized key was revoked", Code.PermissionDenied);
      }
      await deps.db.insertInto("authorized_keys").values({
        fingerprint: fp, public_key: pubkey, label: req.label, added_at: Date.now(),
      }).onConflict((oc) => oc.column("fingerprint").doUpdateSet({ label: req.label })).execute();
      // Authorization must not advance the revocation generation: a verifier
      // may already have loaded this newly committed row.
      refreshJwtKey(deps.jwtCache, fp);
      log.info("auth.connect", "browser_authorized", { fp, label: req.label });
      return create(AuthAuthorizeBrowserResponseSchema, { fingerprint: fp });
    },

    async authMintBootstrap(req, ctx) {
      const caller = requireAuth(ctx.values);
      const token = randomToken("roost_bt_", 24);
      const now = Date.now();
      const expires_at_ms = now + 24 * 60 * 60 * 1000;
      await deps.db.insertInto("bootstrap_tokens").values({
        token, kind: req.kind, label: req.label,
        created_at_ms: now, expires_at_ms,
        used_at_ms: null, used_by_fp: null,
        minted_by_fp: caller.fingerprint,
      }).execute();
      log.info("auth.connect", "bootstrap_minted", { kind: req.kind, label: req.label });
      return create(AuthMintBootstrapResponseSchema, { token, expiresAtMs: BigInt(expires_at_ms) });
    },

    async authRedeemWorker(req, _ctx) {
      if (!isSupportedHostPlatform(req.os)) {
        throw new ConnectError("unsupported worker os", Code.InvalidArgument);
      }
      const pubkey = decodeEd25519Pubkey(req.sshPubkeyB64);
      if (!pubkey) throw new ConnectError("invalid ssh_pubkey_b64", Code.InvalidArgument);
      const fp = await fingerprintOf(pubkey);
      if (await isAuthorizedKeyRevoked(deps.db, fp)) {
        throw new ConnectError("authorized key was revoked", Code.Unauthenticated);
      }
      const now = Date.now();
      await deps.db.transaction().execute(async (trx) => {
        const claimed = await trx.updateTable("bootstrap_tokens")
          .set({ used_at_ms: now, used_by_fp: fp })
          .where("token", "=", req.token)
          .where("kind", "=", "worker")
          .where("used_at_ms", "is", null)
          .where("expires_at_ms", ">=", now)
          .returning("token")
          .executeTakeFirst();
        if (!claimed) throw new ConnectError("invalid or expired token", Code.Unauthenticated);
        await trx.insertInto("authorized_keys").values({
          fingerprint: fp, public_key: pubkey, label: req.label, added_at: now,
        }).onConflict((oc) => oc.column("fingerprint").doUpdateSet({ label: req.label })).execute();
        await trx.insertInto("workers").values({
          fp, label: req.label, os: req.os,
          git_sha: req.gitSha ?? null, host_metrics_json: null,
          registered_at_ms: now, last_seen_ms: now,
        }).onConflict((oc) => oc.column("fp").doUpdateSet({
          label: req.label,
          os: req.os,
          git_sha: req.gitSha ?? null,
          last_seen_ms: now,
        })).execute();
      });
      refreshJwtKey(deps.jwtCache, fp);
      log.info("auth.connect", "worker_redeemed", { fp, label: req.label });
      return create(AuthRedeemWorkerResponseSchema, {
        fingerprint: fp, label: req.label,
        coordVerifyingKeyB64: deps.coordKey.verifyingKeyB64(),
      });
    },

    async authRedeemBrowser(req, _ctx) {
      const pubkey = decodeEd25519Pubkey(req.sshPubkeyB64);
      if (!pubkey) throw new ConnectError("invalid ssh_pubkey_b64", Code.InvalidArgument);
      const fp = await fingerprintOf(pubkey);
      if (await isAuthorizedKeyRevoked(deps.db, fp)) {
        throw new ConnectError("authorized key was revoked", Code.Unauthenticated);
      }
      const now = Date.now();
      await deps.db.transaction().execute(async (trx) => {
        const claimed = await trx.updateTable("bootstrap_tokens")
          .set({ used_at_ms: now, used_by_fp: fp })
          .where("token", "=", req.token)
          .where("kind", "=", "browser")
          .where("used_at_ms", "is", null)
          .where("expires_at_ms", ">=", now)
          .returning("token")
          .executeTakeFirst();
        if (!claimed) throw new ConnectError("invalid or expired token", Code.Unauthenticated);
        await trx.insertInto("authorized_keys").values({
          fingerprint: fp, public_key: pubkey, label: req.label, added_at: now,
        }).onConflict((oc) => oc.column("fingerprint").doUpdateSet({ label: req.label })).execute();
      });
      refreshJwtKey(deps.jwtCache, fp);
      log.info("auth.connect", "browser_redeemed", { fp, label: req.label });
      return create(AuthRedeemBrowserResponseSchema, { fingerprint: fp, label: req.label });
    },
    // ─── pair ──────────────────────────────────────────────────────────
    async pairCreate(req, _ctx) {
      // public
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
      const inserted = await sql`
        INSERT INTO pair_requests (
          id, ephemeral_id, public_key, label, status, created_at_ms, decided_at_ms
        )
        SELECT ${id}, ${ephemeral_id}, ${pubkey}, ${req.label}, 'pending', ${now}, NULL
        WHERE NOT EXISTS (
          SELECT 1 FROM authorized_key_revocations WHERE fingerprint = ${proposedFp}
        )
      `.execute(deps.db);
      if (inserted.numAffectedRows !== 1n) {
        throw new ConnectError("authorized key was revoked", Code.PermissionDenied);
      }
      pairBus.publish({ kind: "pending", ephemeral_id, label: req.label, created_at_ms: now });
      log.info("pair.connect", "created", { ephemeral_id, label: req.label });
      return create(PairCreateResponseSchema, { ephemeralId: ephemeral_id });
    },

    async pairPoll(req, _ctx) {
      // public
      const row = await deps.db.selectFrom("pair_requests").select("status")
        .where("ephemeral_id", "=", req.ephemeralId).executeTakeFirst();
      if (!row) throw new ConnectError("not found", Code.NotFound);
      return create(PairPollResponseSchema, { status: row.status as string });
    },

    // pairList/pairApprove/pairDeny: authed browser (notifier click) OR a
    // LOOPBACK caller — the on-host agent/CLI approves devices via API
    // (Author 2026-07-11 "approve new devices via API"). Deliberately NOT
    // tailnet-wide: a tailnet grant would let a device pairCreate + self-
    // approve past the approval gate. authAuthorizeBrowser's tailnet
    // self-register is the one deliberate wide door; this stays tight.
    async pairList(_req, ctx) {
      if (!optionalAuth(ctx.values)) assertOnHost(callerOrigin(ctx.values));
      const rows = await deps.db.selectFrom("pair_requests")
        .select(["ephemeral_id", "label", "created_at_ms"])
        .where("status", "=", "pending").execute();
      return create(PairListResponseSchema, {
        requests: rows.map(r => create(PairRequestSchema, {
          ephemeralId: r.ephemeral_id, label: r.label,
          createdAtMs: BigInt(r.created_at_ms),
        })),
      });
    },

    async pairApprove(req, ctx) {
      const caller = optionalAuth(ctx.values);
      if (!caller) assertOnHost(callerOrigin(ctx.values));
      const now = Date.now();
      let approvedFp = "";
      await deps.db.transaction().execute(async (trx) => {
        let update = trx.updateTable("pair_requests")
          .set({ status: "approved", decided_at_ms: now })
          .where("ephemeral_id", "=", req.ephemeralId)
          .where("status", "=", "pending");
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
          fingerprint: fp, public_key: pubkey, label: row.label, added_at: now,
        }).onConflict((oc) => oc.column("fingerprint").doUpdateSet({ label: row.label })).execute();
        approvedFp = fp;
      });
      refreshJwtKey(deps.jwtCache, approvedFp);
      pairBus.publish({ kind: "removed", ephemeral_id: req.ephemeralId });
      log.info("pair.connect", "approved", { ephemeral_id: req.ephemeralId, fp: approvedFp });
      return create(PairApproveResponseSchema, { ok: true });
    },

    async pairDeny(req, ctx) {
      if (!optionalAuth(ctx.values)) assertOnHost(callerOrigin(ctx.values));
      const result = await deps.db.updateTable("pair_requests")
        .set({ status: "denied", decided_at_ms: Date.now() })
        .where("ephemeral_id", "=", req.ephemeralId)
        .where("status", "=", "pending").returningAll().executeTakeFirst();
      if (!result) throw new ConnectError("not found", Code.NotFound);
      pairBus.publish({ kind: "removed", ephemeral_id: req.ephemeralId });
      log.info("pair.connect", "denied", { ephemeral_id: req.ephemeralId });
      return create(PairDenyResponseSchema, { ok: true });
    },

    async devicesList(_req, ctx) {
      const caller = requireAuth(ctx.values);
      const [keys, workers] = await Promise.all([
        deps.db.selectFrom("authorized_keys").selectAll()
          .orderBy("added_at", "desc").execute(),
        deps.db.selectFrom("workers").select("fp").execute(),
      ]);
      const workerFps = new Set(workers.map((worker) => worker.fp));
      return create(DevicesListResponseSchema, {
        devices: keys
          .filter((key) => !workerFps.has(key.fingerprint))
          .map((key) => create(DeviceRowSchema, {
            fingerprint: key.fingerprint,
            label: key.label,
            addedAtMs: BigInt(key.added_at),
            isSelf: key.fingerprint === caller.fingerprint,
          })),
      });
    },

    async devicesRevoke(req, ctx) {
      const caller = optionalAuth(ctx.values);
      if (caller?.fingerprint === req.fingerprint) {
        throw new ConnectError("use key rotation to revoke this device", Code.InvalidArgument);
      }
      if (!caller) assertOnHost(callerOrigin(ctx.values));
      const now = Date.now();
      await deps.db.transaction().execute(async (trx) => {
        const target = await trx.selectFrom("authorized_keys").select("fingerprint")
          .where("fingerprint", "=", req.fingerprint).executeTakeFirst();
        if (!target) throw new ConnectError("device not found", Code.NotFound);
        const worker = await trx.selectFrom("workers").select("fp")
          .where("fp", "=", req.fingerprint).executeTakeFirst();
        if (worker) throw new ConnectError("workers must be deleted through WorkersDelete", Code.InvalidArgument);
        await trx.insertInto("authorized_key_revocations").values({
          fingerprint: req.fingerprint,
          revoked_at_ms: now,
          revoked_by_fp: caller?.fingerprint ?? "on-host-recovery",
          reason: "device-revoked",
        }).execute();
        await trx.deleteFrom("bootstrap_tokens")
          .where("used_at_ms", "is", null)
          .where((eb) => eb.or([
            eb("minted_by_fp", "=", req.fingerprint),
            eb("minted_by_fp", "is", null),
          ]))
          .execute();
        await trx.deleteFrom("authorized_keys")
          .where("fingerprint", "=", req.fingerprint).execute();
      });
      invalidateJwtKey(deps.jwtCache, req.fingerprint);
      deps.onKeyRevoked?.(req.fingerprint);
      return create(DevicesRevokeResponseSchema, { ok: true });
    },

    async devicesRotateCurrent(req, ctx) {
      const caller = requireAuth(ctx.values);
      const pubkey = decodeEd25519Pubkey(req.sshPubkeyB64);
      if (!pubkey) throw new ConnectError("invalid ssh_pubkey_b64", Code.InvalidArgument);
      const fingerprint = await fingerprintOf(pubkey);
      if (fingerprint === caller.fingerprint) {
        throw new ConnectError("new key matches current key", Code.InvalidArgument);
      }
      if (await isAuthorizedKeyRevoked(deps.db, fingerprint)) {
        throw new ConnectError("new key was previously revoked", Code.PermissionDenied);
      }
      const collision = await deps.db.selectFrom("authorized_keys").select("fingerprint")
        .where("fingerprint", "=", fingerprint).executeTakeFirst();
      const workerCollision = await deps.db.selectFrom("workers").select("fp")
        .where("fp", "=", fingerprint).executeTakeFirst();
      if (collision || workerCollision) {
        throw new ConnectError("new key is already in use", Code.AlreadyExists);
      }
      const now = Date.now();
      await deps.db.transaction().execute(async (trx) => {
        await trx.insertInto("authorized_keys").values({
          fingerprint, public_key: pubkey, label: req.label, added_at: now,
        }).execute();
        await trx.insertInto("authorized_key_revocations").values({
          fingerprint: caller.fingerprint,
          revoked_at_ms: now,
          revoked_by_fp: caller.fingerprint,
          reason: "device-rotated",
        }).execute();
        await trx.deleteFrom("bootstrap_tokens")
          .where("used_at_ms", "is", null)
          .where((eb) => eb.or([
            eb("minted_by_fp", "=", caller.fingerprint),
            eb("minted_by_fp", "is", null),
          ]))
          .execute();
        await trx.deleteFrom("authorized_keys")
          .where("fingerprint", "=", caller.fingerprint).execute();
      });
      refreshJwtKey(deps.jwtCache, fingerprint);
      invalidateJwtKey(deps.jwtCache, caller.fingerprint);
      deps.onKeyRevoked?.(caller.fingerprint);
      return create(DevicesRotateCurrentResponseSchema, { fingerprint });
    },
  };
}

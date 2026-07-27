// Auth + device-pairing RPC handlers: coord identity, browser/worker key
// authorization, bootstrap-token mint/redeem, and the pairing flow
// (create/poll/list/approve/deny). Spread into router.ts's single
// router.service() literal. Split out of router.ts (400-line cap).

import type { ServiceImpl } from "@connectrpc/connect";
import { Code, ConnectError } from "@connectrpc/connect";
import { create, fromBinary, toBinary } from "@bufbuild/protobuf";
import { randomUUID } from "node:crypto";
import { log } from "@roost/shared/log";
import {
  CoordinatorService,
  AuthCoordIdentityRequestSchema, AuthCoordIdentityResponseSchema,
  AuthAuthorizeBrowserResponseSchema,
  AuthMintBootstrapResponseSchema, AuthRedeemWorkerResponseSchema, AuthRedeemBrowserResponseSchema,
  AuthMintCoordinatorRelocationRequestSchema, AuthMintCoordinatorRelocationResponseSchema,
  AuthRedeemCoordinatorRelocationResponseSchema,
  PairCreateResponseSchema, PairPollResponseSchema, PairListResponseSchema,
  PairApproveResponseSchema, PairDenyResponseSchema,
} from "@roost/shared/proto/coordinator_pb";
import { PairRequestSchema } from "@roost/shared/proto/wire_pb";
import { requireAuth, optionalAuth, remoteAddressKey, authorizationKey } from "./auth-interceptor.ts";
import { fingerprintOf } from "../jwt.ts";
import { decodeEd25519Pubkey } from "../authorized-keys.ts";
import { assertLoopback, assertLoopbackOrTailnet } from "../middleware/loopback-only.ts";
import { COORD_GIT_SHA } from "../git-sha.ts";
import { randomToken } from "./router-helpers.ts";
import { _invalidateLabel } from "./viewer-tracker.ts";
import { pairBus } from "../buses.ts";
import type { ConnectDeps } from "./router.ts";

type AuthMethods =
  | "authCoordIdentity" | "authAuthorizeBrowser" | "authMintBootstrap"
  | "authRedeemWorker" | "authRedeemBrowser"
  | "authMintCoordinatorRelocation" | "authRedeemCoordinatorRelocation"
  | "pairCreate" | "pairPoll" | "pairList" | "pairApprove" | "pairDeny";

const CONNECT_COORDINATOR_PATH = "/roost.v1.CoordinatorService/";

function relocationErrorCode(status: number): Code {
  if (status === 401) return Code.Unauthenticated;
  if (status === 403) return Code.PermissionDenied;
  if (status === 404) return Code.NotFound;
  return Code.FailedPrecondition;
}

async function postCoordinatorRpc(
  baseUrl: string,
  method: "AuthCoordIdentity" | "AuthMintCoordinatorRelocation",
  body: Uint8Array,
  authorization?: string,
): Promise<Uint8Array> {
  const url = new URL(`${CONNECT_COORDINATOR_PATH}${method}`, baseUrl);
  const response = await fetch(url, {
    method: "POST",
    headers: {
      "content-type": "application/proto",
      "connect-protocol-version": "1",
      ...(authorization ? { authorization } : {}),
    },
    body: body.buffer.slice(body.byteOffset, body.byteOffset + body.byteLength) as ArrayBuffer,
    signal: AbortSignal.timeout(5_000),
  });
  if (!response.ok) {
    const detail = await response.text();
    throw new ConnectError(detail || `coordinator relocation request failed (${response.status})`, relocationErrorCode(response.status));
  }
  return new Uint8Array(await response.arrayBuffer());
}

async function resolveCurrentCoordinator(initialUrl: string): Promise<{ url: string; handoffId: string }> {
  const seen = new Set<string>();
  let url = initialUrl;
  for (let hop = 0; hop < 8; hop++) {
    const parsed = new URL(url);
    if (parsed.protocol !== "https:" || parsed.username || parsed.password) {
      throw new ConnectError("invalid coordinator relocation target", Code.FailedPrecondition);
    }
    const origin = parsed.origin;
    if (seen.has(origin)) throw new ConnectError("coordinator relocation cycle detected", Code.FailedPrecondition);
    seen.add(origin);
    const response = fromBinary(
      AuthCoordIdentityResponseSchema,
      await postCoordinatorRpc(origin, "AuthCoordIdentity", toBinary(AuthCoordIdentityRequestSchema, create(AuthCoordIdentityRequestSchema))),
    );
    if (!response.relocatedToUrl) {
      if (!response.handoffId) throw new ConnectError("current coordinator has no relocation handoff", Code.FailedPrecondition);
      return { url: origin, handoffId: response.handoffId };
    }
    url = response.relocatedToUrl;
  }
  throw new ConnectError("coordinator relocation exceeded eight hops", Code.FailedPrecondition);
}

export function makeAuthHandlers(
  deps: ConnectDeps,
): Pick<ServiceImpl<typeof CoordinatorService>, AuthMethods> {
  return {
    // ─── auth ──────────────────────────────────────────────────────────
    async authCoordIdentity(_req, _ctx) {
      // public
      const handoff = deps.move?.current();
      return create(AuthCoordIdentityResponseSchema, {
        fingerprintHex: deps.coordKey.verifyingKeyKid(),
        gitSha: COORD_GIT_SHA,
        publicUrl: deps.cfg.publicUrl ?? "",
        relocatedToUrl: handoff?.role === "SOURCE" && handoff.phase === "COMMITTED" ? handoff.target_url : undefined,
        handoffId: handoff?.handoff_id,
      });
    },

    async authAuthorizeBrowser(req, ctx) {
      // public, loopback OR tailnet peer (the tailnet is the trust boundary —
      // a fresh phone browser self-registers over the FQDN with no prior cred).
      const remote = ctx.values.get(remoteAddressKey);
      assertLoopbackOrTailnet(remote);
      const pubkey = decodeEd25519Pubkey(req.sshPubkeyB64);
      if (!pubkey) throw new ConnectError("invalid ssh_pubkey_b64", Code.InvalidArgument);
      const fp = await fingerprintOf(pubkey);
      await deps.db.insertInto("authorized_keys").values({
        fingerprint: fp, public_key: pubkey, label: req.label, added_at: Date.now(),
      }).onConflict((oc) => oc.column("fingerprint").doUpdateSet({ label: req.label })).execute();
      _invalidateLabel(fp);
      log.info("auth.connect", "browser_authorized", { fp, label: req.label });
      return create(AuthAuthorizeBrowserResponseSchema, { fingerprint: fp });
    },

    async authMintBootstrap(req, ctx) {
      requireAuth(ctx.values);
      const token = randomToken("roost_bt_", 24);
      const now = Date.now();
      const expires_at_ms = now + 24 * 60 * 60 * 1000;
      await deps.db.insertInto("bootstrap_tokens").values({
        token, kind: req.kind as any, label: req.label,
        created_at_ms: now, expires_at_ms,
        used_at_ms: null, used_by_fp: null,
      }).execute();
      log.info("auth.connect", "bootstrap_minted", { kind: req.kind, label: req.label });
      return create(AuthMintBootstrapResponseSchema, { token, expiresAtMs: BigInt(expires_at_ms) });
    },

    async authRedeemWorker(req, _ctx) {
      // public
      const tokenRow = await deps.db.selectFrom("bootstrap_tokens").selectAll()
        .where("token", "=", req.token).where("kind", "=", "worker").executeTakeFirst();
      if (!tokenRow) throw new ConnectError("invalid token", Code.Unauthenticated);
      if (tokenRow.used_at_ms !== null) throw new ConnectError("token already used", Code.Unauthenticated);
      if (tokenRow.expires_at_ms < Date.now()) throw new ConnectError("token expired", Code.Unauthenticated);
      const pubkey = decodeEd25519Pubkey(req.sshPubkeyB64);
      if (!pubkey) throw new ConnectError("invalid ssh_pubkey_b64", Code.InvalidArgument);
      const fp = await fingerprintOf(pubkey);
      const now = Date.now();
      await deps.db.transaction().execute(async (trx) => {
        await trx.updateTable("bootstrap_tokens").set({ used_at_ms: now, used_by_fp: fp })
          .where("token", "=", req.token).execute();
        await trx.insertInto("authorized_keys").values({
          fingerprint: fp, public_key: pubkey, label: req.label, added_at: now,
        }).onConflict((oc) => oc.column("fingerprint").doUpdateSet({ label: req.label })).execute();
        await trx.insertInto("workers").values({
          fp, label: req.label, os: req.os as any,
          git_sha: req.gitSha ?? null, host_metrics_json: null,
          registered_at_ms: now, last_seen_ms: now,
        }).onConflict((oc) => oc.column("fp").doUpdateSet({
          label: req.label, os: req.os as any,
          git_sha: req.gitSha ?? null, last_seen_ms: now,
        })).execute();
      });
      log.info("auth.connect", "worker_redeemed", { fp, label: req.label });
      return create(AuthRedeemWorkerResponseSchema, {
        fingerprint: fp, label: req.label,
        coordVerifyingKeyB64: deps.coordKey.verifyingKeyB64(),
      });
    },

    async authRedeemBrowser(req, _ctx) {
      // public
      const tokenRow = await deps.db.selectFrom("bootstrap_tokens").selectAll()
        .where("token", "=", req.token).where("kind", "=", "browser").executeTakeFirst();
      if (!tokenRow) throw new ConnectError("invalid token", Code.Unauthenticated);
      if (tokenRow.used_at_ms !== null) throw new ConnectError("token already used", Code.Unauthenticated);
      if (tokenRow.expires_at_ms < Date.now()) throw new ConnectError("token expired", Code.Unauthenticated);
      const pubkey = decodeEd25519Pubkey(req.sshPubkeyB64);
      if (!pubkey) throw new ConnectError("invalid ssh_pubkey_b64", Code.InvalidArgument);
      const fp = await fingerprintOf(pubkey);
      const now = Date.now();
      await deps.db.transaction().execute(async (trx) => {
        await trx.updateTable("bootstrap_tokens").set({ used_at_ms: now, used_by_fp: fp })
          .where("token", "=", req.token).execute();
        await trx.insertInto("authorized_keys").values({
          fingerprint: fp, public_key: pubkey, label: req.label, added_at: now,
        }).onConflict((oc) => oc.column("fingerprint").doUpdateSet({ label: req.label })).execute();
      });
      log.info("auth.connect", "browser_redeemed", { fp, label: req.label });
      return create(AuthRedeemBrowserResponseSchema, { fingerprint: fp, label: req.label });
    },

    async authMintCoordinatorRelocation(req, ctx) {
      const handoff = deps.move?.current();
      const sourceCommitted = handoff?.role === "SOURCE" && handoff.phase === "COMMITTED";
      if (sourceCommitted) {
        if (handoff.handoff_id !== req.handoffId) {
          throw new ConnectError("coordinator relocation is not available", Code.FailedPrecondition);
        }
        const authorization = ctx.values.get(authorizationKey);
        if (!authorization) throw new ConnectError("authentication required", Code.Unauthenticated);
        const current = await resolveCurrentCoordinator(handoff.target_url);
        const minted = fromBinary(
          AuthMintCoordinatorRelocationResponseSchema,
          await postCoordinatorRpc(
            current.url,
            "AuthMintCoordinatorRelocation",
            toBinary(
              AuthMintCoordinatorRelocationRequestSchema,
              create(AuthMintCoordinatorRelocationRequestSchema, { handoffId: current.handoffId }),
            ),
            authorization,
          ),
        );
        return create(AuthMintCoordinatorRelocationResponseSchema, {
          token: minted.token,
          targetUrl: minted.targetUrl,
        });
      }
      const caller = requireAuth(ctx.values);
      if (!handoff || handoff.handoff_id !== req.handoffId || deps.move?.gate.mode !== "active") {
        throw new ConnectError("coordinator relocation is not available", Code.FailedPrecondition);
      }
      const now = Math.floor(Date.now() / 1000);
      const token = await deps.coordKey.sign({
        // 60s, not 300s: this is an unbound bearer credential that enrolls an
        // arbitrary ed25519 key into the target's authorized_keys, and the
        // fragment→redeem hop is a single navigation. It cannot be bound to
        // claims.sub — source and target are different origins, so the browser
        // necessarily mints a fresh keypair on the destination.
        aud: "roost-coordinator-relocation", sub: caller.fingerprint, iat: now, exp: now + 60,
        handoff_id: handoff.handoff_id, target_url: handoff.target_url, jti: randomUUID(),
      });
      return create(AuthMintCoordinatorRelocationResponseSchema, { token, targetUrl: handoff.target_url });
    },

    async authRedeemCoordinatorRelocation(req, _ctx) {
      const handoff = deps.move?.current();
      if (!handoff || handoff.role !== "TARGET" || handoff.phase !== "COMMITTED") {
        throw new ConnectError("coordinator relocation is not committed", Code.FailedPrecondition);
      }
      let claims;
      try { claims = await deps.coordKey.verifyRelocation(req.token); }
      catch (error) { throw new ConnectError((error as Error).message, Code.Unauthenticated); }
      if (claims.handoff_id !== handoff.handoff_id || claims.target_url !== handoff.target_url) {
        throw new ConnectError("relocation token targets a different coordinator", Code.Unauthenticated);
      }
      const pubkey = decodeEd25519Pubkey(req.sshPubkeyB64);
      if (!pubkey) throw new ConnectError("invalid ssh_pubkey_b64", Code.InvalidArgument);
      const fingerprint = await fingerprintOf(pubkey);
      const now = Date.now();
      try {
        await deps.db.transaction().execute(async (trx) => {
          await trx.insertInto("bootstrap_tokens").values({
            token: `roost_move_${claims.jti}`, kind: "browser", label: req.label,
            created_at_ms: now, expires_at_ms: claims.exp * 1000, used_at_ms: now, used_by_fp: fingerprint,
          }).execute();
          await trx.insertInto("authorized_keys").values({
            fingerprint, public_key: pubkey, label: req.label, added_at: now,
          }).onConflict((oc) => oc.column("fingerprint").doUpdateSet({ label: req.label })).execute();
        });
      } catch (error) {
        // Only a bootstrap_tokens.token collision means "already used". The
        // target is concurrently running replayCommittedWorkers, so SQLITE_BUSY
        // is likely — reporting that as Unauthenticated silently unpairs a
        // browser that did nothing wrong.
        const message = String((error as Error)?.message ?? error);
        if (/UNIQUE|PRIMARY KEY|constraint failed: bootstrap_tokens/i.test(message)) {
          throw new ConnectError("relocation token already used", Code.Unauthenticated);
        }
        throw new ConnectError(`relocation redeem failed: ${message}`, Code.Unavailable);
      }
      // The upsert above rewrote the label; without this the new coordinator
      // shows a stale presence label for up to CACHE_TTL_MS.
      _invalidateLabel(fingerprint);
      return create(AuthRedeemCoordinatorRelocationResponseSchema, { fingerprint, label: req.label });
    },

    // ─── pair ──────────────────────────────────────────────────────────
    async pairCreate(req, _ctx) {
      // public
      const pubkey = decodeEd25519Pubkey(req.sshPubkeyB64);
      if (!pubkey) throw new ConnectError("invalid ssh_pubkey_b64", Code.InvalidArgument);
      const id = randomUUID();
      const ephBuf = new Uint8Array(16);
      crypto.getRandomValues(ephBuf);
      const ephemeral_id = Array.from(ephBuf).map(b => b.toString(16).padStart(2, "0")).join("");
      const now = Date.now();
      await deps.db.insertInto("pair_requests").values({
        id, ephemeral_id, public_key: pubkey, label: req.label, status: "pending", created_at_ms: now, decided_at_ms: null,
      }).execute();
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
      if (!optionalAuth(ctx.values)) assertLoopback(ctx.values.get(remoteAddressKey));
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
      if (!optionalAuth(ctx.values)) assertLoopback(ctx.values.get(remoteAddressKey));
      const row = await deps.db.selectFrom("pair_requests").selectAll()
        .where("ephemeral_id", "=", req.ephemeralId)
        .where("status", "=", "pending").executeTakeFirst();
      if (!row) throw new ConnectError("not found", Code.NotFound);
      const pubkey = row.public_key instanceof Uint8Array ? row.public_key : new Uint8Array(row.public_key);
      const fp = await fingerprintOf(pubkey);
      const now = Date.now();
      await deps.db.transaction().execute(async (trx) => {
        await trx.updateTable("pair_requests").set({ status: "approved", decided_at_ms: now })
          .where("ephemeral_id", "=", req.ephemeralId).execute();
        await trx.insertInto("authorized_keys").values({
          fingerprint: fp, public_key: pubkey, label: row.label, added_at: now,
        }).onConflict((oc) => oc.column("fingerprint").doUpdateSet({ label: row.label })).execute();
      });
      _invalidateLabel(fp);
      pairBus.publish({ kind: "removed", ephemeral_id: req.ephemeralId });
      log.info("pair.connect", "approved", { ephemeral_id: req.ephemeralId, fp });
      return create(PairApproveResponseSchema, { ok: true });
    },

    async pairDeny(req, ctx) {
      if (!optionalAuth(ctx.values)) assertLoopback(ctx.values.get(remoteAddressKey));
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

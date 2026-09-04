// Coordinator-relocation RPCs: the browser-facing mint (source side, issues a
// 60s unbound bearer bound to the handoff) and redeem (target side, enrolls the
// presented key iff the delegator key is authorized AND unrevoked). The
// follower-chain walk lives here too because both sides need it: a relocated
// cluster is a redirect chain, and every hop re-checks HTTPS + cycle-freedom.
// Redeem's INSERT..SELECT..WHERE-EXISTS makes "delegator revoked mid-flight"
// fail closed inside the token-claim transaction — do not weaken it to a
// read-then-write.

import type { ServiceImpl } from "@connectrpc/connect";
import { Code, ConnectError } from "@connectrpc/connect";
import { create, fromBinary, toBinary } from "@bufbuild/protobuf";
import { sql } from "kysely";
import { randomUUID } from "node:crypto";
import {
  CoordinatorService,
  AuthCoordIdentityRequestSchema, AuthCoordIdentityResponseSchema,
  AuthMintCoordinatorRelocationRequestSchema, AuthMintCoordinatorRelocationResponseSchema,
  AuthRedeemCoordinatorRelocationResponseSchema,
} from "@roost/shared/proto/coordinator_pb";
import { requireAccountDevice, authorizationKey } from "./auth-interceptor.ts";
import { fingerprintOf } from "@roost/shared/fingerprint";
import { decodeEd25519Pubkey } from "../authorized-keys.ts";
import { refreshJwtKey } from "../jwt.ts";
import { log } from "@roost/shared/log";
import type { ConnectDeps } from "./router.ts";

type RelocationMethods =
  | "authMintCoordinatorRelocation" | "authRedeemCoordinatorRelocation";

const CONNECT_COORDINATOR_PATH = "/roost.v1.CoordinatorService/";

function relocationErrorCode(status: number): Code {
  if (status === 401) return Code.Unauthenticated;
  if (status === 403) return Code.PermissionDenied;
  if (status === 404) return Code.NotFound;
  return Code.FailedPrecondition;
}

function invalidRelocationToken(): never {
  throw new ConnectError("invalid or expired relocation token", Code.Unauthenticated);
}

function publicKeysEqual(left: Uint8Array, right: Uint8Array): boolean {
  if (left.byteLength !== right.byteLength) return false;
  for (let index = 0; index < left.byteLength; index += 1) {
    if (left[index] !== right[index]) return false;
  }
  return true;
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

export function makeRelocationHandlers(
  deps: ConnectDeps,
): Pick<ServiceImpl<typeof CoordinatorService>, RelocationMethods> {
  return {
    async authMintCoordinatorRelocation(req, ctx) {
      const caller = requireAccountDevice(ctx.values);
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
      const pubkey = decodeEd25519Pubkey(req.sshPubkeyB64);
      if (!pubkey) throw new ConnectError("invalid ssh_pubkey_b64", Code.InvalidArgument);
      const fingerprint = await fingerprintOf(pubkey);

      const handoff = deps.move?.current();
      if (!handoff || handoff.role !== "TARGET" || handoff.phase !== "COMMITTED") {
        invalidRelocationToken();
      }
      let claims: {
        aud: string;
        sub: string;
        iat: number;
        exp: number;
        handoff_id: string;
        target_url: string;
        jti: string;
      };
      try {
        claims = await deps.coordKey.verifyRelocation(req.token);
      } catch {
        invalidRelocationToken();
      }
      const now = Date.now();
      const expiresAtMs = claims.exp * 1_000;
      if (
        claims.handoff_id !== handoff.handoff_id
        || claims.target_url !== handoff.target_url
        || claims.jti.length === 0
        || claims.sub.length === 0
        || !Number.isSafeInteger(expiresAtMs)
        || expiresAtMs <= now
      ) {
        invalidRelocationToken();
      }
      try {
        await deps.db.transaction().execute(async (trx) => {
          const inserted = await sql<{ accountId: string }>`
            INSERT INTO coordinator_relocation_redemptions (
              jti, account_id, redeemed_at_ms, expires_at_ms,
              used_by_fp, delegated_by_fp
            )
            SELECT ${claims.jti}, delegator_device.account_id, ${now}, ${expiresAtMs},
                   ${fingerprint}, ${claims.sub}
            FROM authorized_keys AS delegator_key
            JOIN account_devices AS delegator_device
              ON delegator_device.fingerprint = delegator_key.fingerprint
            JOIN accounts AS delegator_account
              ON delegator_account.id = delegator_device.account_id
            WHERE delegator_key.fingerprint = ${claims.sub}
              AND delegator_account.status = 'active'
              AND NOT EXISTS (
                SELECT 1
                FROM authorized_key_revocations AS delegator_revocation
                WHERE delegator_revocation.fingerprint = delegator_key.fingerprint
              )
            ON CONFLICT(jti) DO NOTHING
            RETURNING account_id AS accountId
          `.execute(trx);
          const redemption = inserted.rows[0];
          if (!redemption) invalidRelocationToken();

          const revocation = await trx.selectFrom("authorized_key_revocations")
            .select("fingerprint")
            .where("fingerprint", "=", fingerprint)
            .executeTakeFirst();
          if (revocation) invalidRelocationToken();

          const worker = await trx.selectFrom("workers")
            .select("fp")
            .where("fp", "=", fingerprint)
            .executeTakeFirst();
          if (worker) invalidRelocationToken();

          const existingKey = await trx.selectFrom("authorized_keys")
            .select("public_key")
            .where("fingerprint", "=", fingerprint)
            .executeTakeFirst();
          const existingDevice = await trx.selectFrom("account_devices")
            .select("account_id")
            .where("fingerprint", "=", fingerprint)
            .executeTakeFirst();
          if (
            Boolean(existingKey) !== Boolean(existingDevice)
            || (
              existingKey
              && existingDevice
              && (
                !publicKeysEqual(existingKey.public_key, pubkey)
                || existingDevice.account_id !== redemption.accountId
              )
            )
          ) {
            invalidRelocationToken();
          }

          await trx.insertInto("authorized_keys").values({
            fingerprint,
            public_key: pubkey,
            label: req.label,
            added_at: now,
          }).onConflict((oc) => oc.column("fingerprint").doUpdateSet({
            label: req.label,
          })).execute();
          await trx.insertInto("account_devices").values({
            fingerprint,
            account_id: redemption.accountId,
            added_at_ms: now,
            last_seen_at_ms: now,
          }).onConflict((oc) => oc.column("fingerprint").doUpdateSet({
            last_seen_at_ms: now,
          })).execute();
        });
      } catch (error) {
        if (error instanceof ConnectError) throw error;
        log.error("auth.connect", "relocation_redeem_failed", {
          fingerprint,
          error: String(error),
        });
        throw new ConnectError("relocation redemption unavailable", Code.Unavailable);
      }
      refreshJwtKey(deps.jwtCache, fingerprint);
      return create(AuthRedeemCoordinatorRelocationResponseSchema, { fingerprint, label: req.label });
    },
  };
}

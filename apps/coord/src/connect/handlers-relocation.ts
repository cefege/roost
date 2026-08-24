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
import { requireAuth, authorizationKey } from "./auth-interceptor.ts";
import { fingerprintOf } from "@roost/shared/fingerprint";
import { decodeEd25519Pubkey, isAuthorizedKeyRevoked } from "../authorized-keys.ts";
import { refreshJwtKey } from "../jwt.ts";
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
      try {
        claims = await deps.coordKey.verifyRelocation(req.token);
      } catch (error) {
        throw new ConnectError((error as Error).message, Code.Unauthenticated);
      }
      if (claims.handoff_id !== handoff.handoff_id || claims.target_url !== handoff.target_url) {
        throw new ConnectError("relocation token targets a different coordinator", Code.Unauthenticated);
      }
      const pubkey = decodeEd25519Pubkey(req.sshPubkeyB64);
      if (!pubkey) throw new ConnectError("invalid ssh_pubkey_b64", Code.InvalidArgument);
      const fingerprint = await fingerprintOf(pubkey);
      if (await isAuthorizedKeyRevoked(deps.db, fingerprint)) {
        throw new ConnectError("authorized key was revoked", Code.Unauthenticated);
      }
      const now = Date.now();
      try {
        await deps.db.transaction().execute(async (trx) => {
          const replay = await sql`
            INSERT INTO bootstrap_tokens (
              token, kind, label, created_at_ms, expires_at_ms,
              used_at_ms, used_by_fp, minted_by_fp
            )
            SELECT ${`roost_move_${claims.jti}`}, 'browser', ${req.label}, ${now},
                   ${claims.exp * 1000}, ${now}, ${fingerprint}, ${claims.sub}
            WHERE EXISTS (
              SELECT 1 FROM authorized_keys WHERE fingerprint = ${claims.sub}
            ) AND NOT EXISTS (
              SELECT 1 FROM authorized_key_revocations WHERE fingerprint = ${claims.sub}
            )
          `.execute(trx);
          if (replay.numAffectedRows !== 1n) {
            throw new ConnectError("relocation delegator is not authorized", Code.Unauthenticated);
          }
          await trx.insertInto("authorized_keys").values({
            fingerprint, public_key: pubkey, label: req.label, added_at: now,
          }).onConflict((oc) => oc.column("fingerprint").doUpdateSet({ label: req.label })).execute();
        });
        refreshJwtKey(deps.jwtCache, fingerprint);
      } catch (error) {
        if (error instanceof ConnectError) throw error;
        const message = String((error as Error)?.message ?? error);
        if (/UNIQUE|PRIMARY KEY|constraint failed: bootstrap_tokens/i.test(message)) {
          throw new ConnectError("relocation token already used", Code.Unauthenticated);
        }
        if (/bootstrap minter revoked|authorized key revoked/i.test(message)) {
          throw new ConnectError("relocation key was revoked", Code.Unauthenticated);
        }
        throw new ConnectError(`relocation redeem failed: ${message}`, Code.Unavailable);
      }
      return create(AuthRedeemCoordinatorRelocationResponseSchema, { fingerprint, label: req.label });
    },
  };
}

// Pairing handlers isolate the public request flow from authenticated approval.
// Approval persists the key and its account association atomically so a pending
// device cannot gain browser authority before an authorized decision commits.

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
import { pairBus } from "../buses.ts";
import { refreshJwtKey } from "../jwt.ts";
import { assertOnHost } from "../middleware/caller-origin.ts";
import {
  callerOrigin,
  optionalAccountDevice,
} from "./auth-interceptor.ts";
import type { ConnectDeps } from "./router.ts";
import {
  associateSelfHostedBrowser,
  rejectManagedLegacyBrowserAuth,
  selfHostedBrowserAccountId,
} from "./self-hosted-browser-auth.ts";

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
    async pairCreate(req, _ctx) {
      rejectManagedLegacyBrowserAuth(deps);
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
      rejectManagedLegacyBrowserAuth(deps);
      // public
      const row = await deps.db.selectFrom("pair_requests").select("status")
        .where("ephemeral_id", "=", req.ephemeralId).executeTakeFirst();
      if (!row) throw new ConnectError("not found", Code.NotFound);
      return create(PairPollResponseSchema, { status: row.status as string });
    },

    // pairList/pairApprove/pairDeny: an authenticated browser (notifier click)
    // or a direct on-host caller. A tailnet source address never grants
    // authority; otherwise a pending device could approve itself.
    async pairList(_req, ctx) {
      rejectManagedLegacyBrowserAuth(deps);
      if (!optionalAccountDevice(ctx.values)) assertOnHost(callerOrigin(ctx.values));
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
      rejectManagedLegacyBrowserAuth(deps);
      const caller = optionalAccountDevice(ctx.values);
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
        const accountId = caller?.kind === "account-device"
          ? caller.accountId
          : await selfHostedBrowserAccountId(trx, caller?.fingerprint);
        await associateSelfHostedBrowser(trx, fp, accountId, now);
        approvedFp = fp;
      });
      refreshJwtKey(deps.jwtCache, approvedFp);
      pairBus.publish({ kind: "removed", ephemeral_id: req.ephemeralId });
      log.info("pair.connect", "approved", { ephemeral_id: req.ephemeralId, fp: approvedFp });
      return create(PairApproveResponseSchema, { ok: true });
    },

    async pairDeny(req, ctx) {
      rejectManagedLegacyBrowserAuth(deps);
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

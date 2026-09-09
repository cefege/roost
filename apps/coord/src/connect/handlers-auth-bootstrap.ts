// Browser and worker bootstrap handlers stay together because token claims bind
// a public key to exactly one persisted principal kind in a single transaction.
// The auth handler facade combines these RPCs with pairing and device lifecycle.

import { create } from "@bufbuild/protobuf";
import { Code, ConnectError, type ServiceImpl } from "@connectrpc/connect";
import { fingerprintOf } from "@roost/shared/fingerprint";
import { log } from "@roost/shared/log";
import { isSupportedHostPlatform } from "@roost/shared/platform";
import {
  AuthCoordIdentityResponseSchema,
  AuthMintBootstrapResponseSchema,
  AuthRedeemBrowserResponseSchema,
  AuthRedeemWorkerResponseSchema,
  CoordinatorService,
} from "@roost/shared/proto/coordinator_pb";
import { decodeEd25519Pubkey } from "../authorized-keys.ts";
import {
  bootstrapTokenDigest,
  claimBootstrapToken,
  mintBootstrapToken,
} from "../bootstrap-tokens.ts";
import { COORD_GIT_SHA } from "../git-sha.ts";
import { refreshJwtKey } from "../jwt.ts";
import { truncatePersistedUtf8 } from "../persistence-input.ts";
import { requireAccountDevice } from "./auth-interceptor.ts";
import type { ConnectDeps } from "./router.ts";

type AuthBootstrapMethods =
  | "authCoordIdentity"
  | "authMintBootstrap"
  | "authRedeemWorker"
  | "authRedeemBrowser";



function invalidBootstrapToken(): never {
  throw new ConnectError("invalid or expired token", Code.Unauthenticated);
}

function publicKeysEqual(left: Uint8Array, right: Uint8Array): boolean {
  if (left.byteLength !== right.byteLength) return false;
  for (let index = 0; index < left.byteLength; index += 1) {
    if (left[index] !== right[index]) return false;
  }
  return true;
}

export function makeAuthBootstrapHandlers(
  deps: ConnectDeps,
): Pick<ServiceImpl<typeof CoordinatorService>, AuthBootstrapMethods> {
  return {
    // ─── auth ──────────────────────────────────────────────────────────
    async authCoordIdentity(_req, _ctx) {
      // public
      return create(AuthCoordIdentityResponseSchema, {
        gitSha: COORD_GIT_SHA,
        publicUrl: deps.cfg.publicUrl ?? "",
      });
    },


    async authMintBootstrap(req, ctx) {
      const caller = requireAccountDevice(ctx.values);
      if (req.kind !== "worker" && req.kind !== "browser") {
        throw new ConnectError("bootstrap kind must be worker or browser", Code.InvalidArgument);
      }
      const kind = req.kind === "worker" ? "worker" : "browser";
      const minted = await mintBootstrapToken(deps.db, {
        kind,
        label: req.label,
        accountId: deps.selfHostedTenant.accountId,
        dashboardId: deps.selfHostedTenant.dashboardId,
        mintedByFp: caller.fingerprint,
      });
      log.info("auth.connect", "bootstrap_minted", { kind });
      return create(AuthMintBootstrapResponseSchema, {
        token: minted.token,
        expiresAtMs: BigInt(minted.expiresAtMs),
      });
    },

    async authRedeemWorker(req, _ctx) {
      if (!isSupportedHostPlatform(req.os)) {
        throw new ConnectError("unsupported worker os", Code.InvalidArgument);
      }
      const pubkey = decodeEd25519Pubkey(req.sshPubkeyB64);
      if (!pubkey) throw new ConnectError("invalid ssh_pubkey_b64", Code.InvalidArgument);
      const fp = await fingerprintOf(pubkey);
      const tokenHash = await bootstrapTokenDigest(req.token);
      const now = Date.now();
      const label = truncatePersistedUtf8(req.label);
      const gitSha = req.gitSha === undefined
        ? null
        : truncatePersistedUtf8(req.gitSha);
      await deps.db.transaction().execute(async (trx) => {
        const claimed = await claimBootstrapToken(trx, {
          tokenHash,
          kind: "worker",
          fingerprint: fp,
          publicKey: pubkey,
          now,
        });
        if (!claimed) invalidBootstrapToken();

        const accountDevice = await trx.selectFrom("account_devices")
          .select("fingerprint")
          .where("fingerprint", "=", fp)
          .executeTakeFirst();
        if (accountDevice) invalidBootstrapToken();

        const authorizedKey = await trx.selectFrom("authorized_keys")
          .select("public_key")
          .where("fingerprint", "=", fp)
          .executeTakeFirst();
        const worker = await trx.selectFrom("workers")
          .select("fp")
          .where("fp", "=", fp)
          .executeTakeFirst();

        if (worker) {
          if (
            !authorizedKey
            || !publicKeysEqual(authorizedKey.public_key, pubkey)
          ) {
            invalidBootstrapToken();
          }
          await trx.updateTable("authorized_keys")
            .set({ label })
            .where("fingerprint", "=", fp)
            .execute();
          await trx.updateTable("workers")
            .set({
              label,
              os: req.os,
              git_sha: gitSha,
              last_seen_ms: now,
            })
            .where("fp", "=", fp)
            .execute();
        } else {
          if (authorizedKey) invalidBootstrapToken();
          await trx.insertInto("authorized_keys").values({
            fingerprint: fp,
            public_key: pubkey,
            label,
            added_at: now,
          }).execute();
          await trx.insertInto("workers").values({
            fp,
            dashboard_id: deps.selfHostedTenant.dashboardId,
            label,
            os: req.os,
            git_sha: gitSha,
            host_metrics_json: null,
            registered_at_ms: now,
            last_seen_ms: now,
          }).execute();
        }
      });
      refreshJwtKey(deps.jwtCache, fp);
      log.info("auth.connect", "worker_redeemed", { fp, label });
      return create(AuthRedeemWorkerResponseSchema, {
        fingerprint: fp,
        label,
      });
    },

    async authRedeemBrowser(req, _ctx) {
      const pubkey = decodeEd25519Pubkey(req.sshPubkeyB64);
      if (!pubkey) throw new ConnectError("invalid ssh_pubkey_b64", Code.InvalidArgument);
      const fp = await fingerprintOf(pubkey);
      const tokenHash = await bootstrapTokenDigest(req.token);
      const now = Date.now();
      await deps.db.transaction().execute(async (trx) => {
        const claimed = await claimBootstrapToken(trx, {
          tokenHash,
          kind: "browser",
          fingerprint: fp,
          publicKey: pubkey,
          now,
        });
        if (!claimed) invalidBootstrapToken();

        const worker = await trx.selectFrom("workers")
          .select("fp")
          .where("fp", "=", fp)
          .executeTakeFirst();
        if (worker) invalidBootstrapToken();

        const authorizedKey = await trx.selectFrom("authorized_keys")
          .select("public_key")
          .where("fingerprint", "=", fp)
          .executeTakeFirst();
        const accountDevice = await trx.selectFrom("account_devices")
          .select("account_id")
          .where("fingerprint", "=", fp)
          .executeTakeFirst();

        if (authorizedKey || accountDevice) {
          if (
            !authorizedKey
            || !accountDevice
            || accountDevice.account_id !== claimed.accountId
            || !publicKeysEqual(authorizedKey.public_key, pubkey)
          ) {
            invalidBootstrapToken();
          }
          await trx.updateTable("authorized_keys")
            .set({ label: req.label })
            .where("fingerprint", "=", fp)
            .execute();
          await trx.updateTable("account_devices")
            .set({ last_seen_at_ms: now })
            .where("fingerprint", "=", fp)
            .where("account_id", "=", claimed.accountId)
            .execute();
        } else {
          await trx.insertInto("authorized_keys").values({
            fingerprint: fp,
            public_key: pubkey,
            label: req.label,
            added_at: now,
          }).execute();
          await trx.insertInto("account_devices").values({
            fingerprint: fp,
            account_id: claimed.accountId,
            added_at_ms: now,
            last_seen_at_ms: now,
          }).execute();
        }
      });
      refreshJwtKey(deps.jwtCache, fp);
      log.info("auth.connect", "browser_redeemed", { fp, label: req.label });
      return create(AuthRedeemBrowserResponseSchema, {});
    },
  };
}

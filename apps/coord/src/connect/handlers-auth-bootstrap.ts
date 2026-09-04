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
  AuthDashboardAccessResponseSchema,
  AuthMintBootstrapResponseSchema,
  AuthRedeemBrowserResponseSchema,
  AuthRedeemWorkerResponseSchema,
  CoordinatorService,
  DashboardAccessSchema,
  OrganizationAccessSchema,
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
import {
  callerOrigin,
  getDashboardAccessSnapshot,
  requestedDashboardId,
  requireAccountDevice,
  requireDashboardActor,
  requireDashboardAdmin,
  type DashboardActor,
} from "./auth-interceptor.ts";
import type { ConnectDeps } from "./router.ts";
import { rejectManagedLegacyBrowserAuth } from "./self-hosted-browser-auth.ts";

type AuthBootstrapMethods =
  | "authCoordIdentity"
  | "authDashboardAccess"
  | "authMintBootstrap"
  | "authRedeemWorker"
  | "authRedeemBrowser";

function dashboardCapabilities(
  organizationRole: "owner" | "admin" | "member",
  dashboardRole: "admin" | "member",
): string[] {
  const capabilities = ["dashboard:member"];
  if (dashboardRole === "admin") capabilities.push("dashboard:admin");
  if (organizationRole === "owner" || organizationRole === "admin") {
    capabilities.push("organization:admin");
  }
  if (organizationRole === "owner") capabilities.push("organization:owner");
  return capabilities;
}

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
    async authCoordIdentity(_req, ctx) {
      // public
      const handoff = deps.move?.current();
      return create(AuthCoordIdentityResponseSchema, {
        gitSha: COORD_GIT_SHA,
        publicUrl: deps.cfg.publicUrl ?? "",
        relocatedToUrl: handoff?.role === "SOURCE" && handoff.phase === "COMMITTED" ? handoff.target_url : undefined,
        handoffId: handoff?.handoff_id,
        publicListener: callerOrigin(ctx.values).listener === "public-edge",
        saasMode: deps.cfg.saasMode,
        instanceId: deps.cfg.instanceId ?? "",
      });
    },

    async authDashboardAccess(_req, ctx) {
      const caller = requireAccountDevice(ctx.values);
      const snapshot = await getDashboardAccessSnapshot(deps.db, caller.fingerprint);
      if (!snapshot) throw new ConnectError("not found", Code.NotFound);
      const requested = requestedDashboardId(ctx.values);
      // The header is only a hint at bootstrap. A foreign or stale value cannot
      // select anything; return the first deterministic active membership.
      const selected = snapshot.dashboards.find((dashboard) => dashboard.id === requested)
        ?? snapshot.dashboards[0];
      return create(AuthDashboardAccessResponseSchema, {
        accountId: snapshot.accountId,
        organizations: snapshot.organizations.map((organization) => create(OrganizationAccessSchema, {
          id: organization.id,
          slug: organization.slug,
          name: organization.name,
          role: organization.role,
        })),
        dashboards: snapshot.dashboards.map((dashboard) => create(DashboardAccessSchema, {
          id: dashboard.id,
          organizationId: dashboard.organizationId,
          slug: dashboard.slug,
          name: dashboard.name,
          organizationRole: dashboard.organizationRole,
          dashboardRole: dashboard.dashboardRole,
        })),
        selectedDashboardId: selected?.id ?? "",
        capabilities: selected
          ? dashboardCapabilities(selected.organizationRole, selected.dashboardRole)
          : [],
      });
    },


    async authMintBootstrap(req, ctx) {
      requireAccountDevice(ctx.values);
      let actor: DashboardActor;
      if (deps.cfg.saasMode) {
        if (req.kind !== "worker") {
          throw new ConnectError("managed bootstrap kind must be worker", Code.InvalidArgument);
        }
        actor = requireDashboardAdmin(ctx.values);
      } else {
        if (req.kind !== "worker" && req.kind !== "browser") {
          throw new ConnectError("bootstrap kind must be worker or browser", Code.InvalidArgument);
        }
        actor = requireDashboardActor(ctx.values);
      }
      const kind = req.kind === "worker" ? "worker" : "browser";
      const minted = await mintBootstrapToken(deps.db, {
        kind,
        label: req.label,
        accountId: actor.accountId,
        dashboardId: actor.dashboardId,
        mintedByFp: actor.deviceFingerprint,
      });
      log.info("auth.connect", "bootstrap_minted", {
        kind,
        account_id: actor.accountId,
        dashboard_id: actor.dashboardId,
      });
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
          .select(["fp", "dashboard_id"])
          .where("fp", "=", fp)
          .executeTakeFirst();

        if (worker) {
          if (
            worker.dashboard_id !== claimed.dashboardId
            || !authorizedKey
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
            .where("dashboard_id", "=", claimed.dashboardId)
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
            dashboard_id: claimed.dashboardId,
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
      rejectManagedLegacyBrowserAuth(deps);
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

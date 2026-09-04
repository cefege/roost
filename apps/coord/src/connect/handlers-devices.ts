// Device lifecycle handlers keep listing, revocation, rotation, and logout under
// one ownership boundary because each mutation must revoke delegated authority
// and notify every affected dashboard only after the database transaction commits.

import { create } from "@bufbuild/protobuf";
import { Code, ConnectError, type ServiceImpl } from "@connectrpc/connect";
import { fingerprintOf } from "@roost/shared/fingerprint";
import {
  AuthLogoutResponseSchema,
  CoordinatorService,
  DeviceRowSchema,
  DevicesListResponseSchema,
  DevicesRevokeResponseSchema,
  DevicesRotateCurrentResponseSchema,
} from "@roost/shared/proto/coordinator_pb";
import { decodeEd25519Pubkey, isAuthorizedKeyRevoked } from "../authorized-keys.ts";
import { invalidateJwtKey, refreshJwtKey } from "../jwt.ts";
import { assertOnHost } from "../middleware/caller-origin.ts";
import {
  callerOrigin,
  optionalAccountDevice,
  requireAccountDevice,
} from "./auth-interceptor.ts";
import type { ConnectDeps } from "./router.ts";

type DeviceMethods =
  | "devicesList"
  | "devicesRevoke"
  | "devicesRotateCurrent"
  | "authLogout";

export function makeDeviceHandlers(
  deps: ConnectDeps,
): Pick<ServiceImpl<typeof CoordinatorService>, DeviceMethods> {
  return {
    async devicesList(_req, ctx) {
      const caller = requireAccountDevice(ctx.values);
      let keys: Array<{ fingerprint: string; label: string; added_at: number }>;
      let workerFps: Set<string> | undefined;
      if (deps.cfg.saasMode) {
        if (caller.kind !== "account-device") {
          throw new ConnectError("authentication required", Code.Unauthenticated);
        }
        keys = await deps.db
          .selectFrom("authorized_keys as key")
          .innerJoin("account_devices as device", "device.fingerprint", "key.fingerprint")
          .select(["key.fingerprint", "key.label", "key.added_at"])
          .where("device.account_id", "=", caller.accountId)
          .orderBy("key.added_at", "desc")
          .execute();
      } else {
        const [allKeys, workers] = await Promise.all([
          deps.db.selectFrom("authorized_keys")
            .select(["fingerprint", "label", "added_at"])
            .orderBy("added_at", "desc")
            .execute(),
          deps.db.selectFrom("workers").select("fp").execute(),
        ]);
        keys = allKeys;
        workerFps = new Set(workers.map((worker) => worker.fp));
      }
      return create(DevicesListResponseSchema, {
        devices: keys
          .filter((key) => !workerFps?.has(key.fingerprint))
          .map((key) => create(DeviceRowSchema, {
            fingerprint: key.fingerprint,
            label: key.label,
            addedAtMs: BigInt(key.added_at),
            isSelf: key.fingerprint === caller.fingerprint,
          })),
      });
    },

    async devicesRevoke(req, ctx) {
      const caller = deps.cfg.saasMode
        ? requireAccountDevice(ctx.values)
        : optionalAccountDevice(ctx.values);
      if (!caller) assertOnHost(callerOrigin(ctx.values));
      if (caller?.fingerprint === req.fingerprint) {
        throw new ConnectError("use key rotation to revoke this device", Code.InvalidArgument);
      }
      const managedAccountId = deps.cfg.saasMode && caller?.kind === "account-device"
        ? caller.accountId
        : null;
      if (deps.cfg.saasMode && managedAccountId === null) {
        throw new ConnectError("authentication required", Code.Unauthenticated);
      }

      const affectedDashboards = new Set<string>();
      const now = Date.now();
      await deps.db.transaction().execute(async (trx) => {
        const target = managedAccountId
          ? await trx
            .selectFrom("authorized_keys as key")
            .innerJoin("account_devices as device", "device.fingerprint", "key.fingerprint")
            .select("key.fingerprint")
            .where("key.fingerprint", "=", req.fingerprint)
            .where("device.account_id", "=", managedAccountId)
            .executeTakeFirst()
          : await trx.selectFrom("authorized_keys")
            .select("fingerprint")
            .where("fingerprint", "=", req.fingerprint)
            .executeTakeFirst();
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

        let delegatedTokens = trx.deleteFrom("bootstrap_tokens")
          .where("used_at_ms", "is", null);
        delegatedTokens = managedAccountId
          ? delegatedTokens.where("minted_by_fp", "=", req.fingerprint)
          : delegatedTokens.where((eb) => eb.or([
            eb("minted_by_fp", "=", req.fingerprint),
            eb("minted_by_fp", "is", null),
          ]));
        await delegatedTokens.execute();
        await trx.deleteFrom("push_subscriptions")
          .where("viewer_fp", "=", req.fingerprint)
          .execute();

        const accountDevices = managedAccountId
          ? [{ account_id: managedAccountId }]
          : await trx.selectFrom("account_devices")
            .select("account_id")
            .where("fingerprint", "=", req.fingerprint)
            .execute();
        if (accountDevices.length > 0) {
          const memberships = await trx.selectFrom("dashboard_memberships")
            .select("dashboard_id")
            .where("account_id", "in", accountDevices.map((device) => device.account_id))
            .execute();
          for (const membership of memberships) affectedDashboards.add(membership.dashboard_id);
        }

        let accountDeviceDelete = trx.deleteFrom("account_devices")
          .where("fingerprint", "=", req.fingerprint);
        if (managedAccountId) {
          accountDeviceDelete = accountDeviceDelete.where("account_id", "=", managedAccountId);
        }
        await accountDeviceDelete.execute();
        await trx.deleteFrom("authorized_keys")
          .where("fingerprint", "=", req.fingerprint)
          .execute();
      });

      invalidateJwtKey(deps.jwtCache, req.fingerprint);
      deps.onKeyRevoked?.(req.fingerprint);
      for (const dashboardId of affectedDashboards) {
        deps.onDashboardRevoked?.(dashboardId, req.fingerprint);
      }
      return create(DevicesRevokeResponseSchema, { ok: true });
    },

    async devicesRotateCurrent(req, ctx) {
      const caller = requireAccountDevice(ctx.values);
      if (deps.cfg.saasMode && caller.kind !== "account-device") {
        throw new ConnectError("authentication required", Code.Unauthenticated);
      }
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
        let accountDeviceQuery = trx.selectFrom("account_devices")
          .select("account_id")
          .where("fingerprint", "=", caller.fingerprint);
        if (caller.kind === "account-device") {
          accountDeviceQuery = accountDeviceQuery.where("account_id", "=", caller.accountId);
        }
        const accountDevice = await accountDeviceQuery.executeTakeFirst();
        if (deps.cfg.saasMode && !accountDevice) {
          throw new ConnectError("authentication required", Code.Unauthenticated);
        }

        await trx.insertInto("authorized_keys").values({
          fingerprint, public_key: pubkey, label: req.label, added_at: now,
        }).execute();
        if (accountDevice) {
          await trx.insertInto("account_devices").values({
            fingerprint,
            account_id: accountDevice.account_id,
            added_at_ms: now,
            last_seen_at_ms: now,
          }).execute();
        }
        await trx.insertInto("authorized_key_revocations").values({
          fingerprint: caller.fingerprint,
          revoked_at_ms: now,
          revoked_by_fp: caller.fingerprint,
          reason: "device-rotated",
        }).execute();

        let delegatedTokens = trx.deleteFrom("bootstrap_tokens")
          .where("used_at_ms", "is", null);
        delegatedTokens = deps.cfg.saasMode
          ? delegatedTokens.where("minted_by_fp", "=", caller.fingerprint)
          : delegatedTokens.where((eb) => eb.or([
            eb("minted_by_fp", "=", caller.fingerprint),
            eb("minted_by_fp", "is", null),
          ]));
        await delegatedTokens.execute();
        await trx.deleteFrom("push_subscriptions")
          .where("viewer_fp", "=", caller.fingerprint)
          .execute();
        await trx.deleteFrom("account_devices")
          .where("fingerprint", "=", caller.fingerprint)
          .execute();
        await trx.deleteFrom("authorized_keys")
          .where("fingerprint", "=", caller.fingerprint)
          .execute();
      });

      refreshJwtKey(deps.jwtCache, fingerprint);
      invalidateJwtKey(deps.jwtCache, caller.fingerprint);
      deps.onKeyRevoked?.(caller.fingerprint);
      return create(DevicesRotateCurrentResponseSchema, { fingerprint });
    },

    async authLogout(_req, ctx) {
      const caller = requireAccountDevice(ctx.values);
      const accountId = caller.kind === "account-device" ? caller.accountId : null;
      if (deps.cfg.saasMode && accountId === null) {
        throw new ConnectError("authentication required", Code.Unauthenticated);
      }

      const affectedDashboards = new Set<string>();
      const now = Date.now();
      await deps.db.transaction().execute(async (trx) => {
        const currentKey = accountId
          ? await trx
            .selectFrom("authorized_keys as key")
            .innerJoin("account_devices as device", "device.fingerprint", "key.fingerprint")
            .select("key.fingerprint")
            .where("key.fingerprint", "=", caller.fingerprint)
            .where("device.account_id", "=", accountId)
            .executeTakeFirst()
          : await trx.selectFrom("authorized_keys")
            .select("fingerprint")
            .where("fingerprint", "=", caller.fingerprint)
            .executeTakeFirst();
        if (!currentKey) {
          throw new ConnectError("authentication required", Code.Unauthenticated);
        }

        if (accountId) {
          const memberships = await trx.selectFrom("dashboard_memberships")
            .select("dashboard_id")
            .where("account_id", "=", accountId)
            .execute();
          for (const membership of memberships) affectedDashboards.add(membership.dashboard_id);
        }
        await trx.insertInto("authorized_key_revocations").values({
          fingerprint: caller.fingerprint,
          revoked_at_ms: now,
          revoked_by_fp: caller.fingerprint,
          reason: "browser-logout",
        }).execute();
        await trx.deleteFrom("bootstrap_tokens")
          .where("used_at_ms", "is", null)
          .where("minted_by_fp", "=", caller.fingerprint)
          .execute();
        await trx.deleteFrom("push_subscriptions")
          .where("viewer_fp", "=", caller.fingerprint)
          .execute();

        let deviceDelete = trx.deleteFrom("account_devices")
          .where("fingerprint", "=", caller.fingerprint);
        if (accountId) deviceDelete = deviceDelete.where("account_id", "=", accountId);
        await deviceDelete.execute();
        await trx.deleteFrom("authorized_keys")
          .where("fingerprint", "=", caller.fingerprint)
          .execute();
      });

      invalidateJwtKey(deps.jwtCache, caller.fingerprint);
      deps.onKeyRevoked?.(caller.fingerprint);
      for (const dashboardId of affectedDashboards) {
        deps.onDashboardRevoked?.(dashboardId, caller.fingerprint);
      }
      return create(AuthLogoutResponseSchema, { ok: true });
    },
  };
}

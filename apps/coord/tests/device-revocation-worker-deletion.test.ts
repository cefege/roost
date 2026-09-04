/**
 * Owns worker deletion rollback, tombstone, and connection-revocation coverage.
 * This split keeps the end-to-end deletion case readable as one coherent scenario.
 * It depends on the shared revocation fixture, worker registry, and channel map.
 */
import { afterEach, describe, expect, test } from "bun:test";
import { create } from "@bufbuild/protobuf";
import { WorkersDeleteRequestSchema } from "@roost/shared/proto/coordinator_pb";
import { asChannelId, asSessionId, asWorkerFp } from "@roost/shared/wire";
import {
  lookupSessionId,
  primeChannelMap,
} from "../src/byte-hub.ts";
import {
  __setConnectWorkerForTest,
  connectWorkers,
} from "../src/connect/worker-registry.ts";
import {
  authorize,
  dashboardAdminCtx,
  createDeviceRevocationHarnessOwner,
  key,
  token,
  workerDeleteDashboardId,
} from "./device-revocation-fixture.ts";

const { cleanupHarnesses, openHarness: harness } = createDeviceRevocationHarnessOwner();
afterEach(cleanupHarnesses);

describe("authorized device lifecycle", () => {
  test("worker deletion tombstones the key and invalidates delegated tokens", async () => {
    const h = await harness();
    const worker = await key();
    await authorize(h.db, worker, "worker");
    await h.db.insertInto("accounts").values({
      id: "administrator-account",
      email_normalized: "administrator@example.test",
      password_hash: null,
      status: "active",
      created_at_ms: 1,
      password_changed_at_ms: null,
    }).execute();
    await h.db.insertInto("organizations").values({
      id: "worker-delete-organization",
      slug: "worker-delete-organization",
      name: "Worker Delete Organization",
      status: "active",
      created_at_ms: 1,
    }).execute();
    await h.db.insertInto("organization_memberships").values({
      organization_id: "worker-delete-organization",
      account_id: "administrator-account",
      role: "owner",
      created_at_ms: 1,
    }).execute();
    await h.db.insertInto("dashboards").values({
      id: workerDeleteDashboardId,
      organization_id: "worker-delete-organization",
      slug: "worker-delete-dashboard",
      name: "Worker Delete Dashboard",
      status: "active",
      created_at_ms: 1,
    }).execute();
    await h.db.insertInto("dashboard_memberships").values({
      dashboard_id: workerDeleteDashboardId,
      account_id: "administrator-account",
      role: "admin",
      created_at_ms: 1,
    }).execute();
    await h.db.insertInto("workers").values({
      fp: worker.fingerprint,
      dashboard_id: workerDeleteDashboardId,
      label: "worker",
      os: "linux",
      git_sha: null,
      host_metrics_json: null,
      registered_at_ms: 1,
      last_seen_ms: 1,
      reachable_addr: null,
      keeper_stale: null,
    }).execute();
    const sessionId = asSessionId("00000000-0000-4000-8000-000000000601");
    const workspaceId = "00000000-0000-4000-8000-000000000602";
    await h.db.insertInto("workspaces").values({
      id: workspaceId,
      dashboard_id: workerDeleteDashboardId,
      worker_fp: worker.fingerprint,
      name: "retained",
      folder_path: "/tmp/retained",
      color: null,
      position: 0,
      version: 0,
      created_at_ms: 1,
      updated_at_ms: 1,
    }).execute();
    await h.db.insertInto("sessions").values({
      id: sessionId,
      dashboard_id: workerDeleteDashboardId,
      worker_fp: worker.fingerprint,
      channel: 17,
      kind: "shell",
      cwd: "/tmp/retained",
      workspace_id: workspaceId,
      status: "open",
      created_at: 1,
    }).execute();
    await h.db.insertInto("workspace_sessions").values({
      workspace_id: workspaceId,
      dashboard_id: workerDeleteDashboardId,
      session_id: sessionId,
      added_at_ms: 1,
    }).execute();
    await h.db.insertInto("events").values({
      dashboard_id: workerDeleteDashboardId,
      kind: "cwd",
      session_id: sessionId,
      worker_fp: worker.fingerprint,
      payload_json: JSON.stringify({
        kind: "cwd",
        session_id: sessionId,
        cwd: "/tmp/retained",
        ts: 1,
      }),
      ts: 1,
      client_seq: 1,
    }).execute();
    const liveHandle = {
      workerFp: worker.fingerprint,
      dashboardId: workerDeleteDashboardId,
      revoked: false,
      ready: true,
      send: () => 1,
    };
    __setConnectWorkerForTest(worker.fingerprint, liveHandle);
    primeChannelMap([{
      id: sessionId,
      worker_fp: worker.fingerprint,
      channel: 17,
    }]);
    await token(h, "worker-delegated", worker.fingerprint, "browser", workerDeleteDashboardId);
    await token(h, "legacy-uncertain", null, "browser", workerDeleteDashboardId);
    h.sqlite.exec(`
      CREATE TRIGGER worker_delete_rollback
      BEFORE UPDATE OF deleted_at_ms ON workers
      BEGIN
        SELECT RAISE(ABORT, 'injected worker tombstone failure');
      END
    `);
    await expect(h.workerHandlers.workersDelete(
      create(WorkersDeleteRequestSchema, { fp: worker.fingerprint }),
      dashboardAdminCtx(),
    )).rejects.toThrow("injected worker tombstone failure");
    expect(await h.db.selectFrom("workers").select("deleted_at_ms")
      .where("fp", "=", worker.fingerprint).executeTakeFirst())
      .toEqual({ deleted_at_ms: null });
    expect(await h.db.selectFrom("authorized_keys").select("fingerprint")
      .where("fingerprint", "=", worker.fingerprint).executeTakeFirst())
      .toEqual({ fingerprint: worker.fingerprint });
    expect(await h.db.selectFrom("authorized_key_revocations").select("fingerprint")
      .where("fingerprint", "=", worker.fingerprint).executeTakeFirst())
      .toBeUndefined();
    expect(await h.db.selectFrom("bootstrap_tokens").select("token_hash").execute())
      .toHaveLength(2);
    expect(h.workerFences).toEqual([]);
    expect(h.workerSyncRemovals).toEqual([]);
    expect(h.revoked).toEqual([]);
    expect(connectWorkers.get(worker.fingerprint)).toBe(liveHandle);
    expect(liveHandle.revoked).toBe(false);
    expect(lookupSessionId(asWorkerFp(worker.fingerprint), asChannelId(17)))
      .toBe(sessionId);
    h.sqlite.exec("DROP TRIGGER worker_delete_rollback");

    const response = await h.workerHandlers.workersDelete(
      create(WorkersDeleteRequestSchema, { fp: worker.fingerprint }),
      dashboardAdminCtx(),
    );
    expect(response.ok).toBe(true);
    const tombstone = await h.db.selectFrom("workers")
      .select(["fp", "deleted_at_ms"])
      .where("fp", "=", worker.fingerprint)
      .executeTakeFirstOrThrow();
    expect(tombstone.fp).toBe(worker.fingerprint);
    expect(typeof tombstone.deleted_at_ms).toBe("number");
    expect(await h.db.selectFrom("authorized_keys").selectAll().execute()).toEqual([]);
    expect(await h.db.selectFrom("bootstrap_tokens")
      .select("minted_by_fp")
      .execute()).toEqual([{ minted_by_fp: null }]);
    expect(await h.db.selectFrom("authorized_key_revocations").select("fingerprint").execute())
      .toEqual([{ fingerprint: worker.fingerprint }]);
    expect(await h.db.selectFrom("sessions").select("id").execute())
      .toEqual([{ id: sessionId }]);
    expect(await h.db.selectFrom("workspaces").select("id").execute())
      .toEqual([{ id: workspaceId }]);
    expect(await h.db.selectFrom("workspace_sessions")
      .select(["workspace_id", "session_id"]).execute())
      .toEqual([{ workspace_id: workspaceId, session_id: sessionId }]);
    expect(await h.db.selectFrom("events").select("session_id").execute())
      .toEqual([{ session_id: sessionId }]);
    expect(liveHandle.revoked).toBe(true);
    expect(connectWorkers.has(worker.fingerprint)).toBe(false);
    expect(lookupSessionId(asWorkerFp(worker.fingerprint), asChannelId(17)))
      .toBeUndefined();
    expect(h.workerFences).toEqual([worker.fingerprint]);
    expect(h.workerSyncRemovals).toEqual([{
      dashboardId: workerDeleteDashboardId,
      fingerprint: worker.fingerprint,
    }]);
    await expect(h.db.insertInto("authorized_keys").values({
      fingerprint: worker.fingerprint,
      public_key: worker.raw,
      label: "reconnect",
      added_at: Date.now(),
    }).execute()).rejects.toThrow("authorized key revoked");
    expect(h.revoked).toEqual([worker.fingerprint]);
  });

});

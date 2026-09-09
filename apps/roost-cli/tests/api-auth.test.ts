import { Database } from "bun:sqlite";
import { Code, ConnectError } from "@connectrpc/connect";
import { describe, expect, test } from "bun:test";
import { mkdtemp, rm } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { runMigrations } from "../../coord/src/db/migrate.ts";
import { ensureSelfHostedTenant } from "../../coord/src/self-hosted-tenant.ts";
import type { WorkerConfig } from "../../worker/src/config.ts";
import type { CoordClient, CoordClientOptions } from "../../worker/src/coord-client.ts";
import { loadWorkerKey } from "../../worker/src/jwt.ts";
import {
  _buildCliContextForCredentials,
  CLI_DASHBOARD_HEADER,
  CLI_KEY_LABEL,
  CLI_LEGACY_SCOPE_RESOLUTION_FAILED,
  CLI_PAIRING_REQUIRED,
  cliKeyPath,
  ensureCliEnrollment,
  type CliKey,
} from "../src/cli-auth.ts";
const TEST_CONFIG = {
  coordinatorUrl: "http://127.0.0.1:4103",
  label: CLI_KEY_LABEL,
} as WorkerConfig;
function testCliKey(fingerprint: string): CliKey {
  return { fingerprint, pubKey: new Uint8Array([1, 2, 3]) } as CliKey;
}
function fakeClient(value: Record<string, unknown>): CoordClient {
  return value as unknown as CoordClient;
}
async function createLegacyDatabase(
  fingerprint: string,
  activeMemberships: 0 | 1 | 2,
): Promise<{ databasePath: string; dashboardId: string; remove(): Promise<void> }> {
  const root = await mkdtemp(join(tmpdir(), "roost-cli-auth-"));
  const databasePath = join(root, "coordinator_v2.db");
  const sqlite = new Database(databasePath);
  let dashboardId = "";
  try {
    await runMigrations(sqlite, undefined, undefined, (name) => {
      if (name === "0024_auth_tenancy_stabilization") {
        ensureSelfHostedTenant(sqlite, { backfillLegacyScopes: true });
      }
    });
    const tenant = ensureSelfHostedTenant(sqlite, { backfillLegacyScopes: false });
    dashboardId = tenant.dashboardId;
    const now = Date.now();
    sqlite.query(`
      INSERT INTO authorized_keys (fingerprint, public_key, label, added_at)
      VALUES (?, ?, ?, ?)
    `).run(fingerprint, new Uint8Array([1, 2, 3]), CLI_KEY_LABEL, now);
    if (activeMemberships > 0) {
      sqlite.query(`
        INSERT INTO account_devices (fingerprint, account_id, added_at_ms, last_seen_at_ms)
        VALUES (?, ?, ?, ?)
      `).run(fingerprint, tenant.accountId, now, now);
    }
    if (activeMemberships === 2) {
      sqlite.query(`
        INSERT INTO dashboards (id, organization_id, slug, name, status, created_at_ms)
        VALUES (?, ?, ?, ?, 'active', ?)
      `).run("second-dashboard", tenant.organizationId, "second", "Second", now);
      sqlite.query(`
        INSERT INTO dashboard_memberships (dashboard_id, account_id, role, created_at_ms)
        VALUES (?, ?, 'admin', ?)
      `).run("second-dashboard", tenant.accountId, now);
    }
  } finally {
    sqlite.close(true);
  }
  return {
    databasePath,
    dashboardId,
    remove: () => rm(root, { recursive: true, force: true }),
  };
}
describe("CLI device authentication", () => {
  test("the production key path is isolated from the worker key", () => {
    expect(cliKeyPath("/home/alice")).toBe("/home/alice/.roost/cli-key");
    expect(cliKeyPath("/home/alice")).not.toContain("RoostWorkerV2");
  });

  test("key loading caches by path instead of lending a previously loaded worker key", async () => {
    const root = await mkdtemp(join(tmpdir(), "roost-cli-key-"));
    try {
      const worker = await loadWorkerKey(join(root, "worker-key"));
      const cli = await loadWorkerKey(join(root, "cli-key"));
      expect(cli.fingerprint).not.toBe(worker.fingerprint);
      expect(cli.pubKey).not.toEqual(worker.pubKey);
    } finally {
      await rm(root, { recursive: true, force: true });
    }
  });

  test("an already enrolled key costs one protected probe and never redeems", async () => {
    let probes = 0;
    await ensureCliEnrollment({
      client: {
        async workersList() {
          probes++;
          return { workers: [] };
        },
      },
      publicClient: {
        async authRedeemBrowser() { throw new Error("must not redeem a known key"); },
      },
      publicKeyB64: "public-key",
      label: "roost-cli",
      localDatabasePath: null,
      async mintHostBrowserToken() { throw new Error("must not mint for a known key"); },
    });
    expect(probes).toBe(1);
  });

  test("host-mints one browser grant, redeems the CLI key, then reprobes", async () => {
    let probes = 0;
    let mintInput: unknown;
    let redeemed: unknown;
    await ensureCliEnrollment({
      client: {
        async workersList() {
          probes++;
          if (probes === 1) throw new ConnectError("unknown key", Code.Unauthenticated);
          return { workers: [] };
        },
      },
      publicClient: {
        async authRedeemBrowser(request: unknown) { redeemed = request; },
      },
      publicKeyB64: "cli-public-key",
      label: "roost-cli",
      localDatabasePath: "/var/lib/roost/coordinator_v2.db",
      async mintHostBrowserToken(databasePath, input) {
        mintInput = { databasePath, input };
        return { token: "one-shot-secret", expiresAtMs: 123 };
      },
    });

    expect(mintInput).toEqual({
      databasePath: "/var/lib/roost/coordinator_v2.db",
      input: { kind: "browser", label: "roost-cli" },
    });
    expect(redeemed).toEqual({
      token: "one-shot-secret",
      sshPubkeyB64: "cli-public-key",
      label: "roost-cli",
    });
    // The enrolled key is proven against the coordinator, not assumed.
    expect(probes).toBe(2);
  });

  test("an unknown remote self-hosted key requires explicit pairing", async () => {
    const promise = ensureCliEnrollment({
      client: {
        async workersList() {
          throw new ConnectError("unknown key", Code.Unauthenticated);
        },
      },
      publicClient: {
        async authRedeemBrowser() { throw new Error("must not redeem remotely"); },
      },
      publicKeyB64: "cli-public-key",
      label: "roost-cli",
      localDatabasePath: null,
    });
    await expect(promise).rejects.toThrow(CLI_PAIRING_REQUIRED);
  });

  test("a probe failure that is not an authentication verdict never enrolls", async () => {
    let mints = 0;
    const promise = ensureCliEnrollment({
      client: {
        async workersList() {
          throw new ConnectError("coordinator unreachable", Code.Unavailable);
        },
      },
      publicClient: {
        async authRedeemBrowser() { throw new Error("must not redeem after a transport failure"); },
      },
      publicKeyB64: "cli-public-key",
      label: "roost-cli",
      localDatabasePath: "/var/lib/roost/coordinator_v2.db",
      async mintHostBrowserToken() {
        mints++;
        return { token: "one-shot-secret", expiresAtMs: 123 };
      },
    });
    await expect(promise).rejects.toThrow("coordinator unreachable");
    expect(mints).toBe(0);
  });

  test("keeps a modern successful probe unscoped without resolving local state", async () => {
    const clientOptions: CoordClientOptions[] = [];
    let resolverCalls = 0;
    const context = await _buildCliContextForCredentials({
      cfg: TEST_CONFIG,
      key: testCliKey("modern-cli"),
      label: CLI_KEY_LABEL,
      localDatabasePath: "/not-opened.db",
      createClient(options) {
        clientOptions.push(options);
        return fakeClient({ async workersList() { return { workers: [] }; } });
      },
      publicClient: { async authRedeemBrowser() { throw new Error("must not redeem"); } },
      resolveLegacyDashboardId() {
        resolverCalls++;
        throw new Error("must not resolve");
      },
    });
    expect(context.legacyDashboardId).toBeNull();
    expect(clientOptions).toHaveLength(1);
    expect(clientOptions[0]?.configureRequestHeaders).toBeUndefined();
    expect(resolverCalls).toBe(0);
  });

  test("enrolls once, scopes the legacy retry, and retains its client", async () => {
    const key = testCliKey("legacy-cli");
    const database = await createLegacyDatabase(key.fingerprint, 1);
    try {
      const clientOptions: CoordClientOptions[] = [];
      let baseProbes = 0;
      let mints = 0;
      let redeems = 0;
      let laterProtectedCalls = 0;
      const baseClient = fakeClient({
        async workersList() {
          baseProbes++;
          throw new ConnectError(
            baseProbes === 1 ? "unknown key" : "legacy selection required",
            baseProbes === 1 ? Code.Unauthenticated : Code.NotFound,
          );
        },
      });
      const scopedClient = fakeClient({
        async workersList() { return { workers: [] }; },
        async authMintBootstrap() {
          laterProtectedCalls++;
          return { token: "worker-grant" };
        },
      });
      const context = await _buildCliContextForCredentials({
        cfg: TEST_CONFIG,
        key,
        label: CLI_KEY_LABEL,
        localDatabasePath: database.databasePath,
        createClient(options) {
          clientOptions.push(options);
          return clientOptions.length === 1 ? baseClient : scopedClient;
        },
        publicClient: { async authRedeemBrowser() { redeems++; } },
        async mintHostBrowserToken() {
          mints++;
          return { token: "browser-grant", expiresAtMs: 1 };
        },
      });
      expect([baseProbes, mints, redeems]).toEqual([2, 1, 1]);
      expect(context.legacyDashboardId).toBe(database.dashboardId);
      expect(context.client).toBe(scopedClient);
      const headers = new Headers({ retained: "header", authorization: "caller-value" });
      clientOptions[1]?.configureRequestHeaders?.(headers);
      expect(headers.get(CLI_DASHBOARD_HEADER)).toBe(database.dashboardId);
      expect(headers.get("retained")).toBe("header");
      // The selected fake exposes one later protected unary call.
      const selectedClient = context.client as unknown as {
        authMintBootstrap(): Promise<unknown>;
      };
      await selectedClient.authMintBootstrap();
      expect(laterProtectedCalls).toBe(1);
      let initialFactories = 0;
      const initialContext = await _buildCliContextForCredentials({
        cfg: TEST_CONFIG, key, label: CLI_KEY_LABEL, localDatabasePath: database.databasePath,
        createClient: () => initialFactories++ === 0
          ? fakeClient({ async workersList() { throw new ConnectError("legacy selection required", Code.NotFound); } })
          : scopedClient,
        publicClient: { async authRedeemBrowser() { throw new Error("must not redeem"); } },
      });
      expect(initialContext).toMatchObject({ client: scopedClient, legacyDashboardId: database.dashboardId });
    } finally {
      await database.remove();
    }
  });

  test("does not scope an initial legacy NotFound without local enrollment", async () => {
    const initialNotFound = new ConnectError("legacy selection required", Code.NotFound);
    let resolverCalls = 0;
    await expect(_buildCliContextForCredentials({
      cfg: TEST_CONFIG,
      key: testCliKey("initial-not-found"),
      label: CLI_KEY_LABEL,
      localDatabasePath: null,
      createClient: () => fakeClient({
        async workersList() { throw initialNotFound; },
      }),
      publicClient: { async authRedeemBrowser() { throw new Error("must not redeem"); } },
      resolveLegacyDashboardId() {
        resolverCalls++;
        return "must-not-scope";
      },
    })).rejects.toBe(initialNotFound);
    expect(resolverCalls).toBe(0);
  });

  test("fails closed for unavailable or denied protected probes", async () => {
    let resolverCalls = 0;
    for (const code of [Code.Unavailable, Code.PermissionDenied]) {
      const failure = new ConnectError("protected probe failed", code);
      await expect(_buildCliContextForCredentials({
        cfg: TEST_CONFIG,
        key: testCliKey(`probe-${code}`),
        label: CLI_KEY_LABEL,
        localDatabasePath: "/not-opened.db",
        createClient: () => fakeClient({
          async workersList() { throw failure; },
        }),
        publicClient: { async authRedeemBrowser() { throw new Error("must not redeem"); } },
        resolveLegacyDashboardId() {
          resolverCalls++;
          return "must-not-scope";
        },
      })).rejects.toBe(failure);
    }
    expect(resolverCalls).toBe(0);
  });

  test("rejects zero or multiple active local dashboard memberships", async () => {
    for (const activeMemberships of [0, 2] as const) {
      const key = testCliKey(`membership-${activeMemberships}`);
      const database = await createLegacyDatabase(key.fingerprint, activeMemberships);
      try {
        let clients = 0;
        let probes = 0;
        await expect(_buildCliContextForCredentials({
          cfg: TEST_CONFIG,
          key,
          label: CLI_KEY_LABEL,
          localDatabasePath: database.databasePath,
          createClient: () => {
            clients++;
            return fakeClient({
              async workersList() {
                probes++;
                throw new ConnectError(
                  probes === 1 ? "unknown key" : "legacy selection required",
                  probes === 1 ? Code.Unauthenticated : Code.NotFound,
                );
              },
            });
          },
          publicClient: { async authRedeemBrowser() {} },
          async mintHostBrowserToken() {
            return { token: "browser-grant", expiresAtMs: 1 };
          },
        })).rejects.toThrow(CLI_LEGACY_SCOPE_RESOLUTION_FAILED);
        expect(clients).toBe(1);
      } finally {
        await database.remove();
      }
    }
  });

  test("does not broaden authorization after a scoped retry fails", async () => {
    const key = testCliKey("failed-scoped-retry");
    const database = await createLegacyDatabase(key.fingerprint, 1);
    try {
      let clients = 0;
      let baseProbes = 0;
      const scopedFailure = new ConnectError("scoped retry denied", Code.PermissionDenied);
      await expect(_buildCliContextForCredentials({
        cfg: TEST_CONFIG,
        key,
        label: CLI_KEY_LABEL,
        localDatabasePath: database.databasePath,
        createClient: () => {
          clients++;
          if (clients === 1) {
            return fakeClient({
              async workersList() {
                baseProbes++;
                throw new ConnectError(
                  baseProbes === 1 ? "unknown key" : "legacy selection required",
                  baseProbes === 1 ? Code.Unauthenticated : Code.NotFound,
                );
              },
            });
          }
          return fakeClient({ async workersList() { throw scopedFailure; } });
        },
        publicClient: { async authRedeemBrowser() {} },
        async mintHostBrowserToken() {
          return { token: "browser-grant", expiresAtMs: 1 };
        },
      })).rejects.toBe(scopedFailure);
      expect(clients).toBe(2);
    } finally {
      await database.remove();
    }
  });
});

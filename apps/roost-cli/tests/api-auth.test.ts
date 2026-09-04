import { Code, ConnectError } from "@connectrpc/connect";
import { describe, expect, test } from "bun:test";
import { X_ROOST_DASHBOARD_ID } from "@roost/shared/wire/headers";
import { mkdtemp, rm } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { loadWorkerKey } from "../../worker/src/jwt.ts";
import {
  CLI_PAIRING_REQUIRED,
  MANAGED_CLI_ENROLLMENT_UNSUPPORTED,
  cliKeyPath,
  ensureCliEnrollment,
  withDashboardScope,
} from "../src/cli-auth.ts";

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

  test("uses AuthDashboardAccess selection for every unary request", async () => {
    const accessHeaders: Array<string | null> = [];
    const client = {
      async authDashboardAccess(_request: Record<string, never>, options?: { headers?: HeadersInit }) {
        accessHeaders.push(new Headers(options?.headers).get(X_ROOST_DASHBOARD_ID));
        return { selectedDashboardId: "dashboard-selected" };
      },
    };
    const selected = await ensureCliEnrollment({
      client,
      publicClient: {
        async authCoordIdentity() { throw new Error("must not probe a known key"); },
        async authRedeemBrowser() { throw new Error("must not redeem a known key"); },
      },
      publicKeyB64: "public-key",
      label: "roost-cli",
      requestedDashboardId: "dashboard-requested",
      localDatabasePath: null,
    });
    expect(selected).toBe("dashboard-selected");
    expect(accessHeaders).toEqual(["dashboard-requested"]);

    const unaryHeaders: Array<string | null> = [];
    const scoped = withDashboardScope({
      async workersList(_request: Record<string, never>, options?: { headers?: HeadersInit }) {
        unaryHeaders.push(new Headers(options?.headers).get(X_ROOST_DASHBOARD_ID));
        return { workers: [] };
      },
    }, selected);
    await scoped.workersList({});
    expect(unaryHeaders).toEqual(["dashboard-selected"]);
  });

  test("host-mints one browser grant and redeems the CLI key", async () => {
    let accessAttempts = 0;
    let mintInput: unknown;
    let redeemed: unknown;
    const client = {
      async authDashboardAccess() {
        accessAttempts++;
        if (accessAttempts === 1) {
          throw new ConnectError("unknown key", Code.Unauthenticated);
        }
        return { selectedDashboardId: "dashboard-local" };
      },
    };
    const selected = await ensureCliEnrollment({
      client,
      publicClient: {
        async authCoordIdentity() { return { saasMode: false }; },
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

    expect(selected).toBe("dashboard-local");
    expect(mintInput).toEqual({
      databasePath: "/var/lib/roost/coordinator_v2.db",
      input: { kind: "browser", label: "roost-cli" },
    });
    expect(redeemed).toEqual({
      token: "one-shot-secret",
      sshPubkeyB64: "cli-public-key",
      label: "roost-cli",
    });
    expect(accessAttempts).toBe(2);
  });

  test("an unknown remote self-hosted key requires explicit pairing", async () => {
    const promise = ensureCliEnrollment({
      client: {
        async authDashboardAccess() {
          throw new ConnectError("unknown key", Code.Unauthenticated);
        },
      },
      publicClient: {
        async authCoordIdentity() { return { saasMode: false }; },
        async authRedeemBrowser() { throw new Error("must not redeem remotely"); },
      },
      publicKeyB64: "cli-public-key",
      label: "roost-cli",
      localDatabasePath: null,
    });
    await expect(promise).rejects.toThrow(CLI_PAIRING_REQUIRED);
  });

  test("a fresh managed CLI never acquires browser or worker authority", async () => {
    const promise = ensureCliEnrollment({
      client: {
        async authDashboardAccess() {
          throw new ConnectError("unknown key", Code.Unauthenticated);
        },
      },
      publicClient: {
        async authCoordIdentity() { return { saasMode: true }; },
        async authRedeemBrowser() { throw new Error("must not redeem in managed mode"); },
      },
      publicKeyB64: "cli-public-key",
      label: "roost-cli",
      localDatabasePath: "/managed/coordinator_v2.db",
      async mintHostBrowserToken() {
        throw new Error("must not mint in managed mode");
      },
    });
    await expect(promise).rejects.toThrow(MANAGED_CLI_ENROLLMENT_UNSUPPORTED);
  });
});

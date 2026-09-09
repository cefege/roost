import { Code, ConnectError } from "@connectrpc/connect";
import { describe, expect, test } from "bun:test";
import { mkdtemp, rm } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { loadWorkerKey } from "../../worker/src/jwt.ts";
import {
  CLI_PAIRING_REQUIRED,
  cliKeyPath,
  ensureCliEnrollment,
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
});

import { Database } from "bun:sqlite";
import { describe, expect, test } from "bun:test";
import { mkdtemp, rm, stat } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join } from "node:path";
import {
  acquireMachineTransaction,
  MachineTransactionBusyError,
  type AcquireMachineTransactionOptions,
  type MachineTransactionKind,
} from "../src/machine-transaction.ts";

const KINDS: readonly MachineTransactionKind[] = [
  "keeper-refresh",
  "update",
  "relocation",
  "deploy",
];

async function withTempRoot(run: (root: string) => Promise<void>): Promise<void> {
  const root = await mkdtemp(join(tmpdir(), "roost-machine-transaction-"));
  try {
    await run(root);
  } finally {
    await rm(root, { recursive: true, force: true });
  }
}

function options(
  root: string,
  lockPath: string,
  processEpoch: string,
): AcquireMachineTransactionOptions {
  return {
    platform: "linux",
    env: { ROOST_SERVICE_DIR: root },
    lockPath,
    processEpoch,
  };
}

async function expectBusy(operation: Promise<unknown>): Promise<MachineTransactionBusyError> {
  let caught: unknown;
  try {
    await operation;
  } catch (error) {
    caught = error;
  }
  expect(caught).toBeInstanceOf(MachineTransactionBusyError);
  return caught as MachineTransactionBusyError;
}


describe("machine transactions", () => {
  test("every transaction kind excludes every other kind", async () => {
    await withTempRoot(async (root) => {
      for (const ownerKind of KINDS) {
        for (const contenderKind of KINDS) {
          const lockPath = join(root, `${ownerKind}-${contenderKind}.sqlite`);
          const owner = await acquireMachineTransaction(
            ownerKind,
            `/journals/${ownerKind}.jsonl`,
            options(root, lockPath, `owner-${ownerKind}-${contenderKind}`),
          );
          const busy = await expectBusy(acquireMachineTransaction(
            contenderKind,
            `/journals/${contenderKind}.jsonl`,
            options(root, lockPath, `contender-${ownerKind}-${contenderKind}`),
          ));
          expect(busy.active).toBeNull();
          await owner.release();
        }
      }
    });
  }, 30_000);

  test("remote leases exclude local transactions and expired leases are reclaimed", async () => {
    await withTempRoot(async (root) => {
      const lockPath = join(root, "machine-transaction.sqlite");
      const now = Math.floor(Date.now() / 1000);
      const seed = new Database(lockPath, { create: true });
      try {
        seed.exec(`
          CREATE TABLE deploy_lease (
            singleton INTEGER PRIMARY KEY CHECK (singleton = 1),
            owner TEXT NOT NULL,
            created_at INTEGER NOT NULL,
            expires_at INTEGER NOT NULL
          )
        `);
        seed.query(
          "INSERT INTO deploy_lease (singleton, owner, created_at, expires_at) VALUES (1, ?, ?, ?)",
        ).run("remote-owner", now, now + 60);
      } finally {
        seed.close();
      }

      await expectBusy(acquireMachineTransaction(
        "update",
        "/journals/update.jsonl",
        options(root, lockPath, "blocked-by-remote"),
      ));

      const malformed = new Database(lockPath);
      try {
        malformed.query(
          "UPDATE deploy_lease SET expires_at = 'not-a-timestamp' WHERE singleton = 1",
        ).run();
      } finally {
        malformed.close();
      }
      await expectBusy(acquireMachineTransaction(
        "relocation",
        "/journals/relocation.jsonl",
        options(root, lockPath, "blocked-by-malformed-remote"),
      ));

      const reversedTimestamps = new Database(lockPath);
      try {
        reversedTimestamps.query(
          "UPDATE deploy_lease SET owner = ?, created_at = 2, expires_at = 1 WHERE singleton = 1",
        ).run("malformed-owner");
      } finally {
        reversedTimestamps.close();
      }
      await expectBusy(acquireMachineTransaction(
        "relocation",
        "/journals/relocation.jsonl",
        options(root, lockPath, "blocked-by-reversed-lease"),
      ));

      const expire = new Database(lockPath);
      try {
        expire.query(
          "UPDATE deploy_lease SET owner = ?, created_at = 1, expires_at = 2 WHERE singleton = 1",
        ).run("expired-owner");
      } finally {
        expire.close();
      }
      const recovered = await acquireMachineTransaction(
        "keeper-refresh",
        "/journals/keeper.jsonl",
        options(root, lockPath, "recovered"),
      );
      await recovered.release();

      const inspect = new Database(lockPath, { readonly: true });
      try {
        expect(inspect.query(
          "SELECT owner FROM deploy_lease WHERE singleton = 1",
        ).get()).toBeNull();
      } finally {
        inspect.close();
      }
    });
  });

  test("release atomically hands the machine to a successor", async () => {
    await withTempRoot(async (root) => {
      const lockPath = join(root, "machine-transaction.sqlite");
      const owner = await acquireMachineTransaction(
        "update",
        "/journals/update.jsonl",
        options(root, lockPath, "owner"),
      );
      await expectBusy(acquireMachineTransaction(
        "deploy",
        "/journals/deploy.jsonl",
        options(root, lockPath, "early-successor"),
      ));
      await owner.release();

      const successor = await acquireMachineTransaction(
        "deploy",
        "/journals/deploy.jsonl",
        options(root, lockPath, "successor"),
      );
      expect(successor.processEpoch).toBe("successor");
      await successor.release();
    });
  });

  test("kernel lock is released when the owner process is killed", async () => {
    await withTempRoot(async (root) => {
      const lockPath = join(root, "machine-transaction.sqlite");
      const moduleUrl = new URL("../src/machine-transaction.ts", import.meta.url).href;
      const child = Bun.spawn([
        process.execPath,
        "-e",
        `import { acquireMachineTransaction } from ${JSON.stringify(moduleUrl)};
         await acquireMachineTransaction("update", "/journals/child.jsonl", {
           platform: "linux",
           lockPath: ${JSON.stringify(lockPath)},
           env: { ROOST_SERVICE_DIR: ${JSON.stringify(root)} },
           processEpoch: "child",
         });
         console.log("READY");
         await new Response(Bun.stdin.stream()).text();`,
      ], {
        stdin: "pipe",
        stdout: "pipe",
        stderr: "pipe",
      });
      try {
        const ready = await child.stdout.getReader().read();
        expect(new TextDecoder().decode(ready.value)).toContain("READY");
        await expectBusy(acquireMachineTransaction(
          "relocation",
          "/journals/parent.jsonl",
          options(root, lockPath, "parent-before-kill"),
        ));
      } finally {
        child.kill("SIGKILL");
        await child.exited;
      }

      const recovered = await acquireMachineTransaction(
        "relocation",
        "/journals/parent.jsonl",
        options(root, lockPath, "parent-after-kill"),
      );
      await recovered.release();
    });
  });

  test("lock database stays private and reusable", async () => {
    await withTempRoot(async (root) => {
      const lockPath = join(root, "machine-transaction.sqlite");
      const first = await acquireMachineTransaction(
        "keeper-refresh",
        "/journals/keeper.jsonl",
        options(root, lockPath, "first"),
      );
      await first.release();
      expect((await stat(lockPath)).mode & 0o777).toBe(0o600);

      const second = await acquireMachineTransaction(
        "update",
        "/journals/update.jsonl",
        options(root, lockPath, "second"),
      );
      await second.release();
    });
  });
});

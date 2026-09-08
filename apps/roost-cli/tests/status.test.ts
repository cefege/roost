// Status tests pin the two origins the readout can speak about, the exact
// ✓/✗ lines and remedies, the health gate, and the worker-inventory
// projection that feeds update admission.
import { describe, expect, test } from "bun:test";
import { Database } from "bun:sqlite";
import { mkdtempSync, rmSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import {
  _probeCoordinatorIdentity,
  printStatusReport,
  resolveCoordinatorDbPath,
  resolveStatusEndpoint,
  statusReportIsHealthy,
  workerInventory,
  workerInventoryForUpdateAdmission,
  type StatusReport,
} from "../src/status.ts";

function report(overrides: Partial<StatusReport> = {}): StatusReport {
  return {
    coordAgentLoaded: true,
    workerAgentLoaded: true,
    coord: { reachable: true, gitSha: null },
    workers: [],
    endpoint: { publicUrl: "https://dash.example.test", answers: true },
    ...overrides,
  };
}

function renderedStatus(report: StatusReport): string[] {
  const lines: string[] = [];
  const originalLog = console.log;
  console.log = (...args: unknown[]) => { lines.push(args.join(" ")); };
  try {
    printStatusReport(report);
  } finally {
    console.log = originalLog;
  }
  return lines;
}

type TestFetchImplementation = (
  input: string | URL | Request,
  init?: BunFetchRequestInit,
) => Promise<Response>;

function testFetch(implementation: TestFetchImplementation): typeof fetch {
  return Object.assign(implementation, { preconnect: fetch.preconnect });
}

describe("status endpoint resolution", () => {
  test("reads the front door and the coordinator's own bind from one unit", () => {
    const service = [
      "[Service]",
      'Environment="ROOST_COORDINATOR_BIND=127.0.0.1:4213"',
      'Environment="ROOST_WEB_PUBLIC_URL=https://dash.example.test"',
      'Environment="ROOST_COORDINATOR_PUBLIC_URL="',
    ].join("\n");
    expect(resolveStatusEndpoint(service, { platform: "linux" })).toEqual({
      publicUrl: "https://dash.example.test",
      coordUrl: "http://127.0.0.1:4213",
    });
  });

  test("falls back to the coordinator identity origin and refuses a plaintext front door", () => {
    expect(resolveStatusEndpoint(
      "<key>ROOST_COORDINATOR_PUBLIC_URL</key><string>https://coord.example.test:7443</string>",
      { platform: "darwin" },
    )).toEqual({ publicUrl: "https://coord.example.test:7443", coordUrl: null });
    expect(resolveStatusEndpoint(
      'Environment="ROOST_WEB_PUBLIC_URL=http://dash.example.test"',
      { platform: "linux" },
    ).publicUrl).toBeNull();
  });

  test("an override names the front door with no installed service at all", () => {
    expect(resolveStatusEndpoint(null, {
      platform: "linux",
      override: { origin: "https://dash.example.test" },
    })).toEqual({ publicUrl: "https://dash.example.test", coordUrl: null });
  });
});

describe("status front-door reporting", () => {
  test("a declared front door that answers prints ✓ and passes the gate", () => {
    const healthy = report();
    const lines = renderedStatus(healthy);

    expect(lines).toContain("  ✓ public url https://dash.example.test");
    expect(lines).toContain("  open: https://dash.example.test");
    expect(statusReportIsHealthy(healthy)).toBe(true);
  });

  test("a declared front door that stays silent fails the gate with its remedy", () => {
    const silent = report({
      endpoint: { publicUrl: "https://dash.example.test", answers: false },
    });
    const lines = renderedStatus(silent);

    expect(lines).toContain("  ✗ public url https://dash.example.test");
    expect(lines.join("\n")).toContain("does not answer AuthCoordIdentity");
    expect(statusReportIsHealthy(silent)).toBe(false);
  });

  test("no front door is informational: a same-origin install stays healthy", () => {
    const unconfigured = report({ endpoint: { publicUrl: null, answers: false } });
    const lines = renderedStatus(unconfigured);

    expect(lines).toContain("  - public url: not configured");
    expect(lines.join("\n")).toContain("ROOST_WEB_PUBLIC_URL");
    expect(lines.join("\n")).not.toContain("open:");
    expect(statusReportIsHealthy(unconfigured)).toBe(true);
  });

  test("an unreachable coordinator fails the gate", () => {
    const down = report({ coord: { reachable: false, gitSha: null } });
    expect(statusReportIsHealthy(down)).toBe(false);
    expect(renderedStatus(down).join("\n")).toContain("✗ coord reachable");
  });
});

describe("status coordinator liveness", () => {
  test("reads git identity from the unauthenticated identity RPC", async () => {
    let requestedUrl = "";
    const result = await _probeCoordinatorIdentity(
      "https://coord.example.test/roost.v1.CoordinatorService/AuthCoordIdentity",
      testFetch(async (input) => {
        requestedUrl = String(input);
        return Response.json({ gitSha: "abcdef123456" });
      }),
    );

    expect(requestedUrl).toEndWith("/AuthCoordIdentity");
    expect(result).toEqual({ reachable: true, gitSha: "abcdef123456" });
  });

  test("rejects a successful response without coordinator identity", async () => {
    const result = await _probeCoordinatorIdentity(
      "https://coord.example.test/roost.v1.CoordinatorService/AuthCoordIdentity",
      testFetch(async () => Response.json({ ok: true })),
    );
    expect(result).toEqual({ reachable: false, gitSha: null });
  });
});

describe("status coordinator database discovery", () => {
  test("uses the database path installed in the POSIX service", () => {
    expect(resolveCoordinatorDbPath(
      'Environment="ROOST_COORDINATOR_DB=/srv/roost/state%%blue/coordinator.db"',
      "linux",
      "/default/coordinator.db",
    )).toBe("/srv/roost/state%blue/coordinator.db");
    expect(resolveCoordinatorDbPath(
      "<key>ROOST_COORDINATOR_DB</key><string>/Library/Application Support/Roost&amp;Blue/db.sqlite</string>",
      "darwin",
      "/default/coordinator.db",
    )).toBe("/Library/Application Support/Roost&Blue/db.sqlite");
  });

  test("falls back only when no installed database path exists", () => {
    expect(resolveCoordinatorDbPath("[Service]\n", "linux", "/default/coordinator.db"))
      .toBe("/default/coordinator.db");
  });
});

describe("status worker inventory", () => {
  test("projects runtime proof and open sessions for active workers only", () => {
    const root = mkdtempSync(join(tmpdir(), "roost-status-workers-"));
    try {
      const databasePath = join(root, "coordinator.db");
      const keeperRuntime = {
        schema_version: 1,
        running_contract: {
          protocol_version: 1,
          supported_features: [],
          required_features: [],
          implementation_digest: "a".repeat(64),
          bun_abi: "bun-1",
          platform: "linux",
          arch: "x64",
          build_sha: "abc",
        },
        keeper_pid: 42,
        keeper_epoch: "00000000-0000-4000-8000-000000000001",
        channel_count: 2,
        binding_digest: "b".repeat(64),
        reconciled_at_ms: 1,
      } as const;
      const sqlite = new Database(databasePath);
      try {
        sqlite.exec(`
          CREATE TABLE workers (
            fp TEXT PRIMARY KEY,
            label TEXT NOT NULL,
            os TEXT NOT NULL,
            reachable_addr TEXT,
            git_sha TEXT,
            keeper_runtime_json TEXT,
            last_seen_ms INTEGER NOT NULL,
            deleted_at_ms INTEGER
          );
          CREATE TABLE sessions (
            id TEXT PRIMARY KEY,
            worker_fp TEXT NOT NULL,
            status TEXT NOT NULL
          );
          INSERT INTO workers (
            fp, label, os, reachable_addr, git_sha, keeper_runtime_json,
            last_seen_ms, deleted_at_ms
          ) VALUES
            ('active-fp', 'active', 'linux', 'active.test', 'abc', NULL, 1000, NULL),
            ('deleted-fp', 'deleted', 'darwin', 'deleted.test', 'def', NULL, 2000, 3000);
          INSERT INTO sessions (id, worker_fp, status) VALUES
            ('z-open', 'active-fp', 'open'),
            ('a-open', 'active-fp', 'open'),
            ('closed', 'active-fp', 'closed'),
            ('deleted-open', 'deleted-fp', 'open');
        `);
        sqlite.query(
          "UPDATE workers SET keeper_runtime_json = ? WHERE fp = 'active-fp'",
        ).run(JSON.stringify(keeperRuntime));
      } finally {
        sqlite.close();
      }

      const inventory = workerInventoryForUpdateAdmission(databasePath);
      expect(inventory).toHaveLength(1);
      expect(inventory[0]?.fingerprint).toBe("active-fp");
      expect(inventory[0]?.keeperRuntime).toEqual(keeperRuntime);
      expect(inventory[0]?.coordinatorOpenSessionIds).toEqual(["a-open", "z-open"]);
    } finally {
      rmSync(root, { recursive: true, force: true });
    }
  });

  test("reads pre-runtime worker schemas as unproven admission rows", () => {
    const root = mkdtempSync(join(tmpdir(), "roost-status-legacy-workers-"));
    try {
      const databasePath = join(root, "coordinator.db");
      const sqlite = new Database(databasePath);
      try {
        sqlite.exec(`
          CREATE TABLE workers (
            fp TEXT PRIMARY KEY,
            label TEXT NOT NULL,
            os TEXT NOT NULL,
            reachable_addr TEXT,
            git_sha TEXT,
            last_seen_ms INTEGER NOT NULL,
            deleted_at_ms INTEGER
          );
          CREATE TABLE sessions (
            id TEXT PRIMARY KEY,
            worker_fp TEXT NOT NULL,
            status TEXT NOT NULL
          );
          INSERT INTO workers (
            fp, label, os, reachable_addr, git_sha, last_seen_ms, deleted_at_ms
          ) VALUES ('legacy-fp', 'legacy', 'linux', 'legacy.test', 'abc', 1000, NULL);
          INSERT INTO sessions (id, worker_fp, status)
          VALUES ('legacy-open', 'legacy-fp', 'open');
        `);
      } finally {
        sqlite.close();
      }

      expect(workerInventoryForUpdateAdmission(databasePath)).toEqual([
        expect.objectContaining({
          fingerprint: "legacy-fp",
          keeperRuntime: null,
          coordinatorOpenSessionIds: ["legacy-open"],
        }),
      ]);
    } finally {
      rmSync(root, { recursive: true, force: true });
    }
  });

  test("keeps display fallback separate from update-admission failures", () => {
    const root = mkdtempSync(join(tmpdir(), "roost-status-missing-"));
    try {
      const databasePath = join(root, "coordinator.db");
      expect(workerInventory(databasePath)).toEqual([]);
      expect(() => workerInventoryForUpdateAdmission(databasePath))
        .toThrow("coordinator database not found");

      const sqlite = new Database(databasePath);
      try {
        sqlite.exec("CREATE TABLE workers (fp TEXT PRIMARY KEY)");
      } finally {
        sqlite.close();
      }
      expect(workerInventory(databasePath)).toEqual([]);
      expect(() => workerInventoryForUpdateAdmission(databasePath)).toThrow();
    } finally {
      rmSync(root, { recursive: true, force: true });
    }
  });
});

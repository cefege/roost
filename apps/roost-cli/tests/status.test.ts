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
  resolveTlsMode,
  statusReportIsHealthy,
  workerInventory,
  type StatusReport,
} from "../src/status.ts";

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

describe("status TLS topology", () => {
  test("fronted Linux services require the configured Tailscale Serve mapping", () => {
    const service = [
      "[Service]",
      "Environment=ROOST_FRONTED=1",
      "Environment=ROOST_COORD_LOOPBACK_PORT=4213",
    ].join("\n");
    expect(resolveTlsMode(service, "https://host:4102 -> http://127.0.0.1:4213", "linux"))
      .toBe("tailscale-serve");
    expect(resolveTlsMode(service, "https://host:4102 -> http://127.0.0.1:4103", "linux"))
      .toBe("missing");
  });

  test("fronted Darwin services use the default loopback port when omitted", () => {
    const plist = "<key>ROOST_FRONTED</key>\n<string>1</string>";
    expect(resolveTlsMode(plist, "https://host:4102 -> http://127.0.0.1:4103", "darwin"))
      .toBe("tailscale-serve");
  });

  test("direct mode requires both certificate and key paths", () => {
    const complete = [
      "Environment=ROOST_TLS_CERT_PATH=/data/tls/host.crt",
      "Environment=ROOST_TLS_KEY_PATH=/data/tls/host.key",
    ].join("\n");
    expect(resolveTlsMode(complete, null, "linux")).toBe("direct");
    expect(resolveTlsMode("Environment=ROOST_TLS_CERT_PATH=/data/tls/host.crt", null, "linux"))
      .toBe("missing");
  });

  test("direct services use the installed public URL without probing Tailscale", () => {
    const service = [
      "[Service]",
      "Environment=ROOST_FRONTED=0",
      "Environment=ROOST_COORDINATOR_BIND=0.0.0.0:7443",
      "Environment=ROOST_COORDINATOR_PUBLIC_URL=https://coord.example.test:7443",
      "Environment=ROOST_TLS_CERT_PATH=/data/tls/host.crt",
      "Environment=ROOST_TLS_KEY_PATH=/data/tls/host.key",
    ].join("\n");
    const endpoint = resolveStatusEndpoint(service, {
      platform: "linux",
      resolveTailscale: () => {
        throw new Error("direct status must not probe Tailscale");
      },
    });

    expect(endpoint).toEqual({
      mode: "explicit",
      origin: "https://coord.example.test:7443",
      healthUrl: "https://coord.example.test:7443/roost.v1.CoordinatorService/AuthCoordIdentity",
      tailscale: {
        required: false,
        state: "NotRequired",
        fqdn: null,
        running: false,
      },
    });
  });

  test("automatic services retain their Tailscale FQDN and installed HTTPS port", () => {
    const service = [
      "[Service]",
      "Environment=ROOST_FRONTED=1",
      "Environment=ROOST_TAILNET_HTTPS_PORT=4212",
    ].join("\n");
    let probes = 0;
    const endpoint = resolveStatusEndpoint(service, {
      platform: "linux",
      resolveTailscale: () => {
        probes += 1;
        return { state: "Running", fqdn: "host.tail.ts.net" };
      },
    });

    expect(probes).toBe(1);
    expect(endpoint).toEqual({
      mode: "automatic",
      origin: "https://host.tail.ts.net:4212",
      healthUrl: "https://host.tail.ts.net:4212/roost.v1.CoordinatorService/AuthCoordIdentity",
      tailscale: {
        required: true,
        state: "Running",
        fqdn: "host.tail.ts.net",
        running: true,
      },
    });
  });

  test("automatic endpoint overrides still require Tailscale", () => {
    const endpoint = resolveStatusEndpoint(null, {
      platform: "win32",
      override: { mode: "automatic", origin: "https://host.tail.ts.net:4102" },
      resolveTailscale: () => ({ state: "Stopped", fqdn: null }),
    });

    expect(endpoint.mode).toBe("automatic");
    expect(endpoint.origin).toBe("https://host.tail.ts.net:4102");
    expect(endpoint.tailscale.required).toBe(true);
    expect(endpoint.tailscale.running).toBe(false);
  });

  test("automatic topology prints and gates on Tailscale", () => {
    const report: StatusReport = {
      tailscale: {
        required: true,
        state: "Running",
        fqdn: "host.tail.ts.net",
        running: true,
      },
      coordAgentLoaded: true,
      workerAgentLoaded: true,
      coord: { reachable: true, gitSha: "abcdef123456" },
      workers: [],
      tlsMode: "tailscale-serve",
      url: "https://host.tail.ts.net:4102",
      handoff: null,
    };
    const lines = renderedStatus(report);

    expect(lines).toContain("  ✓ tailscale: Running (host.tail.ts.net)");
    expect(lines).toContain("  ✓ coord TLS: tailscale serve");
    expect(lines.join("\n")).not.toContain("mint: tailscale cert");
    expect(statusReportIsHealthy(report)).toBe(true);
    expect(statusReportIsHealthy({
      ...report,
      tailscale: { ...report.tailscale, running: false },
    })).toBe(false);
  });

  test("direct topology omits Tailscale output and does not gate on it", () => {
    const report: StatusReport = {
      tailscale: {
        required: false,
        state: "NotRequired",
        fqdn: null,
        running: false,
      },
      coordAgentLoaded: true,
      workerAgentLoaded: true,
      coord: { reachable: true, gitSha: null },
      workers: [],
      tlsMode: "direct",
      url: "https://coord.example.test:7443",
      handoff: null,
    };
    const lines = renderedStatus(report);

    expect(lines.join("\n")).not.toContain("tailscale:");
    expect(lines).toContain("  open: https://coord.example.test:7443");
    expect(statusReportIsHealthy(report)).toBe(true);
  });
});

describe("status coordinator liveness", () => {
  test("uses the public identity contract accepted by managed coordinators", async () => {
    let requestedUrl = "";
    const result = await _probeCoordinatorIdentity(
      "https://coord.example.test/roost.v1.CoordinatorService/AuthCoordIdentity",
      testFetch(async (input) => {
        requestedUrl = String(input);
        return Response.json({ gitSha: "abcdef123456", saasMode: true });
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
  test("reports active workers and excludes referential tombstones", () => {
    const root = mkdtempSync(join(tmpdir(), "roost-status-workers-"));
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
            keeper_stale TEXT,
            last_seen_ms INTEGER NOT NULL,
            deleted_at_ms INTEGER
          );
          INSERT INTO workers VALUES
            ('active-fp', 'active', 'linux', 'active.test', 'abc', '', 1000, NULL),
            ('deleted-fp', 'deleted', 'darwin', 'deleted.test', 'def', '', 2000, 3000);
        `);
      } finally {
        sqlite.close();
      }

      expect(workerInventory(databasePath).map((worker) => worker.fingerprint))
        .toEqual(["active-fp"]);
    } finally {
      rmSync(root, { recursive: true, force: true });
    }
  });
});

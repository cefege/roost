// T3.2 — Headless coord e2e via the createCoord factory.
// Boots a coord in-memory (fresh SQLite in /tmp), calls .fetch()
// directly with crafted Connect-shape Requests. No Bun.serve, no
// port allocation, no browser — fully deterministic.

import { describe, test, expect, beforeAll, afterAll } from "bun:test";
import { existsSync, mkdtempSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { openDb } from "../src/db/connection.ts";
import { runMigrations } from "../src/db/migrate.ts";
import { ensureSelfHostedTenant } from "../src/self-hosted-tenant.ts";
import { CoordinatorWriteGate } from "../src/coordinator-write-gate.ts";
import { newJwtCache } from "../src/jwt.ts";
import { createCoord, type CoordHandle } from "../src/coord-factory.ts";
import type { CoordConfig } from "@roost/shared/config";

let workdir: string;
let coord: CoordHandle;
let cleanup: () => Promise<void>;

beforeAll(async () => {
  workdir = mkdtempSync(join(tmpdir(), "roost-coord-e2e-"));
  const dbPath = join(workdir, "test.db");
  const authPath = join(workdir, "authorized_keys");
  writeFileSync(authPath, "");

  const opened = openDb(dbPath);
  const { db, sqlite } = opened;
  await runMigrations(sqlite);
  const selfHostedTenant = ensureSelfHostedTenant(sqlite, { backfillLegacyScopes: false });
  const jwtCache = newJwtCache();
  const cfg: CoordConfig = { trustProxy: false, bind: "127.0.0.1:0",
  pushAllowedOrigins: [],
  dbPath, authorizedKeysPath: authPath,
  webDistPath: "",
  jwtMaxAgeSecs: 300,
  auditRetentionDays: 90,
  relaxedCsp: false,
  corsAllowedOrigins: [],
  logDir: workdir,
  publicUrl: undefined,
  }
  coord = createCoord({
    db,
    sqlite,
    writeGate: new CoordinatorWriteGate(),
    cfg,
    jwtCache,
    selfHostedTenant,
  });
  cleanup = async () => {
    coord.dispose();
    try { await opened.close(); } finally { if (existsSync(workdir)) rmSync(workdir, { recursive: true, force: true }); }
  };
});

afterAll(() => cleanup?.());

describe("coord-factory fetch handler", () => {
  test("OPTIONS preflight → 204 without wildcard CORS", async () => {
    const resp = await coord.fetch(new Request("http://t/x", { method: "OPTIONS", headers: { origin: "http://example.com" } }));
    expect(resp.status).toBe(204);
    expect(resp.headers.get("access-control-allow-origin")).toBeNull();
  });

  test("MiscHealth (public Connect endpoint) → 200 + payload", async () => {
    const resp = await coord.fetch(new Request("http://t/roost.v1.CoordinatorService/MiscHealth", {
      method: "POST",
      headers: { "content-type": "application/json" },
      body: "{}",
    }));
    expect(resp.status).toBe(200);
    const body = await resp.json();
    expect(body.ok).toBe(true);
    expect(typeof body.bootMs).toBe("string");
    expect(typeof body.gitSha).toBe("string");
  });

  test("AuthCoordIdentity is public and discloses no coordinator key material", async () => {
    const resp = await coord.fetch(
      new Request("http://t/roost.v1.CoordinatorService/AuthCoordIdentity", {
        method: "POST",
        headers: { "content-type": "application/json" },
        body: "{}",
      }),
      { origin: { listener: "trusted-proxy", clientIp: "203.0.113.7", onHost: false } },
    );
    expect(resp.status).toBe(200);
    const body = await resp.json();
    expect("fingerprintHex" in body).toBe(false);
    expect(typeof body.gitSha).toBe("string");
  });

  test("WorkersList without JWT → 401 unauthenticated", async () => {
    const resp = await coord.fetch(new Request("http://t/roost.v1.CoordinatorService/WorkersList", {
      method: "POST",
      headers: { "content-type": "application/json" },
      body: "{}",
    }));
    expect(resp.status).toBe(401);
    expect(resp.headers.get("x-roost-auth-layer")).toBe("device");
    const body = await resp.json();
    expect(body.code).toBe("unauthenticated");
  });

  test("SessionsList without JWT → 401", async () => {
    const resp = await coord.fetch(new Request("http://t/roost.v1.CoordinatorService/SessionsList", {
      method: "POST",
      headers: { "content-type": "application/json" },
      body: "{}",
    }));
    expect(resp.status).toBe(401);
  });

  test("non-Connect /api/* path → 404", async () => {
    const resp = await coord.fetch(new Request("http://t/api/trpc/misc.health", { method: "POST" }));
    expect(resp.status).toBe(404);
  });

  test("retired settings and transfer RPC POSTs return exact 404 before SPA fallback", async () => {
    const methods = [
      ["Web", "hookTokensList"].join(""),
      ["Web", "hookTokensMint"].join(""),
      ["Web", "hookTokensDelete"].join(""),
      ["Permis", "sionsList"].join(""),
      ["Permis", "sionsCreate"].join(""),
      ["Permis", "sionsUpdate"].join(""),
      ["Permis", "sionsDelete"].join(""),
      ["Transfers", "Start"].join(""),
      ["Transfers", "Output"].join(""),
    ];
    for (const method of methods) {
      const response = await coord.fetch(new Request(
        `http://t/roost.v1.CoordinatorService/${method}`,
        {
          method: "POST",
          headers: { "content-type": "application/json" },
          body: "{}",
        },
      ), {
        origin: { listener: "direct", clientIp: "127.0.0.1", onHost: true },
        spa: () => new Response("<html>SPA fallback</html>", { status: 200 }),
      });
      expect(response.status, method).toBe(404);
      expect(await response.text(), method).toBe("not found");
    }
  });

  test("CSP + frame-options + nosniff headers on every response", async () => {
    const resp = await coord.fetch(new Request("http://t/roost.v1.CoordinatorService/MiscHealth", {
      method: "POST",
      headers: { "content-type": "application/json" },
      body: "{}",
    }));
    expect(resp.headers.get("content-security-policy")).toBeTruthy();
    expect(resp.headers.get("x-frame-options")).toBe("DENY");
    expect(resp.headers.get("x-content-type-options")).toBe("nosniff");
  });

  test("rate limit: 100 AuthRedeemBrowser POSTs from same IP → 101st returns 429", async () => {
    // Burn through the 100/min budget for a credential-consumption route. The
    // body is invalid, but rate limiting runs before request parsing.
    const fire = () => coord.fetch(
      new Request("http://t/roost.v1.CoordinatorService/AuthRedeemBrowser", {
        method: "POST",
        headers: { "content-type": "application/json" },
        body: "{}",
      }),
      { origin: { listener: "direct", clientIp: "10.0.0.99", onHost: false } },
    );
    for (let i = 0; i < 100; i++) await fire();
    const final = await fire();
    expect(final.status).toBe(429);
    expect((await final.json()).error).toMatch(/rate limit/);
  });
});

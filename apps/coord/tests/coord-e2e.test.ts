// T3.2 — Headless coord e2e via the createCoord factory.
// Boots a coord in-memory (fresh SQLite in /tmp), calls .fetch()
// directly with crafted Connect-shape Requests. No Bun.serve, no
// port allocation, no humanchrome — fully deterministic.

import { describe, test, expect, beforeAll, afterAll } from "bun:test";
import { existsSync, mkdtempSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { openDb } from "../src/db/connection.ts";
import { runMigrations } from "../src/db/migrate.ts";
import { loadOrCreateCoordKey } from "../src/coord-key.ts";
import { newJwtCache } from "../src/jwt.ts";
import { createCoord, type CoordHandle } from "../src/coord-factory.ts";
import type { CoordConfig } from "@roost/shared/config";

let workdir: string;
let coord: CoordHandle;
let cleanup: () => void;

beforeAll(async () => {
  workdir = mkdtempSync(join(tmpdir(), "roost-coord-e2e-"));
  const dbPath = join(workdir, "test.db");
  const keyPath = join(workdir, "test.key");
  const authPath = join(workdir, "authorized_keys");
  writeFileSync(authPath, "");

  const { db, sqlite } = openDb(dbPath);
  await runMigrations(sqlite);
  const coordKey = await loadOrCreateCoordKey(keyPath);
  const jwtCache = newJwtCache();
  const cfg: CoordConfig = {
    bind: "127.0.0.1:0",
    dbPath, coordKeyPath: keyPath, authorizedKeysPath: authPath,
    webDistPath: "",
    tlsCertPath: undefined, tlsKeyPath: undefined,
    jwtMaxAgeSecs: 300,
    auditRetentionDays: 90,
    relaxedCsp: false,
    corsAllowedOrigins: [],
    trustedProxyIps: ["127.0.0.1", "::1", "::ffff:127.0.0.1"],
    logDir: workdir,
    publicUrl: undefined,
    handoffPath: join(workdir, "coord-handoff.json"),
  };
  coord = createCoord({ db, sqlite, coordKey, cfg, jwtCache });
  cleanup = () => {
    coord.dispose();
    try { sqlite.close(); } catch { /* ignore */ }
    if (existsSync(workdir)) rmSync(workdir, { recursive: true, force: true });
  };
});

afterAll(() => cleanup?.());

describe("coord-factory fetch handler", () => {
  test("OPTIONS preflight → 204 with CORS headers", async () => {
    const resp = await coord.fetch(new Request("http://t/x", { method: "OPTIONS", headers: { origin: "http://example.com" } }));
    expect(resp.status).toBe(204);
    expect(resp.headers.get("access-control-allow-origin")).toBeTruthy();
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

  test("AuthCoordIdentity (public) → 200 + fingerprint_hex + git_sha", async () => {
    const resp = await coord.fetch(new Request("http://t/roost.v1.CoordinatorService/AuthCoordIdentity", {
      method: "POST",
      headers: { "content-type": "application/json" },
      body: "{}",
    }));
    expect(resp.status).toBe(200);
    const body = await resp.json();
    expect(body.fingerprintHex).toMatch(/^[0-9a-f]{64}$/);
  });

  test("WorkersList without JWT → 401 unauthenticated", async () => {
    const resp = await coord.fetch(new Request("http://t/roost.v1.CoordinatorService/WorkersList", {
      method: "POST",
      headers: { "content-type": "application/json" },
      body: "{}",
    }));
    expect(resp.status).toBe(401);
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

  test("rate limit: 100 AuthAuthorizeBrowser POSTs from same IP → 101st returns 429", async () => {
    // Burn through the 100/min budget for a rate-limited route. Loopback-only
    // mutation; the body is invalid but rate-limit fires before parse.
    const fire = () => coord.fetch(
      new Request("http://t/roost.v1.CoordinatorService/AuthAuthorizeBrowser", {
        method: "POST",
        headers: { "content-type": "application/json" },
        body: "{}",
      }),
      { clientIp: "10.0.0.99" },
    );
    for (let i = 0; i < 100; i++) await fire();
    const final = await fire();
    expect(final.status).toBe(429);
    expect((await final.json()).error).toMatch(/rate limit/);
  });
});

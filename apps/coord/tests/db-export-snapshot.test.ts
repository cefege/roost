// Pins /api/db-export's file-backed response and the export-snapshot sweep.
// The response body must stay a file so Bun streams it instead of buffering a
// whole database into heap, which means the snapshot outlives the handler and
// the sweep is what bounds the residue. Drives the real listener through the
// _serve seam, the same way bun-coordinator-request-timeout.test.ts does.

import { afterEach, describe, expect, test } from "bun:test";
import { Database } from "bun:sqlite";
import { randomUUID } from "node:crypto";
import {
  existsSync,
  mkdirSync,
  mkdtempSync,
  readdirSync,
  rmSync,
  statSync,
  utimesSync,
  writeFileSync,
} from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import {
  _sweepExportSnapshots,
  EXPORT_SNAPSHOT_TTL_MS,
  startBunCoordinatorListeners,
} from "../src/bun-coordinator-listeners.ts";
import type { CallerOrigin } from "../src/middleware/caller-origin.ts";

const EXPORT_SNAPSHOT_GLOB_PREFIX = ".coord-export-";
const SNAPSHOT_AGE_PAST_TTL_MS = 30 * 60_000;

interface ListenerServer {
  port: number;
  requestIP(request: Request): { address: string };
  upgrade(): boolean;
  timeout(request: Request, seconds: number): void;
}

interface CapturedServeOptions {
  fetch?: (
    request: Request,
    server: ListenerServer,
  ) => Response | undefined | Promise<Response | undefined>;
}

const tempDirs: string[] = [];
const openDatabases: Database[] = [];

afterEach(() => {
  for (const db of openDatabases.splice(0)) db.close();
  for (const dir of tempDirs.splice(0)) rmSync(dir, { recursive: true, force: true });
});

/** A coordinator data directory holding a real, non-empty database. */
function makeDbDir(): { dir: string; dbPath: string; sqlite: Database } {
  const dir = mkdtempSync(join(tmpdir(), "roost-db-export-"));
  tempDirs.push(dir);
  const dbPath = join(dir, "coordinator_v2.db");
  const sqlite = new Database(dbPath, { create: true });
  openDatabases.push(sqlite);
  sqlite.exec("PRAGMA journal_mode=WAL");
  sqlite.exec("create table probe (id integer primary key, payload text)");
  sqlite.exec("insert into probe (payload) values ('a'), ('b'), ('c')");
  return { dir, dbPath, sqlite };
}

function writeExportSnapshot(dir: string, payload: string): string {
  const path = join(dir, `${EXPORT_SNAPSHOT_GLOB_PREFIX}${randomUUID()}.db`);
  writeFileSync(path, payload);
  return path;
}

function exportSnapshotNames(dir: string): string[] {
  return readdirSync(dir).filter((name) => name.startsWith(EXPORT_SNAPSHOT_GLOB_PREFIX));
}

function makeListenerFixture(fixtureDb: { dbPath: string; sqlite: Database }) {
  const serveOptions: CapturedServeOptions[] = [];
  const server: ListenerServer = {
    port: 4104,
    requestIP: () => ({ address: "127.0.0.1" }),
    upgrade: () => false,
    timeout(): void {},
  };
  const serve = ((options: unknown) => {
    serveOptions.push(options as CapturedServeOptions);
    return server;
  }) as unknown as typeof Bun.serve;

  startBunCoordinatorListeners({
    cfg: {
      bind: "127.0.0.1:4104",
      trustProxy: false,
      dbPath: fixtureDb.dbPath,
      authorizedKeysPath: "/tmp/authorized_keys",
      webDistPath: undefined,
      jwtMaxAgeSecs: 300,
      auditRetentionDays: 90,
      corsAllowedOrigins: [],
      pushAllowedOrigins: [],
      relaxedCsp: false,
      logDir: "/tmp",
      publicUrl: "https://coord.example",
      webPublicUrl: "https://dashboard.example",
    },
    coord: {
      // The listener owns the export closure; this stub is the coordinator's
      // route for it, so calling it also proves the wiring and the on-host
      // origin the listener resolved for a loopback peer.
      async fetch(
        _request: Request,
        context?: {
          origin: CallerOrigin;
          dbExport?: (origin: CallerOrigin) => Promise<Response>;
        },
      ): Promise<Response> {
        if (!context?.dbExport) throw new Error("listener wired no db-export handler");
        return context.dbExport(context.origin);
      },
      dispose() {},
    },
    sqlite: fixtureDb.sqlite,
    workerDeps: {},
    syncDeps: {},
    workerWs: { open() {}, message() {}, close() {} },
    syncWs: { open() {}, message() {}, drain() {}, close() {} },
    spa: () => new Response("spa"),
    _serve: serve,
  } as unknown as Parameters<typeof startBunCoordinatorListeners>[0]);

  const options = serveOptions[0];
  if (!options) throw new Error("listener was never constructed");
  return { options, server };
}

async function dispatch(
  options: CapturedServeOptions,
  server: ListenerServer,
  request: Request,
): Promise<Response> {
  if (!options.fetch) throw new Error("captured listener has no fetch handler");
  const response = await options.fetch(request, server);
  if (!response) throw new Error("request unexpectedly upgraded");
  return response;
}

describe("export snapshot sweep", () => {
  test("a full sweep removes every export snapshot and nothing else", () => {
    const { dir } = makeDbDir();
    mkdirSync(join(dir, "backups"));
    const spared = readdirSync(dir).sort();
    expect(spared).toContain("coordinator_v2.db");
    expect(spared).toContain("coordinator_v2.db-wal");
    expect(spared).toContain("backups");
    const first = writeExportSnapshot(dir, "snapshot-one");
    const second = writeExportSnapshot(dir, "snapshot-two");

    expect(_sweepExportSnapshots(dir, 0)).toBe(2);

    expect(existsSync(first)).toBe(false);
    expect(existsSync(second)).toBe(false);
    expect(readdirSync(dir).sort()).toEqual(spared);
  });

  test("the age bound spares a snapshot still inside the download window", () => {
    const { dir } = makeDbDir();
    const aged = writeExportSnapshot(dir, "aged");
    const fresh = writeExportSnapshot(dir, "fresh");
    const agedSeconds = (Date.now() - SNAPSHOT_AGE_PAST_TTL_MS) / 1_000;
    utimesSync(aged, agedSeconds, agedSeconds);

    expect(_sweepExportSnapshots(dir, EXPORT_SNAPSHOT_TTL_MS)).toBe(1);

    expect(existsSync(aged)).toBe(false);
    expect(existsSync(fresh)).toBe(true);
  });

  test("the count bound keeps only the newest snapshots inside the window", () => {
    const { dir } = makeDbDir();
    // All three are young enough to survive the age bound, so only the count
    // bound can stop three full database copies from sharing one small disk.
    const oldest = writeExportSnapshot(dir, "oldest");
    const middle = writeExportSnapshot(dir, "middle");
    const newest = writeExportSnapshot(dir, "newest");
    const nowSeconds = Date.now() / 1_000;
    utimesSync(oldest, nowSeconds - 120, nowSeconds - 120);
    utimesSync(middle, nowSeconds - 60, nowSeconds - 60);
    utimesSync(newest, nowSeconds, nowSeconds);

    expect(_sweepExportSnapshots(dir, EXPORT_SNAPSHOT_TTL_MS, 1)).toBe(2);

    expect(existsSync(oldest)).toBe(false);
    expect(existsSync(middle)).toBe(false);
    expect(existsSync(newest)).toBe(true);
  });

  test("a data directory that does not exist yet sweeps nothing", () => {
    const { dir } = makeDbDir();

    expect(_sweepExportSnapshots(join(dir, "absent"), 0)).toBe(0);
  });

  test("constructing the listener drops a crash-leaked snapshot", () => {
    const fixtureDb = makeDbDir();
    const leaked = writeExportSnapshot(fixtureDb.dir, "leaked-by-a-kill");

    makeListenerFixture(fixtureDb);

    expect(existsSync(leaked)).toBe(false);
  });
});

describe("db-export response", () => {
  test("an on-host export serves the snapshot file with its real length", async () => {
    const fixtureDb = makeDbDir();
    const fixture = makeListenerFixture(fixtureDb);

    const response = await dispatch(
      fixture.options,
      fixture.server,
      new Request("https://coord.example/api/db-export", {
        headers: { host: "127.0.0.1:4104" },
      }),
    );

    expect(response.status).toBe(200);
    expect(response.headers.get("content-type")).toBe("application/x-sqlite3");
    expect(response.headers.get("content-disposition"))
      .toBe(`attachment; filename="coordinator_v2.db"`);

    // The body is still on disk: unlinking before Bun reads it would truncate
    // the download, so the sweep and the TTL timer own the cleanup.
    const snapshots = exportSnapshotNames(fixtureDb.dir);
    expect(snapshots).toHaveLength(1);
    const snapshotPath = join(fixtureDb.dir, snapshots[0] as string);

    const bytes = new Uint8Array(await response.arrayBuffer());
    expect(response.headers.get("content-length")).toBe(String(bytes.byteLength));
    expect(statSync(snapshotPath).size).toBe(bytes.byteLength);

    const downloadPath = join(fixtureDb.dir, "downloaded.db");
    writeFileSync(downloadPath, bytes);
    const downloaded = new Database(downloadPath, { readonly: true });
    openDatabases.push(downloaded);
    expect(downloaded.query("PRAGMA integrity_check").get())
      .toEqual({ integrity_check: "ok" });
    expect(downloaded.query("select count(*) as rows from probe").get())
      .toEqual({ rows: 3 });
  });

  test("an off-host caller is refused before any snapshot is written", async () => {
    const fixtureDb = makeDbDir();
    const fixture = makeListenerFixture(fixtureDb);
    fixture.server.requestIP = () => ({ address: "203.0.113.9" });

    const response = await dispatch(
      fixture.options,
      fixture.server,
      new Request("https://coord.example/api/db-export", {
        headers: { host: "127.0.0.1:4104" },
      }),
    );

    expect(response.status).toBe(403);
    expect(await response.json()).toEqual({ error: "on-host only" });
    expect(exportSnapshotNames(fixtureDb.dir)).toEqual([]);
  });
});

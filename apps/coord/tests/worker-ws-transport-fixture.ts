// Centralizes isolated server setup so split worker transport suites do not duplicate it.
// The sibling worker-ws-transport test files call this fixture without sharing mutable state.
// It owns temporary databases and servers and depends on the real coord transport handlers.

import { type HandlerContext } from "@connectrpc/connect";
import { create, fromBinary, toBinary } from "@bufbuild/protobuf";
import { existsSync, mkdtempSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import {
  CoordWorkerDownSchema,
  CoordWorkerUpSchema,
  WHelloSchema,
  WSessionEventSchema,
  type CoordWorkerDown,
  type CoordWorkerUp,
} from "@roost/shared/proto/worker_transport_pb";
import type { CoordConfig } from "@roost/shared/config";
import { fingerprintOf } from "@roost/shared/fingerprint";
import { eventToProto } from "@roost/shared/wire/event-proto";
import { SessionEvent, asWorkerFp } from "@roost/shared/wire";
import { WORKER_AUTH_SUBPROTOCOL } from "@roost/shared/wire/coord-worker";
import { loadOrCreateCoordKey } from "../src/coord-key.ts";
import { openDb } from "../src/db/connection.ts";
import { runMigrations } from "../src/db/migrate.ts";
import { signJwt, newJwtCache } from "../src/jwt.ts";
import {
  handleWorkerWsUpgrade,
  makeWorkerWsHandler,
} from "../src/connect/worker-ws-handler.ts";
import type { WorkerServiceDeps } from "../src/connect/worker-service.ts";
import { callerKey, dashboardActorKey } from "../src/connect/auth-interceptor.ts";
import { PasswordWorkGate } from "../src/connect/password-work-gate.ts";
import type { ConnectDeps } from "../src/connect/router.ts";
import { PendingEventPublicationStore } from "../src/pending-event-publications.ts";

export interface TestWorkerConnection {
  ws: WebSocket;
  opened: Promise<void>;
  sendUp(frame: CoordWorkerUp): void;
  waitFor(
    predicate: (frame: CoordWorkerDown) => boolean,
    timeoutMs?: number,
  ): Promise<CoordWorkerDown>;
  close(): void;
}

export const helloFrame = (fingerprint: string) => create(CoordWorkerUpSchema, {
  frame: {
    case: "hello",
    value: create(WHelloSchema, { workerFp: fingerprint, version: "test" }),
  },
});

export async function startWorkerWsTransportFixture() {
  const workdir = mkdtempSync(join(tmpdir(), "roost-ws-transport-"));
  const dbPath = join(workdir, "test.db");
  const keyPath = join(workdir, "test.key");
  const authPath = join(workdir, "authorized_keys");
  writeFileSync(authPath, "");

  const opened = openDb(dbPath);
  const db = opened.db;
  const sqlite = opened.sqlite;
  await runMigrations(sqlite);
  const coordKey = await loadOrCreateCoordKey(keyPath);
  const jwtCache = newJwtCache();
  const cfg: CoordConfig = {
    trustProxy: false,
    bind: "127.0.0.1:0",
    saasMode: false,
    managedContainer: false,
    pushAllowedOrigins: [],
    dbPath,
    coordKeyPath: keyPath,
    authorizedKeysPath: authPath,
    webDistPath: "",
    tlsCertPath: undefined,
    tlsKeyPath: undefined,
    jwtMaxAgeSecs: 300,
    auditRetentionDays: 90,
    relaxedCsp: false,
    corsAllowedOrigins: [],
    logDir: workdir,
    publicUrl: undefined,
    handoffPath: join(workdir, "coord-handoff.json"),
  };
  const deps: WorkerServiceDeps = {
    db,
    pendingPublications: new PendingEventPublicationStore(),
    jwtCache,
    cfg,
  };
  const connectDeps: ConnectDeps = {
    ...deps,
    sqlite,
    coordKey,
    passwordWorkGate: new PasswordWorkGate(),
  };

  const workerKeys = await crypto.subtle.generateKey(
    { name: "Ed25519" },
    true,
    ["sign", "verify"],
  );
  const rawPublicKey = new Uint8Array(
    await crypto.subtle.exportKey("raw", workerKeys.publicKey),
  );
  const workerFp = await fingerprintOf(rawPublicKey);
  await db.insertInto("authorized_keys").values({
    fingerprint: workerFp,
    public_key: rawPublicKey,
    label: "test-worker",
    added_at: Date.now(),
  }).execute();
  const organizationId = "worker-ws-transport-org";
  const dashboardId = "worker-ws-transport-dashboard";
  const dashboardNow = Date.now();
  await db.insertInto("organizations").values({
    id: organizationId,
    slug: "worker-ws-transport-org",
    name: "Worker transport",
    status: "active",
    created_at_ms: dashboardNow,
  }).execute();
  await db.insertInto("dashboards").values({
    id: dashboardId,
    organization_id: organizationId,
    slug: "worker-ws-transport",
    name: "Worker transport",
    status: "active",
    created_at_ms: dashboardNow,
  }).execute();
  await db.insertInto("workers").values({
    fp: workerFp,
    dashboard_id: dashboardId,
    label: "test-worker",
    os: "linux",
    git_sha: null,
    host_metrics_json: null,
    registered_at_ms: Date.now(),
    last_seen_ms: Date.now(),
    reachable_addr: null,
    keeper_stale: null,
  }).execute();
  const now = Math.floor(Date.now() / 1000);
  const workerJwt = await signJwt(
    { aud: "roost-coordinator", sub: workerFp, iat: now, exp: now + 60 },
    workerKeys.privateKey,
    workerFp,
  );

  const serverOpenWaiters: Array<PromiseWithResolvers<void>> = [];
  const workerWsHandler = makeWorkerWsHandler(deps);
  const fenceWorkerSockets = workerWsHandler.fenceForFingerprint;
  const closeWorkerSockets = workerWsHandler.closeForFingerprint;
  const synchronizedWorkerWsHandler = {
    ...workerWsHandler,
    open(ws: Parameters<typeof workerWsHandler.open>[0]): void {
      workerWsHandler.open(ws);
      const waiter = serverOpenWaiters.shift();
      if (ws.data.conn) {
        waiter?.resolve();
      } else {
        waiter?.reject(new Error(
          `worker server rejected open before initialization; auth deadline ${ws.data.authDeadlineAtMs}`,
        ));
      }
    },
  };
  const server = Bun.serve({
    hostname: "127.0.0.1",
    port: 0,
    async fetch(request, bunServer) {
      const upgrade = await handleWorkerWsUpgrade(request, bunServer, deps);
      if (upgrade !== null) return upgrade;
      return new Response("not found", { status: 404 });
    },
    websocket: synchronizedWorkerWsHandler,
  });
  const port = server.port!;
  let snapshotClientSeq = 10_000n;

  function connectWorker(fingerprint: string, token: string): TestWorkerConnection {
    const serverOpened = Promise.withResolvers<void>();
    serverOpenWaiters.push(serverOpened);
    const ws = new WebSocket(
      `ws://127.0.0.1:${port}/ws/coord-worker/${fingerprint}`,
      [WORKER_AUTH_SUBPROTOCOL, token],
    );
    let closedDetail = "";
    ws.onclose = (event) => {
      closedDetail = `${event.code}:${event.reason}`;
    };
    ws.binaryType = "arraybuffer";
    const downstreamFrames: CoordWorkerDown[] = [];
    const waiters: Array<{
      predicate: (frame: CoordWorkerDown) => boolean;
      resolve: (frame: CoordWorkerDown) => void;
    }> = [];
    ws.onmessage = (event) => {
      const frame = fromBinary(
        CoordWorkerDownSchema,
        new Uint8Array(event.data as ArrayBuffer),
      );
      downstreamFrames.push(frame);
      for (let index = waiters.length - 1; index >= 0; index--) {
        const waiter = waiters[index]!;
        if (waiter.predicate(frame)) {
          waiter.resolve(frame);
          waiters.splice(index, 1);
        }
      }
    };
    const clientOpened = new Promise<void>((resolve, reject) => {
      ws.onopen = () => setImmediate(resolve);
      ws.onerror = () => {
        const index = serverOpenWaiters.indexOf(serverOpened);
        if (index >= 0) serverOpenWaiters.splice(index, 1);
        reject(new Error("ws error/closed"));
      };
    });
    const openedPromise = Promise.all([
      clientOpened,
      serverOpened.promise,
    ]).then(() => {});
    return {
      ws,
      opened: openedPromise,
      sendUp(frame: CoordWorkerUp) {
        if (ws.readyState !== WebSocket.OPEN) {
          throw new Error(
            `worker test socket is not open (${ws.readyState}; ${closedDetail})`,
          );
        }
        ws.send(toBinary(CoordWorkerUpSchema, frame));
      },
      waitFor(
        predicate: (frame: CoordWorkerDown) => boolean,
        timeoutMs = 3000,
      ): Promise<CoordWorkerDown> {
        const matchingFrame = downstreamFrames.find(predicate);
        if (matchingFrame) return Promise.resolve(matchingFrame);
        return new Promise((resolve, reject) => {
          const timeout = setTimeout(
            () => reject(new Error("waitFor timeout")),
            timeoutMs,
          );
          waiters.push({
            predicate,
            resolve: (frame) => {
              clearTimeout(timeout);
              resolve(frame);
            },
          });
        });
      },
      close() {
        try {
          ws.close();
        } catch {
          // Closing an already-failed client is harmless during test cleanup.
        }
      },
    };
  }

  async function readyWorker(
    worker: TestWorkerConnection,
    fingerprint: string,
  ): Promise<void> {
    snapshotClientSeq += 1n;
    const clientSeq = snapshotClientSeq;
    const snapshot = SessionEvent.parse({
      kind: "snapshot",
      worker_fp: asWorkerFp(fingerprint),
      sessions: [],
      ts: Date.now(),
    });
    worker.sendUp(create(CoordWorkerUpSchema, {
      frame: {
        case: "event",
        value: create(WSessionEventSchema, {
          event: eventToProto(snapshot, 0)!,
          clientSeq,
        }),
      },
    }));
    await worker.waitFor((frame) =>
      frame.frame.case === "eventAck"
      && frame.frame.value.clientSeq === clientSeq
    );
  }

  function browserAuthContext(): HandlerContext {
    const caller = {
      kind: "account-device" as const,
      fingerprint: "browser-fp",
      label: "test",
      accountId: "browser-account",
    };
    const actor = {
      accountId: "browser-account",
      organizationId,
      dashboardId,
      organizationRole: "owner" as const,
      dashboardRole: "admin" as const,
      deviceFingerprint: "browser-fp",
    };
    return {
      values: {
        get: (key: unknown) => key === callerKey
          ? caller
          : key === dashboardActorKey
            ? actor
            : null,
      },
    } as unknown as HandlerContext;
  }

  return {
    browserAuthContext,
    cleanup: async (): Promise<void> => {
      try {
        server.stop(true);
      } catch {
        // A stopped fixture has no remaining server resources.
      }
      try {
        await opened.close();
      } finally {
        if (existsSync(workdir)) {
          rmSync(workdir, { recursive: true, force: true });
        }
      }
    },
    closeWorkerSockets,
    connectDeps,
    connectWorker,
    dashboardId,
    deps,
    fenceWorkerSockets,
    port,
    readyWorker,
    workerFp,
    workerJwt,
  };
}

export type WorkerWsTransportFixture = Awaited<
  ReturnType<typeof startWorkerWsTransportFixture>
>;

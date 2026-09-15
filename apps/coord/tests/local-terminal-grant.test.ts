// Focused coverage for the SessionsGrantLocalTerminal handler: route
// authorization against the named worker, the digest-only worker install, the
// ack gate in front of the secret, renewal replacement, and device revocation.
// Runs on an isolated SQLite with a fake worker handle installed through the
// registry's test seam, so no listener or network is involved.

import { afterAll, beforeAll, beforeEach, describe, expect, test } from "bun:test";
import { createHash } from "node:crypto";
import { mkdtempSync, rmSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { Code, ConnectError, createContextValues, type HandlerContext } from "@connectrpc/connect";
import { create } from "@bufbuild/protobuf";
import {
  SessionsGrantLocalTerminalRequestSchema,
  SessionsGrantLocalTerminalResponseSchema,
  type SessionsGrantLocalTerminalResponse,
} from "@roost/shared/proto/coordinator_pb";
import type {
  CoordWorkerDown,
  DLocalTerminalGrant,
} from "@roost/shared/proto/worker_transport_pb";
import { callerKey } from "../src/connect/auth-interceptor.ts";
import {
  LOCAL_TERMINAL_GRANT_TTL_MS,
  _localTerminalGrantLeases,
  _resetLocalTerminalGrants,
  makeSessionLocalTerminalGrantHandlers,
  revokeLocalTerminalGrantsForFingerprint,
  type LocalTerminalGrantHandlers,
} from "../src/connect/local-terminal-grants.ts";
import { __setConnectWorkerForTest } from "../src/connect/worker-registry.ts";
import { rejectPendingRpc, resolvePendingRpc } from "../src/router/pending-rpcs.ts";
import type { ConnectDeps } from "../src/connect/router.ts";
import { openDb, type DbHandle } from "../src/db/connection.ts";
import { runMigrations } from "../src/db/migrate.ts";
import { ensureSelfHostedTenant } from "../src/self-hosted-tenant.ts";

const LOCAL_WORKER_FP = "a".repeat(64);
const REMOTE_WORKER_FP = "b".repeat(64);
const DEVICE_FP = "c".repeat(64);
const TAB_ID = "local-tab";

interface StartedGrant {
  readonly response: Promise<SessionsGrantLocalTerminalResponse>;
  readonly frame: DLocalTerminalGrant;
}

const grantFrames: DLocalTerminalGrant[] = [];
const revokedFingerprints: string[] = [];

let workdir: string;
let opened: DbHandle;
let deps: ConnectDeps;
let accountId: string;
let dashboardId: string;
let handlers: LocalTerminalGrantHandlers;
let sessionSequence = 0;
let announceGrantFrame: ((frame: DLocalTerminalGrant) => void) | null = null;

function browserContext(): HandlerContext {
  const values = createContextValues();
  values.set(callerKey, {
    kind: "account-device",
    fingerprint: DEVICE_FP,
    label: "Loopback page",
    accountId,
  });
  return { values, signal: new AbortController().signal } as unknown as HandlerContext;
}

async function insertSession(
  workerFp: string,
  status: "open" | "closed" = "open",
): Promise<string> {
  sessionSequence += 1;
  const id = `00000000-0000-4000-8000-${String(sessionSequence).padStart(12, "0")}`;
  await opened.db.insertInto("sessions").values({
    id,
    dashboard_id: dashboardId,
    worker_fp: workerFp,
    channel: sessionSequence,
    kind: "shell",
    cwd: "/tmp",
    status,
    created_at: Date.now(),
  }).execute();
  return id;
}

/** The service impl may answer with a message init shape; normalize once so the
 *  assertions read the concrete response. */
function requestGrant(sessionIds: string[]): Promise<SessionsGrantLocalTerminalResponse> {
  return Promise.resolve(handlers.sessionsGrantLocalTerminal(
    create(SessionsGrantLocalTerminalRequestSchema, {
      sessionIds,
      workerFp: LOCAL_WORKER_FP,
      tabId: TAB_ID,
    }),
    browserContext(),
  )).then((value) => create(SessionsGrantLocalTerminalResponseSchema, value));
}

/** Drive the handler to the instant the worker holds the install and leave the
 *  pending RPC unsettled, so each caller decides how the worker answers. */
async function startGrant(sessionIds: string[]): Promise<StartedGrant> {
  const { promise: frameArrived, resolve } = Promise.withResolvers<DLocalTerminalGrant>();
  announceGrantFrame = resolve;
  const response = requestGrant(sessionIds);
  response.catch(() => {});
  // Racing the response surfaces a pre-send refusal as its own error instead of
  // waiting out the suite timeout on a frame that will never arrive.
  const frame = await Promise.race([
    frameArrived,
    response.then(() => {
      throw new Error("handler answered without installing a grant");
    }),
  ]);
  return { response, frame };
}

async function grantWithWorkerAck(
  sessionIds: string[],
): Promise<SessionsGrantLocalTerminalResponse> {
  const started = await startGrant(sessionIds);
  resolvePendingRpc(started.frame.requestId, {}, LOCAL_WORKER_FP);
  return started.response;
}

async function connectErrorFrom(work: Promise<unknown>): Promise<ConnectError> {
  const error = await work.then(() => null, (thrown: unknown) => thrown);
  if (!(error instanceof ConnectError)) {
    throw new Error(`expected a ConnectError, got ${String(error)}`);
  }
  return error;
}

function installFakeWorker(workerFp: string): void {
  __setConnectWorkerForTest(workerFp, {
    workerFp,
    send: (frame: CoordWorkerDown) => {
      if (frame.frame.case === "localTerminalGrant") {
        grantFrames.push(frame.frame.value);
        announceGrantFrame?.(frame.frame.value);
        announceGrantFrame = null;
      }
      if (frame.frame.case === "localTerminalGrantRevoke") {
        revokedFingerprints.push(frame.frame.value.deviceFingerprint);
      }
      return 1;
    },
  });
}

beforeAll(async () => {
  workdir = mkdtempSync(join(tmpdir(), "roost-local-grant-"));
  opened = openDb(join(workdir, "coord.db"));
  await runMigrations(opened.sqlite);
  const tenant = ensureSelfHostedTenant(opened.sqlite, { backfillLegacyScopes: false });
  accountId = tenant.accountId;
  dashboardId = tenant.dashboardId;
  const now = Date.now();
  await opened.db.insertInto("workers").values(
    ([[LOCAL_WORKER_FP, "local"], [REMOTE_WORKER_FP, "remote"]] as const).map(([fp, label]) => ({
      fp,
      dashboard_id: dashboardId,
      label,
      os: "linux",
      git_sha: null,
      host_metrics_json: null,
      registered_at_ms: now,
      last_seen_ms: now,
      reachable_addr: null,
    })),
  ).execute();
  deps = { db: opened.db, selfHostedTenant: tenant } as unknown as ConnectDeps;
  handlers = makeSessionLocalTerminalGrantHandlers(deps);
  for (const fp of [LOCAL_WORKER_FP, REMOTE_WORKER_FP]) installFakeWorker(fp);
});

afterAll(async () => {
  for (const fp of [LOCAL_WORKER_FP, REMOTE_WORKER_FP]) __setConnectWorkerForTest(fp, null);
  _resetLocalTerminalGrants();
  await opened?.close();
  rmSync(workdir, { recursive: true, force: true });
});

beforeEach(() => {
  _resetLocalTerminalGrants();
  grantFrames.length = 0;
  revokedFingerprints.length = 0;
  announceGrantFrame = null;
});

describe("sessionsGrantLocalTerminal", () => {
  test("refuses a session that lives on another worker without installing a grant", async () => {
    const remoteSession = await insertSession(REMOTE_WORKER_FP);
    const error = await connectErrorFrom(requestGrant([remoteSession]));
    expect(error.code).toBe(Code.PermissionDenied);
    expect(grantFrames).toHaveLength(0);
    expect(_localTerminalGrantLeases()).toHaveLength(0);
  });

  test("reports a closed session as not found", async () => {
    const closedSession = await insertSession(LOCAL_WORKER_FP, "closed");
    const error = await connectErrorFrom(requestGrant([closedSession]));
    expect(error.code).toBe(Code.NotFound);
    expect(grantFrames).toHaveLength(0);
  });

  test("rejects a grant request that names no session", async () => {
    const error = await connectErrorFrom(requestGrant([]));
    expect(error.code).toBe(Code.InvalidArgument);
    expect(grantFrames).toHaveLength(0);
  });

  test("reports an offline worker so the browser can stay on the coordinator", async () => {
    const session = await insertSession(LOCAL_WORKER_FP);
    __setConnectWorkerForTest(LOCAL_WORKER_FP, null);
    try {
      const error = await connectErrorFrom(requestGrant([session]));
      expect(error.code).toBe(Code.Unavailable);
      expect(error.rawMessage.startsWith("worker_offline:")).toBe(true);
    } finally {
      installFakeWorker(LOCAL_WORKER_FP);
    }
    expect(grantFrames).toHaveLength(0);
    expect(_localTerminalGrantLeases()).toHaveLength(0);
  });

  test("hands the worker only the secret's digest and the browser the secret", async () => {
    const session = await insertSession(LOCAL_WORKER_FP);
    const response = await grantWithWorkerAck([session]);

    expect(response.secret).toMatch(/^[0-9a-f]{64}$/);
    expect(response.ttlMs).toBe(LOCAL_TERMINAL_GRANT_TTL_MS);

    const frame = grantFrames[0]!;
    expect(frame.grantId).toBe(response.grantId);
    expect(frame.secretSha256).toBe(
      createHash("sha256").update(response.secret).digest("hex"),
    );
    expect(JSON.stringify(frame)).not.toContain(response.secret);
    expect(frame.sessionIds).toEqual([session]);
    expect(frame.deviceFingerprint).toBe(DEVICE_FP);
    expect(frame.tabId).toBe(TAB_ID);
    expect(frame.ttlMs).toBe(LOCAL_TERMINAL_GRANT_TTL_MS);

    const leases = _localTerminalGrantLeases();
    expect(leases).toHaveLength(1);
    expect(leases[0]).toMatchObject({
      grantId: response.grantId,
      workerFp: LOCAL_WORKER_FP,
      deviceFingerprint: DEVICE_FP,
      sessionIds: [session],
    });
    expect(JSON.stringify(leases)).not.toContain(response.secret);
  });

  test("returns no secret when the worker refuses the install", async () => {
    const session = await insertSession(LOCAL_WORKER_FP);
    const started = await startGrant([session]);
    rejectPendingRpc(started.frame.requestId, "keeper refused the grant", LOCAL_WORKER_FP);
    const error = await connectErrorFrom(started.response);

    expect(error.code).toBe(Code.Internal);
    expect(error.rawMessage.startsWith("worker_failed:")).toBe(true);
    expect(grantFrames).toHaveLength(1);
    expect(_localTerminalGrantLeases()).toHaveLength(0);
  });

  test("a renewal from the same device and tab replaces its predecessor", async () => {
    const first = await insertSession(LOCAL_WORKER_FP);
    const second = await insertSession(LOCAL_WORKER_FP);
    const initial = await grantWithWorkerAck([first]);
    const renewed = await grantWithWorkerAck([first, second]);

    expect(renewed.grantId).not.toBe(initial.grantId);
    expect(renewed.secret).not.toBe(initial.secret);
    expect(grantFrames).toHaveLength(2);
    const leases = _localTerminalGrantLeases();
    expect(leases).toHaveLength(1);
    expect(leases[0]).toMatchObject({
      grantId: renewed.grantId,
      sessionIds: [first, second],
    });
  });

  test("revoking the device drops its lease and tells the workers", async () => {
    const session = await insertSession(LOCAL_WORKER_FP);
    await grantWithWorkerAck([session]);

    expect(revokeLocalTerminalGrantsForFingerprint(DEVICE_FP)).toBe(1);
    expect(revokedFingerprints).toContain(DEVICE_FP);
    expect(_localTerminalGrantLeases()).toHaveLength(0);
  });
});

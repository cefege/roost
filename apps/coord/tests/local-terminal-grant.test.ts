// Focused SessionsGrantLocalTerminal boundary coverage: authenticated tab binding,
// durable session-route checks, worker ACK fencing, and capability response fields.
// Lease renewal, revocation, and retirement lifecycle behavior lives beside its
// composition owner in terminal-grant-owner.test.ts.

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
} from "@roost/protocol/proto/coordinator_pb";
import {
  TERMINAL_INPUT_ROUTE_CAPABILITY,
  TERMINAL_PEER_WEBRTC_CAPABILITY,
} from "@roost/protocol/terminal-peer";
import type { CoordWorkerDown, DLocalTerminalGrant } from "@roost/protocol/proto/worker_transport_pb";
import { callerKey, tabIdKey } from "../src/auth/auth-interceptor.ts";
import {
  makeSessionLocalTerminalGrantHandlers,
  type LocalTerminalGrantHandlers,
} from "../src/terminal/direct/local-terminal-grants.ts";
import {
  LOCAL_TERMINAL_GRANT_TTL_MS,
  TerminalGrantOwner,
} from "../src/terminal/direct/terminal-grant-owner.ts";
import { __setConnectWorkerForTest } from "../src/workers/worker-registry.ts";
import { resolvePendingRpc } from "../src/router/pending-rpcs.ts";
import type { ConnectDeps } from "../src/rpc/router.ts";
import { openDb, type DbHandle } from "../src/db/connection.ts";
import { runMigrations } from "../src/db/migrate.ts";
import { ensureSelfHostedTenant } from "../src/auth/self-hosted-tenant.ts";

const LOCAL_WORKER_FP = "a".repeat(64);
const REMOTE_WORKER_FP = "b".repeat(64);
const DEVICE_FP = "c".repeat(64);
const TAB_ID = "local-tab";
const LOCAL_EPOCH = "worker-epoch-local";
const REMOTE_EPOCH = "worker-epoch-remote";
const STUN_URL = "stun:stun.example.test:3478";

interface StartedGrant {
  readonly response: Promise<SessionsGrantLocalTerminalResponse>;
  readonly workerFp: string;
  readonly frame: DLocalTerminalGrant;
}

interface GrantRequestOptions {
  readonly workerFp?: string;
  readonly tabId?: string;
  readonly headerTabId?: string | undefined;
}

interface GrantTestConfig {
  terminalPeerEnabled: boolean;
  terminalPeerStunUrls: string[];
}

const grantFrames: Array<{ workerFp: string; frame: DLocalTerminalGrant }> = [];
let announceGrantFrame: ((frame: { workerFp: string; frame: DLocalTerminalGrant }) => void) | null = null;
let workdir: string;
let opened: DbHandle;
let deps: ConnectDeps;
let accountId: string;
let dashboardId: string;
let handlers: LocalTerminalGrantHandlers;
let terminalGrants: TerminalGrantOwner;
let config: GrantTestConfig;
let sessionSequence = 0;

function browserContext(headerTabId: string | undefined): HandlerContext {
  const values = createContextValues();
  values.set(callerKey, {
    kind: "account-device",
    fingerprint: DEVICE_FP,
    label: "Terminal browser",
    accountId,
  });
  values.set(tabIdKey, headerTabId);
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

function requestGrant(
  sessionIds: string[],
  options: GrantRequestOptions = {},
): Promise<SessionsGrantLocalTerminalResponse> {
  const headerTabId = "headerTabId" in options ? options.headerTabId : TAB_ID;
  return Promise.resolve(handlers.sessionsGrantLocalTerminal(
    create(SessionsGrantLocalTerminalRequestSchema, {
      sessionIds,
      workerFp: options.workerFp ?? LOCAL_WORKER_FP,
      tabId: options.tabId ?? TAB_ID,
    }),
    browserContext(headerTabId),
  )).then((value) => create(SessionsGrantLocalTerminalResponseSchema, value));
}

async function startGrant(
  sessionIds: string[],
  options: GrantRequestOptions = {},
): Promise<StartedGrant> {
  const { promise: frameArrived, resolve } = Promise.withResolvers<{
    workerFp: string;
    frame: DLocalTerminalGrant;
  }>();
  announceGrantFrame = resolve;
  const response = requestGrant(sessionIds, options);
  response.catch(() => {});
  const received = await Promise.race([
    frameArrived,
    response.then(() => {
      throw new Error("handler answered without installing a grant");
    }),
  ]);
  return { response, ...received };
}

async function grantWithWorkerAck(
  sessionIds: string[],
  options: GrantRequestOptions = {},
): Promise<SessionsGrantLocalTerminalResponse> {
  const started = await startGrant(sessionIds, options);
  resolvePendingRpc(started.frame.requestId, {}, started.workerFp);
  return started.response;
}

async function connectErrorFrom(work: Promise<unknown>): Promise<ConnectError> {
  const error = await work.then(() => null, (thrown: unknown) => thrown);
  if (!(error instanceof ConnectError)) throw new Error(`expected a ConnectError, got ${String(error)}`);
  return error;
}

function installFakeWorker(
  workerFp: string,
  processEpoch: string | null,
  capabilities: readonly string[] = [],
): void {
  __setConnectWorkerForTest(workerFp, {
    workerFp,
    processEpoch,
    capabilities: new Set(capabilities),
    send: (frame: CoordWorkerDown) => {
      if (frame.frame.case === "localTerminalGrant") {
        const received = { workerFp, frame: frame.frame.value };
        grantFrames.push(received);
        announceGrantFrame?.(received);
        announceGrantFrame = null;
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
});

afterAll(async () => {
  terminalGrants.dispose();
  __setConnectWorkerForTest(LOCAL_WORKER_FP, null);
  __setConnectWorkerForTest(REMOTE_WORKER_FP, null);
  await opened.close();
  rmSync(workdir, { recursive: true, force: true });
});

beforeEach(() => {
  terminalGrants?.dispose();
  terminalGrants = new TerminalGrantOwner();
  config = { terminalPeerEnabled: true, terminalPeerStunUrls: [STUN_URL] };
  deps = {
    db: opened.db,
    selfHostedTenant: { accountId, dashboardId },
    cfg: config,
    terminalGrants,
  } as unknown as ConnectDeps;
  handlers = makeSessionLocalTerminalGrantHandlers(deps);
  installFakeWorker(LOCAL_WORKER_FP, LOCAL_EPOCH, [
    TERMINAL_PEER_WEBRTC_CAPABILITY,
    TERMINAL_INPUT_ROUTE_CAPABILITY,
  ]);
  installFakeWorker(REMOTE_WORKER_FP, REMOTE_EPOCH);
  grantFrames.length = 0;
  announceGrantFrame = null;
});

describe("sessionsGrantLocalTerminal", () => {
  test("requires the request tab to equal the authenticated interceptor tab", async () => {
    const session = await insertSession(LOCAL_WORKER_FP);
    const mismatch = await connectErrorFrom(requestGrant([session], { tabId: "other-tab" }));
    const missing = await connectErrorFrom(requestGrant([session], { headerTabId: undefined }));

    expect(mismatch.code).toBe(Code.PermissionDenied);
    expect(missing.code).toBe(Code.InvalidArgument);
    expect(grantFrames).toHaveLength(0);
  });

  test("caps request strings and unique session membership before worker install", async () => {
    const session = await insertSession(LOCAL_WORKER_FP);
    const tooLong = await connectErrorFrom(requestGrant([session], { tabId: "x".repeat(129) }));
    const duplicate = await connectErrorFrom(requestGrant([session, session]));
    const tooMany = await connectErrorFrom(requestGrant(
      Array.from({ length: 257 }, (_, index) => `session-${index}`),
    ));

    expect(tooLong.code).toBe(Code.InvalidArgument);
    expect(duplicate.code).toBe(Code.InvalidArgument);
    expect(tooMany.code).toBe(Code.InvalidArgument);
    expect(grantFrames).toHaveLength(0);
  });

  test("refuses closed or cross-worker sessions without installing a grant", async () => {
    const remoteSession = await insertSession(REMOTE_WORKER_FP);
    const closedSession = await insertSession(LOCAL_WORKER_FP, "closed");
    const wrongRoute = await connectErrorFrom(requestGrant([remoteSession]));
    const closed = await connectErrorFrom(requestGrant([closedSession]));

    expect(wrongRoute.code).toBe(Code.PermissionDenied);
    expect(closed.code).toBe(Code.NotFound);
    expect(grantFrames).toHaveLength(0);
  });

  test("returns a secret only after digest-only worker install and exposes supported capabilities", async () => {
    const session = await insertSession(LOCAL_WORKER_FP);
    const response = await grantWithWorkerAck([session]);
    const frame = grantFrames[0]!.frame;

    expect(response.secret).toMatch(/^[0-9a-f]{64}$/);
    expect(response.ttlMs).toBe(LOCAL_TERMINAL_GRANT_TTL_MS);
    expect(frame.secretSha256).toBe(createHash("sha256").update(response.secret).digest("hex"));
    expect(JSON.stringify(frame)).not.toContain(response.secret);
    expect(frame.workerEpoch).toBe(LOCAL_EPOCH);
    expect(response.workerEpoch).toBe(LOCAL_EPOCH);
    expect(response.peerSupported).toBe(true);
    expect(response.stunUrls).toEqual([STUN_URL]);
    expect(response.inputRouteSupported).toBe(true);
    expect(JSON.stringify(terminalGrants.list())).not.toContain(response.secret);
    expect(JSON.stringify(terminalGrants.list())).not.toContain(frame.secretSha256);
    expect(terminalGrants.list()).toMatchObject([{
      grantId: response.grantId,
      workerFp: LOCAL_WORKER_FP,
      workerEpoch: LOCAL_EPOCH,
      sessionIds: [session],
    }]);
  });

  test("keeps input-route support independent when peer setup is disabled", async () => {
    config.terminalPeerEnabled = false;
    const session = await insertSession(LOCAL_WORKER_FP);
    const response = await grantWithWorkerAck([session]);

    expect(response.peerSupported).toBe(false);
    expect(response.stunUrls).toEqual([]);
    expect(response.inputRouteSupported).toBe(true);
  });

  test("keeps rolling workers on unchanged base response fields", async () => {
    installFakeWorker(LOCAL_WORKER_FP, null, [
      TERMINAL_PEER_WEBRTC_CAPABILITY,
      TERMINAL_INPUT_ROUTE_CAPABILITY,
    ]);
    const session = await insertSession(LOCAL_WORKER_FP);
    const response = await grantWithWorkerAck([session]);

    expect(response.workerEpoch).toBe("");
    expect(response.peerSupported).toBe(false);
    expect(response.stunUrls).toEqual([]);
    expect(response.inputRouteSupported).toBe(false);
    expect(grantFrames[0]!.frame.workerEpoch).toBe("");
  });

  test("rechecks durable authorization after the worker ACK before returning a secret", async () => {
    const session = await insertSession(LOCAL_WORKER_FP);
    const started = await startGrant([session]);
    await opened.db.updateTable("sessions").set({ status: "closed" }).where("id", "=", session).execute();
    resolvePendingRpc(started.frame.requestId, {}, started.workerFp);
    const error = await connectErrorFrom(started.response);

    expect(error.code).toBe(Code.NotFound);
    expect(terminalGrants.list()).toEqual([]);
  });
});

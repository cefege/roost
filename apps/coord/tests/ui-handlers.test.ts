// UI handler contract: typed report/list state, the eight legacy dispatches,
// and one acknowledged layout apply with dashboard-bound session authorization.
// Bun drives the real handlers and UI bus against an isolated in-memory database.
// Explicit state/apply owners keep retained identities and target generations test-local.

import { describe, test, expect, beforeAll, beforeEach, afterAll } from "bun:test";
import { mkdtempSync, rmSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { create } from "@bufbuild/protobuf";
import { ConnectError, Code, createContextValues, type HandlerContext } from "@connectrpc/connect";
import {
  UiListStatesRequestSchema, UiDispatchRequestSchema, UiApplyLayoutRequestSchema,
} from "@roost/shared/proto/coordinator_pb";
import {
  UiReportStateRequestSchema, UiCommandSchema, UiSelectTabSchema,
  UiApplyLayoutSchema, UiApplyLayoutOutcome, UiApplyLayoutResultSchema,
} from "@roost/shared/proto/sync_pb";
import { layoutDocumentToProto } from "@roost/shared/layout-document-proto";
import { openDb } from "../src/db/connection.ts";
import { runMigrations } from "../src/db/migrate.ts";
import type { ConnectDeps } from "../src/connect/router.ts";
import {
  callerKey,
  dashboardActorKey,
  type DashboardActor,
} from "../src/connect/auth-interceptor.ts";
import { makeUiHandlers, type UiHandlers } from "../src/connect/handlers-ui.ts";
import { uiBus, type UiBusMsg } from "../src/buses.ts";
import { UiLayoutApplyOwner } from "../src/connect/ui-layout-apply-owner.ts";
import { UI_STATE_TTL_MS, UiStateOwner } from "../src/connect/ui-state-owner.ts";

let workdir: string;
let closeDb: () => Promise<void>;
let handlers: UiHandlers;
let uiLayoutApplies: UiLayoutApplyOwner;
let uiStates: UiStateOwner;

const DASHBOARD = "ui-handlers-dashboard";

function fakeAuthCtx(fingerprint: string): HandlerContext {
  const actor: DashboardActor = {
    accountId: "ui-handlers-account",
    organizationId: "ui-handlers-organization",
    dashboardId: DASHBOARD,
    organizationRole: "owner",
    dashboardRole: "admin",
    deviceFingerprint: fingerprint,
  };
  const values = createContextValues();
  values.set(callerKey, {
    kind: "account-device",
    fingerprint,
    label: "",
    accountId: actor.accountId,
  });
  values.set(dashboardActorKey, actor);
  return { values, signal: new AbortController().signal } as unknown as HandlerContext;
}
const authCtx = fakeAuthCtx("fp-test");

function layoutDocument(sessionId = "sess-9") {
  return layoutDocumentToProto({
    schema_version: 1,
    root: {
      kind: "leaf",
      leaf_key: "leaf-1",
      slot_keys: ["slot-1"],
      selected_slot_key: "slot-1",
    },
    focused_leaf_key: "leaf-1",
    bindings: [{ slot_key: "slot-1", session_id: sessionId }],
  });
}

function reportReq(tabId: string, activePath = "/s/sess-1") {
  return create(UiReportStateRequestSchema, {
    tabId,
    activePath,
    folderKey: "fp:/tmp/proj",
    layoutDocument: layoutDocument(),
  });
}

function selectTabCmd(sessionId: string) {
  return create(UiCommandSchema, {
    command: { case: "selectTab", value: create(UiSelectTabSchema, { sessionId }) },
  });
}

beforeAll(async () => {
  workdir = mkdtempSync(join(tmpdir(), "roost-uibus-"));
  const opened = openDb(join(workdir, "test.db"));
  const db = opened.db;
  await runMigrations(opened.sqlite);
  closeDb = async () => { await opened.close(); };
  const now = Date.now();
  await db.insertInto("organizations").values({
    id: "ui-handlers-organization",
    slug: "ui-handlers",
    name: "UI handlers",
    status: "active",
    created_at_ms: now,
  }).execute();
  await db.insertInto("dashboards").values({
    id: DASHBOARD,
    organization_id: "ui-handlers-organization",
    slug: "ui-handlers",
    name: "UI handlers",
    status: "active",
    created_at_ms: now,
  }).execute();
  await db.insertInto("workers").values({
    fp: "ui-handlers-worker",
    dashboard_id: DASHBOARD,
    label: "UI handlers",
    os: "linux",
    git_sha: null,
    host_metrics_json: null,
    registered_at_ms: now,
    last_seen_ms: now,
    reachable_addr: null,
  }).execute();
  await db.insertInto("sessions").values({
    id: "sess-9",
    dashboard_id: DASHBOARD,
    worker_fp: "ui-handlers-worker",
    channel: 9,
    kind: "shell",
    cwd: "/ui",
    workspace_id: null,
    status: "open",
    created_at: now,
    closed_at: null,
    custom_title: null,
    git_branch: null,
    git_remote: null,
    pr_number: null,
    pr_state: null,
    pr_checks: null,
    pr_url: null,
    ports_json: null,
    spawn_cwd: null,
  }).execute();
  // Known fp → label mapping for the uiListStates label test.
  await db.insertInto("authorized_keys").values({
    fingerprint: "fp-known", public_key: new Uint8Array(32),
    label: "Chrome — test", added_at: now,
  }).execute();
  uiLayoutApplies = new UiLayoutApplyOwner();
  uiStates = new UiStateOwner();
  handlers = makeUiHandlers({ db, uiLayoutApplies, uiStates } as unknown as ConnectDeps);
});

afterAll(async () => {
  uiLayoutApplies?.dispose();
  uiStates?.dispose();
  await closeDb?.();
  rmSync(workdir, { recursive: true, force: true });
});

// Explicit owner cleanup prevents UI state or pending applies from crossing cases.
beforeEach(() => {
  uiStates._statesByTab.clear();
  uiLayoutApplies.dispose();
});

describe("uiReportState → uiListStates roundtrip", () => {
  test("state echoed back with sane lastMs and empty label for unknown fp", async () => {
    const before = Date.now();
    await handlers.uiReportState(reportReq("tab-1"), authCtx);
    const resp = await handlers.uiListStates(create(UiListStatesRequestSchema, {}), authCtx);
    // ServiceImpl return type is MessageInitShape — fields optional at the
    // type level even though create() always materializes them.
    const tabs = resp.tabs ?? [];
    expect(tabs.length).toBe(1);
    const tab = tabs[0]!;
    expect(tab.fp).toBe("fp-test");
    expect(tab.tabId).toBe("tab-1");
    expect(tab.label).toBe(""); // fp-test has no authorized_keys row
    expect(Number(tab.lastMs)).toBeGreaterThanOrEqual(before);
    expect(Number(tab.lastMs)).toBeLessThanOrEqual(Date.now());
    expect(tab.state?.layoutDocument?.schemaVersion).toBe(1);
    expect(tab.state?.layoutDocument?.focusedLeafKey).toBe("leaf-1");
    expect(tab.state?.layoutDocument?.bindings?.map((binding) => binding.sessionId))
      .toEqual(["sess-9"]);
  });

  test("label resolved from authorized_keys for a known fp", async () => {
    await handlers.uiReportState(reportReq("tab-k"), fakeAuthCtx("fp-known"));
    const resp = await handlers.uiListStates(create(UiListStatesRequestSchema, {}), authCtx);
    expect((resp.tabs ?? []).find((t) => t.fp === "fp-known")?.label).toBe("Chrome — test");
  });

  test("report publishes a state msg on uiBus", async () => {
    const msgs: UiBusMsg[] = [];
    const stop = uiBus.subscribe((m) => msgs.push(m), DASHBOARD);
    await handlers.uiReportState(reportReq("tab-1"), authCtx);
    stop();
    expect(msgs.length).toBe(1);
    const m = msgs[0]!;
    expect(m.kind).toBe("state");
    if (m.kind === "state") {
      expect(m.fp).toBe("fp-test");
      expect(m.tabId).toBe("tab-1");
      expect(m.state.activePath).toBe("/s/sess-1");
    }
  });

  test("rejects malformed typed documents before retaining or publishing state", async () => {
    const invalid = reportReq("tab-invalid");
    invalid.layoutDocument!.schemaVersion = 2;
    const msgs: UiBusMsg[] = [];
    const stop = uiBus.subscribe((message) => msgs.push(message), DASHBOARD);
    await expect(handlers.uiReportState(invalid, authCtx))
      .rejects.toMatchObject({ code: Code.InvalidArgument });
    stop();
    expect(uiStates._statesByTab.size).toBe(0);
    expect(msgs).toEqual([]);
  });

  test("rejects report bindings outside the selected dashboard", async () => {
    const request = reportReq("tab-foreign");
    request.layoutDocument = layoutDocument("foreign-session");
    await expect(handlers.uiReportState(request, authCtx))
      .rejects.toMatchObject({ code: Code.NotFound });
    expect(uiStates._statesByTab.size).toBe(0);
  });
});

describe("upsert keying on fp:tabId", () => {
  test("same fp:tabId upserts (1 entry), different tabId adds (2 entries)", async () => {
    await handlers.uiReportState(reportReq("tab-1", "/s/old"), authCtx);
    await handlers.uiReportState(reportReq("tab-1", "/s/new"), authCtx);
    expect(uiStates._statesByTab.size).toBe(1);
    expect(uiStates.list(DASHBOARD)[0]?.state.activePath).toBe("/s/new");

    await handlers.uiReportState(reportReq("tab-2"), authCtx);
    expect(uiStates._statesByTab.size).toBe(2);
  });
});

describe("TTL reap", () => {
  test("stale entry excluded from list and snapshot", async () => {
    await handlers.uiReportState(reportReq("tab-live"), authCtx);
    await handlers.uiReportState(reportReq("tab-dead"), authCtx);
    const dead = uiStates.list(DASHBOARD).find((entry) => entry.tabId === "tab-dead")!;
    dead.lastMs = Date.now() - UI_STATE_TTL_MS - 1;

    const snap = uiStates.snapshot(DASHBOARD);
    expect(snap.map((state) => state.tabId)).toEqual(["tab-live"]);

    const resp = await handlers.uiListStates(create(UiListStatesRequestSchema, {}), authCtx);
    expect((resp.tabs ?? []).map((tab) => tab.tabId)).toEqual(["tab-live"]);
    expect(uiStates._statesByTab.size).toBe(1);
  });
});

describe("uiDispatch", () => {
  test("publishes the command intact and reports delivered = subscriber count", async () => {
    const msgs: UiBusMsg[] = [];
    const stop = uiBus.subscribe((m) => msgs.push(m), DASHBOARD);
    const expected = uiBus.subscriberCountFor(DASHBOARD);
    const resp = await handlers.uiDispatch(create(UiDispatchRequestSchema, {
      targetTabId: "tab-1", command: selectTabCmd("sess-9"),
    }), authCtx);
    stop();
    expect(resp.delivered).toBe(expected);
    expect(resp.delivered).toBeGreaterThanOrEqual(1); // our subscriber was live
    expect(msgs.length).toBe(1);
    const m = msgs[0]!;
    expect(m.kind).toBe("command");
    if (m.kind === "command") {
      expect(m.targetTabId).toBe("tab-1");
      expect(m.command.command.case).toBe("selectTab");
      if (m.command.command.case === "selectTab") {
        expect(m.command.command.value.sessionId).toBe("sess-9");
      }
    }
  });

  test("missing command → ConnectError InvalidArgument, nothing published", async () => {
    const msgs: UiBusMsg[] = [];
    const stop = uiBus.subscribe((m) => msgs.push(m), DASHBOARD);
    // Both shapes must reject: command absent, and command present with no case.
    for (const command of [undefined, create(UiCommandSchema, {})]) {
      try {
        await handlers.uiDispatch(create(UiDispatchRequestSchema, { targetTabId: "", command }), authCtx);
        expect.unreachable("uiDispatch should have thrown");
      } catch (e) {
        expect(e).toBeInstanceOf(ConnectError);
        expect((e as ConnectError).code).toBe(Code.InvalidArgument);
      }
    }
    stop();
    expect(msgs.length).toBe(0);
  });

  test("explicitly refuses applyLayout without changing legacy delivery", async () => {
    const msgs: UiBusMsg[] = [];
    const stop = uiBus.subscribe((message) => msgs.push(message), DASHBOARD);
    const command = create(UiCommandSchema, {
      command: {
        case: "applyLayout",
        value: create(UiApplyLayoutSchema, { document: layoutDocument() }),
      },
    });
    await expect(handlers.uiDispatch(create(UiDispatchRequestSchema, {
      targetTabId: "tab-1",
      command,
    }), authCtx)).rejects.toMatchObject({ code: Code.InvalidArgument });
    stop();
    expect(msgs).toEqual([]);
  });
});

describe("uiApplyLayout", () => {
  test("registers before publication and returns an immediate applied ACK", async () => {
    const target = {
      dashboardId: DASHBOARD,
      fingerprint: "target-fp",
      tabId: "target-tab",
      socketId: "target-socket",
    };
    const unregister = uiLayoutApplies.registerTarget(target);
    const seen: UiBusMsg[] = [];
    const stop = uiBus.subscribe((message) => {
      seen.push(message);
      if (message.kind !== "apply") return;
      expect(uiLayoutApplies.stats().pending).toBe(1);
      uiLayoutApplies.acceptResult(target, create(UiApplyLayoutResultSchema, {
        correlationId: message.correlationId,
        outcome: UiApplyLayoutOutcome.APPLIED,
      }));
    }, DASHBOARD);
    const response = await handlers.uiApplyLayout(create(UiApplyLayoutRequestSchema, {
      targetTabId: target.tabId,
      targetFingerprint: target.fingerprint,
      document: layoutDocument(),
    }), authCtx);
    stop();
    unregister();
    expect(response.outcome).toBe(UiApplyLayoutOutcome.APPLIED);
    expect(response.reason).toBeUndefined();
    expect(seen.map((message) => message.kind)).toEqual(["apply"]);
    expect(uiLayoutApplies.stats().pending).toBe(0);
  });

  test("a retained fresh report without a live socket returns target-gone", async () => {
    await handlers.uiReportState(reportReq("reported-only"), authCtx);
    const reportKey = uiStates.list(DASHBOARD)
      .find((entry) => entry.tabId === "reported-only");
    expect(reportKey).toBeDefined();
    const seen: UiBusMsg[] = [];
    const stop = uiBus.subscribe((message) => seen.push(message), DASHBOARD);
    const response = await handlers.uiApplyLayout(create(UiApplyLayoutRequestSchema, {
      targetTabId: "reported-only",
      targetFingerprint: "fp-test",
      document: layoutDocument(),
    }), authCtx);
    stop();
    expect(response.outcome).toBe(UiApplyLayoutOutcome.TARGET_GONE);
    expect(seen.filter((message) => message.kind === "apply")).toEqual([]);
    expect(uiStates.list(DASHBOARD).some((entry) => entry.tabId === "reported-only")).toBe(true);
  });

  test("rejects an apply with a binding outside the selected dashboard", async () => {
    const target = {
      dashboardId: DASHBOARD,
      fingerprint: "target-fp",
      tabId: "target-tab",
      socketId: "target-socket",
    };
    const unregister = uiLayoutApplies.registerTarget(target);
    const seen: UiBusMsg[] = [];
    const stop = uiBus.subscribe((message) => seen.push(message), DASHBOARD);
    await expect(handlers.uiApplyLayout(create(UiApplyLayoutRequestSchema, {
      targetTabId: target.tabId,
      targetFingerprint: target.fingerprint,
      document: layoutDocument("foreign-session"),
    }), authCtx)).rejects.toMatchObject({ code: Code.NotFound });
    stop();
    unregister();
    expect(seen).toEqual([]);
    expect(uiLayoutApplies.stats().pending).toBe(0);
  });
});

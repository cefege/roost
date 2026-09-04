// ui-cc handler contract: report → list roundtrip, upsert keying, TTL reap,
// and dispatch relay. Drives the REAL makeUiHandlers + REAL uiBus over an
// in-memory DB — same direct-handler pattern as task-bus-publish.test.ts
// (no transport; requireAuth only reads ctx.values.get(callerKey)).

import { describe, test, expect, beforeAll, beforeEach, afterAll } from "bun:test";
import { mkdtempSync, rmSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { create } from "@bufbuild/protobuf";
import { ConnectError, Code, createContextValues, type HandlerContext } from "@connectrpc/connect";
import {
  UiListStatesRequestSchema, UiDispatchRequestSchema,
} from "@roost/shared/proto/coordinator_pb";
import {
  UiReportStateRequestSchema, UiCommandSchema, UiSelectTabSchema,
} from "@roost/shared/proto/sync_pb";
import { openDb } from "../src/db/connection.ts";
import { runMigrations } from "../src/db/migrate.ts";
import type { ConnectDeps } from "../src/connect/router.ts";
import {
  callerKey,
  dashboardActorKey,
  type DashboardActor,
} from "../src/connect/auth-interceptor.ts";
import {
  makeUiHandlers, getUiStateSnapshot, _uiStatesByTab, UI_STATE_TTL_MS,
  type UiHandlers,
} from "../src/connect/handlers-ui.ts";
import { uiBus, type UiBusMsg } from "../src/buses.ts";

let workdir: string;
let closeDb: () => Promise<void>;
let handlers: UiHandlers;

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
  return { values } as unknown as HandlerContext;
}
const authCtx = fakeAuthCtx("fp-test");

function reportReq(tabId: string, activePath = "/s/sess-1") {
  return create(UiReportStateRequestSchema, {
    tabId, activePath, folderKey: "fp:/tmp/proj",
    layoutJson: `{"root":{"kind":"leaf"}}`,
    focusedPaneId: "pane-1", visibleSessionIds: ["sess-1"],
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
    keeper_stale: null,
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
  // Handlers only touch deps.db; the rest of ConnectDeps is transport wiring.
  handlers = makeUiHandlers({ db } as unknown as ConnectDeps);
});

afterAll(async () => {
  await closeDb?.();
  rmSync(workdir, { recursive: true, force: true });
});

// Module-level singleton map — isolate tests from each other.
beforeEach(() => { _uiStatesByTab.clear(); });

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
    expect(tab.state?.activePath).toBe("/s/sess-1");
    expect(tab.state?.layoutJson).toBe(`{"root":{"kind":"leaf"}}`);
    expect(tab.state?.visibleSessionIds).toEqual(["sess-1"]);
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
});

describe("upsert keying on fp:tabId", () => {
  test("same fp:tabId upserts (1 entry), different tabId adds (2 entries)", async () => {
    await handlers.uiReportState(reportReq("tab-1", "/s/old"), authCtx);
    await handlers.uiReportState(reportReq("tab-1", "/s/new"), authCtx);
    expect(_uiStatesByTab.size).toBe(1);
    expect(_uiStatesByTab.get(`${DASHBOARD}:fp-test:tab-1`)?.state.activePath).toBe("/s/new");

    await handlers.uiReportState(reportReq("tab-2"), authCtx);
    expect(_uiStatesByTab.size).toBe(2);
  });
});

describe("TTL reap", () => {
  test("stale entry excluded from uiListStates and getUiStateSnapshot", async () => {
    await handlers.uiReportState(reportReq("tab-live"), authCtx);
    await handlers.uiReportState(reportReq("tab-dead"), authCtx);
    // Age the second entry past TTL through the module's explicit test seam.
    const dead = _uiStatesByTab.get(`${DASHBOARD}:fp-test:tab-dead`)!;
    dead.lastMs = Date.now() - UI_STATE_TTL_MS - 1;

    const snap = getUiStateSnapshot(DASHBOARD);
    expect(snap.map((s) => s.tabId)).toEqual(["tab-live"]);

    const resp = await handlers.uiListStates(create(UiListStatesRequestSchema, {}), authCtx);
    expect((resp.tabs ?? []).map((t) => t.tabId)).toEqual(["tab-live"]);
    // uiListStates reaps inline, not just filters — the map itself shrinks.
    expect(_uiStatesByTab.size).toBe(1);
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
});

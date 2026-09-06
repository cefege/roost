// Pins the legacy UI command session-id boundary before database or bus work.
// Pure extraction proves the shared exact UTF-8 limit, while the handler case covers
// every session, anchor, and destination field with an instrumented database boundary.

import { afterAll, beforeAll, expect, test } from "bun:test";
import {
  Code,
  createContextValues,
  type HandlerContext,
} from "@connectrpc/connect";
import { create } from "@bufbuild/protobuf";
import { LAYOUT_DOCUMENT_MAX_SESSION_ID_UTF8_BYTES } from "@roost/shared/layout-document";
import { UiDispatchRequestSchema } from "@roost/shared/proto/coordinator_pb";
import {
  UiCommandSchema,
  UiMoveTabSchema,
  UiPlaceSplitSchema,
  UiSelectTabSchema,
} from "@roost/shared/proto/sync_pb";
import { uiBus, type UiBusMsg } from "../src/buses.ts";
import {
  callerKey,
  dashboardActorKey,
  type DashboardActor,
} from "../src/connect/auth-interceptor.ts";
import { makeUiHandlers } from "../src/connect/handlers-ui.ts";
import { legacyUiCommandSessionIds } from "../src/connect/ui-legacy-command.ts";
import type { ConnectDeps } from "../src/connect/router.ts";
import {
  createSyncWsKeepaliveCoordFixture,
  SYNC_WS_KEEPALIVE_DASHBOARD_ID,
  type SyncWsKeepaliveCoordFixture,
} from "./sync-ws-keepalive-coord-fixture.ts";

let fixture: SyncWsKeepaliveCoordFixture;
let actorContext: HandlerContext;

beforeAll(async () => {
  fixture = await createSyncWsKeepaliveCoordFixture();
  const actor: DashboardActor = {
    accountId: "legacy-command-account",
    organizationId: "legacy-command-organization",
    dashboardId: SYNC_WS_KEEPALIVE_DASHBOARD_ID,
    organizationRole: "owner",
    dashboardRole: "admin",
    deviceFingerprint: fixture.fingerprint,
  };
  const values = createContextValues();
  values.set(callerKey, {
    kind: "account-device",
    fingerprint: fixture.fingerprint,
    label: "",
    accountId: actor.accountId,
  });
  values.set(dashboardActorKey, actor);
  actorContext = {
    values,
    signal: new AbortController().signal,
  } as unknown as HandlerContext;
});

afterAll(async () => {
  await fixture?.close();
});

test("legacy command extraction admits the shared exact UTF-8 session bound", () => {
  const exactSessionId = "🙂".repeat(LAYOUT_DOCUMENT_MAX_SESSION_ID_UTF8_BYTES / 4);
  const command = create(UiCommandSchema, { command: {
    case: "selectTab",
    value: create(UiSelectTabSchema, { sessionId: exactSessionId }),
  } });
  expect(legacyUiCommandSessionIds(command)).toEqual([exactSessionId]);
});

test("oversized legacy session fields fail before SQLite or UI bus delivery", async () => {
  const oversized = `${"🙂".repeat(LAYOUT_DOCUMENT_MAX_SESSION_ID_UTF8_BYTES / 4)}x`;
  const commands = [
    create(UiCommandSchema, { command: {
      case: "selectTab",
      value: create(UiSelectTabSchema, { sessionId: oversized }),
    } }),
    create(UiCommandSchema, { command: {
      case: "placeSplit",
      value: create(UiPlaceSplitSchema, {
        sessionId: oversized,
        anchorSessionId: "anchor",
        dir: "row",
      }),
    } }),
    create(UiCommandSchema, { command: {
      case: "placeSplit",
      value: create(UiPlaceSplitSchema, {
        sessionId: "session",
        anchorSessionId: oversized,
        dir: "row",
      }),
    } }),
    create(UiCommandSchema, { command: {
      case: "moveTab",
      value: create(UiMoveTabSchema, {
        sessionId: oversized,
        destSessionId: "destination",
      }),
    } }),
    create(UiCommandSchema, { command: {
      case: "moveTab",
      value: create(UiMoveTabSchema, {
        sessionId: "session",
        destSessionId: oversized,
      }),
    } }),
  ];
  let databaseQueried = false;
  const db = {
    selectFrom: () => {
      databaseQueried = true;
      throw new Error("oversized session id reached SQLite");
    },
  } as unknown as ConnectDeps["db"];
  const handlers = makeUiHandlers({ ...fixture.deps, db });
  const published: UiBusMsg[] = [];
  const stop = uiBus.subscribe(
    (message) => published.push(message),
    SYNC_WS_KEEPALIVE_DASHBOARD_ID,
  );
  for (const command of commands) {
    await expect(handlers.uiDispatch(create(UiDispatchRequestSchema, {
      targetTabId: "target-tab",
      command,
    }), actorContext)).rejects.toMatchObject({ code: Code.InvalidArgument });
  }
  stop();
  expect(databaseQueried).toBe(false);
  expect(published).toEqual([]);
});

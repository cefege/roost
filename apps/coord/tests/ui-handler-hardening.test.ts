// Pins coordinator UI ingress canonicalization, shared text/document limits, and
// exact fingerprint targeting. The real handler, bus, state owner, and apply owner
// run against an isolated coordinator database; no transport or browser is mocked.
// Unknown protobuf fields are injected as raw wire bytes to exercise the trust boundary.

import { afterAll, beforeAll, beforeEach, describe, expect, test } from "bun:test";
import {
  Code,
  createContextValues,
  type HandlerContext,
} from "@connectrpc/connect";
import { create, fromBinary, toBinary } from "@bufbuild/protobuf";
import {
  UiApplyLayoutRequestSchema,
  UiDispatchRequestSchema,
} from "@roost/shared/proto/coordinator_pb";
import {
  UiArrangeSchema,
  LayoutDocumentLeafSchema,
  LayoutDocumentV1Schema,
  UiApplyLayoutOutcome,
  UiApplyLayoutResultSchema,
  UiCommandSchema,
  UiNavigateSchema,
  UiReportStateRequestSchema,
  type LayoutDocumentV1 as ProtoLayoutDocumentV1,
  type UiReportStateRequest,
} from "@roost/shared/proto/sync_pb";
import {
  LAYOUT_DOCUMENT_MAX_KEY_UTF8_BYTES,
  type LayoutDocumentV1,
} from "@roost/shared/layout-document";
import { layoutDocumentToProto } from "@roost/shared/layout-document-proto";
import {
  UI_ACTIVE_PATH_MAX_UTF8_BYTES,
  UI_FOLDER_KEY_MAX_UTF8_BYTES,
  UI_TAB_ID_MAX_UTF8_BYTES,
} from "@roost/shared/ui-state";
import { uiBus, type UiBusMsg } from "../src/buses.ts";
import { callerKey } from "../src/connect/auth-interceptor.ts";
import { UiStateOwner } from "../src/connect/ui-state-owner.ts";
import { makeUiHandlers, type UiHandlers } from "../src/connect/handlers-ui.ts";
import {
  createSyncWsKeepaliveCoordFixture,
  type SyncWsKeepaliveCoordFixture,
} from "./sync-ws-keepalive-coord-fixture.ts";

const BASE_DOCUMENT: LayoutDocumentV1 = {
  schema_version: 1,
  root: {
    kind: "leaf",
    leaf_key: "leaf",
    slot_keys: [],
    selected_slot_key: null,
  },
  focused_leaf_key: "leaf",
  bindings: [],
};

let fixture: SyncWsKeepaliveCoordFixture;
let handlers: UiHandlers;
let actorContext: HandlerContext;

function authContext(fingerprint: string): HandlerContext {
  const values = createContextValues();
  values.set(callerKey, {
    kind: "account-device",
    fingerprint,
    label: "",
    accountId: "ui-hardening-account",
  });
  return { values, signal: new AbortController().signal } as unknown as HandlerContext;
}

function reportState(options: {
  tabId?: string;
  activePath?: string;
  folderKey?: string;
  document?: ProtoLayoutDocumentV1;
} = {}): UiReportStateRequest {
  return create(UiReportStateRequestSchema, {
    tabId: options.tabId ?? "tab",
    activePath: options.activePath ?? "/",
    folderKey: options.folderKey ?? "folder",
    layoutDocument: options.document,
  });
}

function navigateCommand(path: string) {
  return create(UiCommandSchema, {
    command: {
      case: "navigate",
      value: create(UiNavigateSchema, { path }),
    },
  });
}

beforeAll(async () => {
  fixture = await createSyncWsKeepaliveCoordFixture();
  handlers = makeUiHandlers(fixture.deps);
  actorContext = authContext(fixture.fingerprint);
});

beforeEach(() => {
  fixture.deps.uiStates._statesByTab.clear();
  fixture.deps.uiLayoutApplies.dispose();
});

afterAll(async () => {
  await fixture?.close();
});

describe("canonical protobuf ingress", () => {
  test("drops retired report tags 4 through 6 before retention and publication", async () => {
    const canonical = reportState({ document: layoutDocumentToProto(BASE_DOCUMENT) });
    const attackedBytes = Uint8Array.from([
      ...toBinary(UiReportStateRequestSchema, canonical),
      0x22, 0x03, 0x6f, 0x6c, 0x64,
      0x2a, 0x01, 0x78,
      0x32, 0x01, 0x79,
    ]);
    const attacked = fromBinary(UiReportStateRequestSchema, attackedBytes);
    expect(toBinary(UiReportStateRequestSchema, attacked).byteLength)
      .toBeGreaterThan(toBinary(UiReportStateRequestSchema, canonical).byteLength);
    const messages: UiBusMsg[] = [];
    const stop = uiBus.subscribe((message) => messages.push(message));
    await handlers.uiReportState(attacked, actorContext);
    stop();

    const retained = fixture.deps.uiStates.snapshot()[0]!.state;
    expect(toBinary(UiReportStateRequestSchema, retained))
      .toEqual(toBinary(UiReportStateRequestSchema, canonical));
    const published = messages.find((message) => message.kind === "state");
    expect(published?.kind).toBe("state");
    if (published?.kind === "state") {
      expect(toBinary(UiReportStateRequestSchema, published.state))
        .toEqual(toBinary(UiReportStateRequestSchema, canonical));
    }
  });

  test("drops unknown fields nested inside a legacy UI command", async () => {
    const canonical = navigateCommand("/known");
    const attacked = fromBinary(
      UiCommandSchema,
      toBinary(UiCommandSchema, canonical),
    );
    if (attacked.command.case !== "navigate") {
      throw new Error("expected navigate command fixture");
    }
    attacked.command.value = fromBinary(
      UiNavigateSchema,
      Uint8Array.from([
        ...toBinary(UiNavigateSchema, attacked.command.value),
        0x98, 0x06, 0x01,
      ]),
    );
    const messages: UiBusMsg[] = [];
    const stop = uiBus.subscribe((message) => messages.push(message));
    await handlers.uiDispatch(create(UiDispatchRequestSchema, {
      targetTabId: "target-tab",
      command: attacked,
    }), actorContext);
    stop();

    const published = messages.find((message) => message.kind === "command");
    expect(published?.kind).toBe("command");
    if (published?.kind === "command") {
      expect(toBinary(UiCommandSchema, published.command))
        .toEqual(toBinary(UiCommandSchema, canonical));
    }

    await expect(handlers.uiDispatch(create(UiDispatchRequestSchema, {
      command: create(UiCommandSchema, { command: {
        case: "arrange",
        value: create(UiArrangeSchema, { preset: "unknown" }),
      } }),
    }), actorContext)).rejects.toMatchObject({ code: Code.InvalidArgument });
  });

  test("drops unknown fields nested inside an apply document", async () => {
    const canonicalDocument = layoutDocumentToProto(BASE_DOCUMENT);
    const attackedDocument = fromBinary(
      LayoutDocumentV1Schema,
      toBinary(LayoutDocumentV1Schema, canonicalDocument),
    );
    if (attackedDocument.root?.node.case !== "leaf") {
      throw new Error("expected leaf document fixture");
    }
    attackedDocument.root.node.value = fromBinary(
      LayoutDocumentLeafSchema,
      Uint8Array.from([
        ...toBinary(LayoutDocumentLeafSchema, attackedDocument.root.node.value),
        0x98, 0x06, 0x01,
      ]),
    );
    expect(toBinary(LayoutDocumentV1Schema, attackedDocument).byteLength)
      .toBeGreaterThan(toBinary(LayoutDocumentV1Schema, canonicalDocument).byteLength);
    const target = {
      fingerprint: fixture.fingerprint,
      tabId: "apply-tab",
      socketId: "apply-socket",
    };
    const unregister = fixture.deps.uiLayoutApplies.registerTarget(target);
    let publishedDocument: ProtoLayoutDocumentV1 | undefined;
    const stop = uiBus.subscribe((message) => {
      if (message.kind !== "apply") return;
      if (message.command.command.case === "applyLayout") {
        publishedDocument = message.command.command.value.document;
      }
      fixture.deps.uiLayoutApplies.acceptResult(target, create(UiApplyLayoutResultSchema, {
        correlationId: message.correlationId,
        outcome: UiApplyLayoutOutcome.APPLIED,
      }));
    });

    await handlers.uiApplyLayout(create(UiApplyLayoutRequestSchema, {
      targetTabId: target.tabId,
      targetFingerprint: target.fingerprint,
      document: attackedDocument,
    }), actorContext);
    stop();
    unregister();
    expect(publishedDocument).toBeDefined();
    expect(toBinary(LayoutDocumentV1Schema, publishedDocument!))
      .toEqual(toBinary(LayoutDocumentV1Schema, canonicalDocument));
  });
});

describe("shared UI ingress limits", () => {
  test("accepts exact UTF-8 report bounds and rejects every over-limit field", async () => {
    const exactTab = "é".repeat(UI_TAB_ID_MAX_UTF8_BYTES / 2);
    const exactPath = "é".repeat(UI_ACTIVE_PATH_MAX_UTF8_BYTES / 2);
    const exactFolder = "é".repeat(UI_FOLDER_KEY_MAX_UTF8_BYTES / 2);
    await handlers.uiReportState(reportState({
      tabId: exactTab,
      activePath: exactPath,
      folderKey: exactFolder,
    }), actorContext);

    for (const request of [
      reportState({ tabId: `${exactTab}x` }),
      reportState({ activePath: `${exactPath}x` }),
      reportState({ folderKey: `${exactFolder}x` }),
    ]) {
      await expect(handlers.uiReportState(request, actorContext))
        .rejects.toMatchObject({ code: Code.InvalidArgument });
    }
  });

  test("rejects oversized layout keys, target identifiers, and navigation paths", async () => {
    const oversizedDocument = layoutDocumentToProto(BASE_DOCUMENT);
    if (oversizedDocument.root?.node.case !== "leaf") {
      throw new Error("expected leaf document fixture");
    }
    const oversizedKey = "x".repeat(LAYOUT_DOCUMENT_MAX_KEY_UTF8_BYTES + 1);
    oversizedDocument.root.node.value.leafKey = oversizedKey;
    oversizedDocument.focusedLeafKey = oversizedKey;
    await expect(handlers.uiReportState(reportState({ document: oversizedDocument }), actorContext))
      .rejects.toMatchObject({ code: Code.InvalidArgument });

    for (const request of [
      create(UiApplyLayoutRequestSchema, {
        targetTabId: "x".repeat(UI_TAB_ID_MAX_UTF8_BYTES + 1),
        targetFingerprint: fixture.fingerprint,
        document: layoutDocumentToProto(BASE_DOCUMENT),
      }),
      create(UiApplyLayoutRequestSchema, {
        targetTabId: "tab",
        targetFingerprint: "x".repeat(UI_TAB_ID_MAX_UTF8_BYTES + 1),
        document: layoutDocumentToProto(BASE_DOCUMENT),
      }),
      create(UiApplyLayoutRequestSchema, {
        targetTabId: "tab",
        targetFingerprint: "",
        document: layoutDocumentToProto(BASE_DOCUMENT),
      }),
    ]) {
      await expect(handlers.uiApplyLayout(request, actorContext))
        .rejects.toMatchObject({ code: Code.InvalidArgument });
    }

    await expect(handlers.uiDispatch(create(UiDispatchRequestSchema, {
      targetTabId: "x".repeat(UI_TAB_ID_MAX_UTF8_BYTES + 1),
      command: navigateCommand("/"),
    }), actorContext)).rejects.toMatchObject({ code: Code.InvalidArgument });
    await expect(handlers.uiDispatch(create(UiDispatchRequestSchema, {
      command: navigateCommand("x".repeat(UI_ACTIVE_PATH_MAX_UTF8_BYTES + 1)),
    }), actorContext)).rejects.toMatchObject({ code: Code.InvalidArgument });
  });
});

test("new-identity rate exhaustion preserves existing heartbeat updates", async () => {
  const uiStates = new UiStateOwner({
    reapIntervalMs: null,
    maxTabsPerFingerprint: 4,
    maxTabsTotal: 8,
    newIdentitiesPerWindow: 1,
  });
  const rateHandlers = makeUiHandlers({ ...fixture.deps, uiStates });
  await rateHandlers.uiReportState(reportState({ tabId: "existing" }), actorContext);
  await rateHandlers.uiReportState(reportState({
    tabId: "existing",
    activePath: "/heartbeat",
  }), actorContext);
  await expect(rateHandlers.uiReportState(
    reportState({ tabId: "new-identity" }),
    actorContext,
  )).rejects.toMatchObject({ code: Code.ResourceExhausted });
  expect(uiStates.snapshot()[0]?.state.activePath).toBe("/heartbeat");
  uiStates.dispose();
});

test("stale victim report cannot fall through to a colliding live target", async () => {
  const tabId = "colliding-tab";
  await handlers.uiReportState(reportState({ tabId }), actorContext);
  await handlers.uiReportState(
    reportState({ tabId }),
    authContext("attacker-fingerprint"),
  );
  const reports = fixture.deps.uiStates.list();
  expect(reports.map((entry) => entry.fp))
    .toEqual([fixture.fingerprint, "attacker-fingerprint"]);
  reports.find((entry) => entry.fp === fixture.fingerprint)!.lastMs -= 10 * 60_000;
  expect(fixture.deps.uiStates.snapshot().map((entry) => entry.fp))
    .toEqual(["attacker-fingerprint"]);

  const attacker = {
    fingerprint: "attacker-fingerprint",
    tabId,
    socketId: "attacker-socket",
  };
  const unregister = fixture.deps.uiLayoutApplies.registerTarget(attacker);
  const publications: UiBusMsg[] = [];
  const stop = uiBus.subscribe((message) => publications.push(message));
  const response = await handlers.uiApplyLayout(create(UiApplyLayoutRequestSchema, {
    targetTabId: tabId,
    targetFingerprint: fixture.fingerprint,
    document: layoutDocumentToProto(BASE_DOCUMENT),
  }), actorContext);
  stop();
  unregister();
  expect(response.outcome).toBe(UiApplyLayoutOutcome.TARGET_GONE);
  expect(publications.filter((message) => message.kind === "apply")).toEqual([]);
});

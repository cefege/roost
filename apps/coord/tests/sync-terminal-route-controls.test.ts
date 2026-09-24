// Focused Sync-terminal control admission coverage. Route claims and probes must
// stay on the unsequenced control lane, but still require the authenticated
// writable tab and the socket's live scoped resources before a worker send.

import { afterEach, describe, expect, test } from "bun:test";
import { create } from "@bufbuild/protobuf";
import {
  TerminalInputRouteClaimSchema,
  TerminalTransportProbeSchema,
} from "@roost/protocol/proto/sync_pb";
import type { ConnectDeps } from "../src/connect/router.ts";
import {
  makeSyncTerminalControlHooks,
} from "../src/connect/sync-terminal-controls.ts";
import type {
  SyncV2CommandContext,
  SyncV2ResultControl,
} from "../src/connect/sync-ws-v2-commands.ts";
import { TerminalInputRouteResults } from "../src/connect/terminal-input-route-results.ts";
import type { TerminalViewHub } from "../src/connect/terminal-view-hub.ts";

const DEVICE_FP = "b".repeat(64);
const TAB_ID = "route-control-tab";
const SOCKET_ID = "route-control-socket";
let routeResults: TerminalInputRouteResults | null = null;

afterEach(() => {
  routeResults?.dispose();
  routeResults = null;
});

function commandContext(
  command: SyncV2CommandContext["command"],
  options: { readOnly?: boolean; sessionIds?: readonly string[]; workerFps?: readonly string[] } = {},
): { context: SyncV2CommandContext; replies: SyncV2ResultControl[] } {
  const replies: SyncV2ResultControl[] = [];
  return {
    context: {
      caller: {
        fingerprint: DEVICE_FP,
        label: "route-control-test",
        keyGeneration: 1,
        validUntilMs: Date.now() + 60_000,
      },
      scope: {
        ownerWorkerFp: null,
        sessionIds: new Set(options.sessionIds ?? []),
        workerFps: new Set(options.workerFps ?? []),
        workspaceIds: new Set(),
      },
      deviceFingerprint: DEVICE_FP,
      tabId: TAB_ID,
      readOnly: options.readOnly ?? false,
      viewerKey: `${DEVICE_FP}:${TAB_ID}`,
      socketId: SOCKET_ID,
      command,
      reply(control): boolean {
        replies.push(control);
        return true;
      },
    },
    replies,
  };
}

function hooks() {
  routeResults = new TerminalInputRouteResults();
  return makeSyncTerminalControlHooks({
    terminalInputRouteResults: routeResults,
  } as ConnectDeps, {} as TerminalViewHub);
}

describe("Sync terminal route controls", () => {
  test("returns a definite route refusal for a read-only claim", () => {
    const controlHooks = hooks();
    const fixture = commandContext({
      case: "inputRouteClaim",
      value: create(TerminalInputRouteClaimSchema, {
        requestId: "claim-read-only",
        sessionId: "session-read-only",
        revision: 1n,
        domainGeneration: 1n,
        workerEpoch: "worker-epoch",
      }),
    }, { readOnly: true, sessionIds: ["session-read-only"] });

    controlHooks.onV2Command(fixture.context);

    expect(fixture.replies).toHaveLength(1);
    const reply = fixture.replies[0];
    if (reply?.case !== "inputRouteResult") throw new Error("expected input route result");
    expect(reply.value).toMatchObject({
      accepted: false,
      requestId: "claim-read-only",
      reason: "this Sync socket cannot write terminal input",
    });
  });

  test("does not probe a worker outside the authenticated socket scope", async () => {
    const controlHooks = hooks();
    const fixture = commandContext({
      case: "terminalTransportProbe",
      value: create(TerminalTransportProbeSchema, {
        requestId: "probe-foreign-worker",
        workerFp: "foreign-worker",
      }),
    });

    controlHooks.onV2Command(fixture.context);
    await Promise.resolve();

    expect(fixture.replies).toHaveLength(1);
    const reply = fixture.replies[0];
    if (reply?.case !== "terminalTransportProbeResult") throw new Error("expected probe result");
    expect(reply.value).toMatchObject({
      requestId: "probe-foreign-worker",
      workerFp: "foreign-worker",
      workerEpoch: "",
    });
  });

  test("does not claim a session outside the authenticated socket scope", async () => {
    const controlHooks = hooks();
    const fixture = commandContext({
      case: "inputRouteClaim",
      value: create(TerminalInputRouteClaimSchema, {
        requestId: "claim-foreign-session",
        sessionId: "foreign-session",
        revision: 1n,
        domainGeneration: 1n,
        workerEpoch: "worker-epoch",
      }),
    });

    controlHooks.onV2Command(fixture.context);
    await Promise.resolve();

    expect(fixture.replies).toHaveLength(1);
    const reply = fixture.replies[0];
    if (reply?.case !== "inputRouteResult") throw new Error("expected input route result");
    expect(reply.value).toMatchObject({
      accepted: false,
      requestId: "claim-foreign-session",
      reason: "terminal session is unavailable",
    });
  });
});

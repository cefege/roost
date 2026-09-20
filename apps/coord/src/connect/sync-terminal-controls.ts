// Routes Sync v2 terminal controls from the websocket layer into TerminalViewHub,
// input routing, and typed route/probe forwarding. Closing a socket synchronously
// cancels every route waiter before its generation can be reused by a new Sync link.
// Input routes are bound to the authenticated fingerprint, tab, and socket identity.

import { create } from "@bufbuild/protobuf";
import {
  InputAcceptedSchema,
  InputAmbiguousSchema,
  InputRejectedSchema,
  TerminalInputRouteResultSchema,
  TerminalTransportProbeResultSchema,
} from "@roost/shared/proto/sync_pb";
import { isTerminalUuid } from "@roost/shared/viewport";
import { TERMINAL_INPUT_ROUTE_CAPABILITY } from "@roost/shared/terminal-peer";
import { processInputControl } from "./input-control.ts";
import type { ConnectDeps } from "./router.ts";
import {
  cancelTerminalControlGeneration,
  resolveSessionRoute,
} from "./terminal-control-lane.ts";
import {
  TerminalRouteControlRefusal,
  MAX_TERMINAL_INPUT_ROUTE_REVISION,
  isTerminalRouteIdentifier,
  type TerminalInputRouteResults,
} from "./terminal-input-route-results.ts";
import type { TerminalRouteControlSlot } from "./terminal-input-route-result-state.ts";
import { currentRoutableWorker } from "./worker-send-target.ts";
import { isCurrentTerminalInputRouteWorker } from "./worker-send-terminal-route.ts";
import type { SyncWsHandlerOptions } from "./sync-ws-handler.ts";
import type { SyncV2CommandContext } from "./sync-ws-v2-commands.ts";
import type { TerminalViewHub } from "./terminal-view-hub.ts";


export interface SyncTerminalControlHooks extends SyncWsHandlerOptions {
  onV2Command(context: SyncV2CommandContext): void;
  onV2Close(context: {
    viewerKey: string;
    socketId: string;
    deviceFingerprint: string;
    tabId: string;
  }): void;
}
export function makeSyncTerminalControlHooks(
  deps: ConnectDeps,
  terminalViews: TerminalViewHub,
): SyncTerminalControlHooks {
  return {
    terminalViews,
    onV2Command(context): void {
      switch (context.command.case) {
        case "terminalView":
          terminalViews.handleViewCommand(context.socketId, context.command.value);
          return;
        case "terminalResync":
          terminalViews.handleResync(context.socketId, context.command.value);
          return;
        case "input":
          handleSyncInput(deps, context);
          return;
        case "inputRouteClaim":
          void handleInputRouteClaim(deps.terminalInputRouteResults, deps, context);
          return;
        case "terminalTransportProbe":
          void handleTransportProbe(deps.terminalInputRouteResults, context);
          return;
      }
    },
    onV2Close({ viewerKey, socketId }): void {
      deps.terminalInputRouteResults?.retireBrowserConnection(socketId);
      cancelTerminalControlGeneration(viewerKey, socketId);
    },
  };
}


function handleSyncInput(deps: ConnectDeps, context: SyncV2CommandContext): void {
  if (context.command.case !== "input") return;
  const command = context.command.value;
  if (!context.scope.sessionIds.has(command.sessionId)) {
    rejectInput(context, command, "terminal session is unavailable");
    return;
  }
  if (context.viewerKey === null || context.tabId === null) {
    rejectInput(context, command, "terminal input requires a tab-bound Sync socket");
    return;
  }
  if (command.inputRouteEpoch !== "" && !isTerminalRouteIdentifier(command.inputRouteEpoch)) {
    rejectInput(context, command, "invalid terminal input route epoch");
    return;
  }
  if (command.viewId !== undefined && command.viewId !== "" && !isTerminalUuid(command.viewId)) {
    rejectInput(context, command, "invalid terminal input view id");
    return;
  }
  void processInputControl(deps, {
    identity: {
      viewerKey: context.viewerKey,
      callerFingerprint: context.deviceFingerprint,
      clientIp: context.remoteAddress,
    },
    inputRouteAuthority: {
      deviceFingerprint: context.deviceFingerprint,
      tabId: context.tabId,
      connectionId: context.socketId,
      inputRouteEpoch: command.inputRouteEpoch,
    },
    sessionId: command.sessionId,
    inputSeq: command.inputSeq,
    data: command.data,
    socketGeneration: context.socketId,
    audit: {},
  }).then((result) => {
    if (result.status === "accepted") {
      context.reply({
        case: "inputAccepted",
        value: create(InputAcceptedSchema, {
          sessionId: result.sessionId,
          inputSeq: result.inputSeq,
          domainGeneration: command.domainGeneration,
          writtenBytes: result.writtenBytes,
        }),
      });
      return;
    }
    if (result.status === "rejected") {
      rejectInput(context, command, result.reason);
      return;
    }
    context.reply({
      case: "inputAmbiguous",
      value: create(InputAmbiguousSchema, {
        sessionId: result.sessionId,
        inputSeq: result.inputSeq,
        domainGeneration: command.domainGeneration,
        writtenBytes: result.writtenBytes,
        reason: result.reason,
      }),
    });
  });
}

async function handleInputRouteClaim(
  routeResults: TerminalInputRouteResults | undefined,
  deps: ConnectDeps,
  context: SyncV2CommandContext,
): Promise<void> {
  if (context.command.case !== "inputRouteClaim") return;
  const command = context.command.value;
  const results = routeResults;
  const refusal = routeControlRefusal(context)
    ?? claimShapeRefusal(command)
    ?? (results ? null : "terminal input route is unavailable");
  if (refusal !== null) {
    rejectInputRouteClaim(context, command, refusal);
    return;
  }
  if (!context.scope.sessionIds.has(command.sessionId)) {
    rejectInputRouteClaim(context, command, "terminal session is unavailable");
    return;
  }
  let slot: TerminalRouteControlSlot | null = null;
  try {
    slot = results!.reserveControl(context.socketId, command.requestId);
  } catch (error) {
    if (error instanceof TerminalRouteControlRefusal) {
      rejectInputRouteClaim(context, command, error.reason);
    }
    return;
  }
  let routeClaimStarted = false;
  try {
    const route = await resolveSessionRoute(deps.db, command.sessionId);
    if (!route || !context.scope.sessionIds.has(command.sessionId) || !context.scope.workerFps.has(route.workerFp)) {
      rejectInputRouteClaim(context, command, "terminal session is unavailable");
      return;
    }
    const worker = currentRoutableWorker(route.workerFp);
    if (
      !worker
      || !isCurrentTerminalInputRouteWorker(worker, command.workerEpoch)
      || !worker.capabilities.has(TERMINAL_INPUT_ROUTE_CAPABILITY)
    ) {
      rejectInputRouteClaim(context, command, "terminal input route is unavailable");
      return;
    }
    const claim = results!.claim({
      browserRequestId: command.requestId,
      sessionId: command.sessionId,
      revision: command.revision,
      deviceFingerprint: context.deviceFingerprint,
      tabId: context.tabId!,
      connectionId: context.socketId,
      worker,
      workerEpoch: command.workerEpoch,
    }, slot!);
    routeClaimStarted = true;
    const result = await claim;
    if (
      !context.scope.sessionIds.has(command.sessionId)
      || !context.scope.workerFps.has(route.workerFp)
      || !isCurrentTerminalInputRouteWorker(worker, command.workerEpoch)
    ) return;
    context.reply({ case: "inputRouteResult", value: result });
  } catch (error) {
    if (error instanceof TerminalRouteControlRefusal) {
      rejectInputRouteClaim(context, command, error.reason);
    } else if (!routeClaimStarted) {
      rejectInputRouteClaim(context, command, "terminal input route is unavailable");
    }
  } finally {
    if (!routeClaimStarted && slot !== null) results!.releaseControl(slot);
  }

}

async function handleTransportProbe(
  routeResults: TerminalInputRouteResults | undefined,
  context: SyncV2CommandContext,
): Promise<void> {
  if (context.command.case !== "terminalTransportProbe") return;
  const command = context.command.value;
  const results = routeResults;
  const refusal = routeControlRefusal(context)
    ?? (!isTerminalRouteIdentifier(command.requestId) || !isTerminalRouteIdentifier(command.workerFp)
      ? "terminal transport probe is unavailable"
      : results ? null : "terminal transport probe is unavailable");
  if (refusal !== null || !context.scope.workerFps.has(command.workerFp)) {
    rejectTransportProbe(context, command);
    return;
  }
  const worker = currentRoutableWorker(command.workerFp);
  if (worker === null || worker.processEpoch === null) {
    rejectTransportProbe(context, command);
    return;
  }
  const workerEpoch = worker.processEpoch;
  if (
    !isCurrentTerminalInputRouteWorker(worker, workerEpoch)
    || !worker.capabilities.has(TERMINAL_INPUT_ROUTE_CAPABILITY)
  ) {
    rejectTransportProbe(context, command);
    return;
  }
  let slot: TerminalRouteControlSlot | null = null;
  try {
    slot = results!.reserveControl(context.socketId, command.requestId);
  } catch (error) {
    if (error instanceof TerminalRouteControlRefusal) rejectTransportProbe(context, command);
    return;
  }
  const probe = results!.probe({
    browserRequestId: command.requestId,
    connectionId: context.socketId,
    workerFp: command.workerFp,
    worker,
    workerEpoch,
  }, slot!);
  try {
    const result = await probe;
    if (
      !context.scope.workerFps.has(command.workerFp)
      || !isCurrentTerminalInputRouteWorker(worker, workerEpoch)
    ) return;
    context.reply({ case: "terminalTransportProbeResult", value: result });
  } catch (error) {
    if (error instanceof TerminalRouteControlRefusal) {
      rejectTransportProbe(context, command);
    }
  }
}

function rejectInput(
  context: SyncV2CommandContext,
  command: Extract<SyncV2CommandContext["command"], { case: "input" }>["value"],
  reason: string,
): void {
  context.reply({
    case: "inputRejected",
    value: create(InputRejectedSchema, {
      sessionId: command.sessionId,
      inputSeq: command.inputSeq,
      domainGeneration: command.domainGeneration,
      reason,
    }),
  });
}

function rejectInputRouteClaim(
  context: SyncV2CommandContext,
  command: Extract<SyncV2CommandContext["command"], { case: "inputRouteClaim" }>["value"],
  reason: string,
): void {
  context.reply({
    case: "inputRouteResult",
    value: create(TerminalInputRouteResultSchema, {
      requestId: command.requestId,
      sessionId: command.sessionId,
      revision: command.revision,
      accepted: false,
      latestRevision: 0n,
      inputRouteEpoch: "",
      workerEpoch: command.workerEpoch,
      reason,
    }),
  });
}

function rejectTransportProbe(
  context: SyncV2CommandContext,
  command: Extract<SyncV2CommandContext["command"], { case: "terminalTransportProbe" }>["value"],
): void {
  context.reply({
    case: "terminalTransportProbeResult",
    value: create(TerminalTransportProbeResultSchema, {
      requestId: command.requestId,
      workerFp: command.workerFp,
      workerEpoch: "",
    }),
  });
}

function routeControlRefusal(context: SyncV2CommandContext): string | null {
  if (context.readOnly) return "this Sync socket cannot write terminal input";
  if (context.viewerKey === null || context.tabId === null) {
    return "terminal input route requires a tab-bound Sync socket";
  }
  return null;
}

function claimShapeRefusal(
  command: Extract<SyncV2CommandContext["command"], { case: "inputRouteClaim" }>["value"],
): string | null {
  if (
    !isTerminalRouteIdentifier(command.requestId)
    || !isTerminalRouteIdentifier(command.sessionId)
    || !isTerminalRouteIdentifier(command.workerEpoch)
    || command.revision <= 0n
    || command.revision > MAX_TERMINAL_INPUT_ROUTE_REVISION
  ) return "terminal input route is unavailable";
  return null;
}
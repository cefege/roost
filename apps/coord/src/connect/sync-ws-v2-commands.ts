// Owns Sync-v2 client command ingress for each authenticated browser socket.
// Every command is fenced to the socket generation before terminal commands or
// layout acknowledgements can mutate coordinator state. Terminal commands also
// require their domain generation; layout acknowledgements intentionally do not.
// The Sync handler injects transport replies and acknowledged-apply settlement.

import type { ServerWebSocket } from "bun";
import { clone, create } from "@bufbuild/protobuf";
import {
  FirehoseFrameSchema,
  InputRejectedSchema,
  SyncClientFrameSchema,
  SyncDomainResetFrameSchema,
  TerminalInputRouteResultSchema,
  TerminalTransportProbeResultSchema,
  SyncDomain,
  type FirehoseFrame,
  type SyncClientFrame,
  type UiApplyLayoutResult,
} from "@roost/shared/proto/sync_pb";
import { log } from "@roost/shared/log";
import { consumeSyncSessionSnapshot } from "./sync-snapshot-registry.ts";
import {
  allocateDomainGeneration,
  clearV2DomainQueue,
  isLazyDomain,
  type SyncV2DomainState,
} from "./sync-ws-v2-state.ts";
import type { SyncWsData } from "./sync-ws-handler.ts";

type SyncTerminalCommand = Extract<
  SyncClientFrame["command"],
  {
    case:
      | "terminalView"
      | "terminalResync"
      | "input"
      | "inputRouteClaim"
      | "terminalTransportProbe";
  }
>;
export type SyncV2ResultControl = Extract<
  FirehoseFrame["frame"],
  {
    case:
      | "inputAccepted"
      | "inputRejected"
      | "inputAmbiguous"
      | "inputRouteResult"
      | "terminalTransportProbeResult";
  }
>;

export interface SyncV2CommandContext {
  readonly caller: SyncWsData["caller"];
  /** Resource ids this socket may observe, resolved before the upgrade. */
  readonly scope: SyncWsData["scope"];
  readonly deviceFingerprint: string;
  readonly tabId: string | null;
  readonly readOnly: boolean;
  readonly viewerKey: string | null;
  readonly remoteAddress?: string;
  readonly socketId: string;
  readonly command: SyncTerminalCommand;
  reply(control: SyncV2ResultControl): boolean;
}

export interface SyncV2CommandDeps {
  sendV2ControlFrame(ws: ServerWebSocket<SyncWsData>, frame: FirehoseFrame): boolean;
  resetV2Domain(
    ws: ServerWebSocket<SyncWsData>,
    domainId: SyncDomain,
    reason: string,
  ): void;
  scheduleV2(ws: ServerWebSocket<SyncWsData>): void;
  onV2Command?: (context: SyncV2CommandContext) => void;
  onUiApplyLayoutResult?: (context: {
    readonly fingerprint: string;
    readonly tabId: string;
    readonly socketId: string;
    readonly result: UiApplyLayoutResult;
  }) => void;
}

export function makeSyncV2CommandHandler(deps: SyncV2CommandDeps) {
  const { sendV2ControlFrame, resetV2Domain, scheduleV2 } = deps;

  const handleV2Command = (
    ws: ServerWebSocket<SyncWsData>,
    clientFrame: SyncClientFrame,
  ): void => {
    const v2 = ws.data.v2;
    if (!v2 || clientFrame.socketId !== v2.socketId) return;
    const command = clientFrame.command;
    if (command.case === "uiApplyLayoutResult") {
      if (!ws.data.readOnly && ws.data.viewerKey !== null && ws.data.tabId !== null) {
        deps.onUiApplyLayoutResult?.({
          fingerprint: ws.data.caller.fingerprint,
          tabId: ws.data.tabId,
          socketId: v2.socketId,
          result: command.value,
        });
      }
      return;
    }
    if (command.case === "domainReady") {
      const domain = v2.domains.get(command.value.domain);
      if (
        !domain
        || !domain.subscribed
        || command.value.generation !== domain.generation
      ) return;
      if (domain.ready) return;
      let terminalSessionIds: ReadonlySet<string> | undefined;
      if (command.value.domain === SyncDomain.TERMINAL) {
        const token = command.value.snapshotToken;
        const sessionIds = token
          ? consumeSyncSessionSnapshot(v2.socketId, token)
          : null;
        if (!sessionIds) {
          resetV2Domain(ws, SyncDomain.TERMINAL, "snapshot_token_invalid");
          return;
        }
        const admittedSessionIds = new Set(
          [...sessionIds].filter((sessionId) => ws.data.scope.sessionIds.has(sessionId)),
        );
        v2.announcedSessions.clear();
        v2.pendingSessionAnnouncements.clear();
        for (const sessionId of admittedSessionIds) v2.announcedSessions.add(sessionId);
        terminalSessionIds = admittedSessionIds;
      }
      domain.ready = true;
      void ws.data.feed?.seedDomain(command.value.domain, terminalSessionIds);
      scheduleV2(ws);
      return;
    }
    if (command.case === "domainSubscribe" || command.case === "domainUnsubscribe") {
      const domainId = command.value.domain;
      if (!isLazyDomain(domainId)) return;
      const domain = v2.domains.get(domainId);
      if (!domain || command.value.generation !== domain.generation) return;
      if (command.case === "domainSubscribe") {
        if (domain.subscribed) return;
        domain.subscribed = true;
        domain.ready = false;
        // This listener closes the snapshot/live gap before domainReady permits delivery.
        ws.data.feed?.setDomainSubscribed(domainId, true);
        log.info("sync-ws", "audit_subscription_changed", {
          caller_fp: ws.data.caller.fingerprint,
          socket_id: v2.socketId,
          domain: "audit",
          subscribed: true,
        });
        return;
      }
      domain.subscribed = false;
      domain.ready = false;
      ws.data.feed?.setDomainSubscribed(domainId, false);
      clearV2DomainQueue(ws, domain);
      domain.generation = allocateDomainGeneration();
      log.info("sync-ws", "audit_subscription_changed", {
        caller_fp: ws.data.caller.fingerprint,
        socket_id: v2.socketId,
        domain: "audit",
        subscribed: false,
      });
      sendV2ControlFrame(ws, create(FirehoseFrameSchema, {
        frame: {
          case: "domainReset",
          value: create(SyncDomainResetFrameSchema, {
            domain: domainId,
            generation: domain.generation,
            reason: "unsubscribed",
            subscribed: false,
          }),
        },
      }));
      return;
    }
    if (!isTerminalCommand(command)) return;
    if (command.case === "terminalTransportProbe") {
      if (ws.data.readOnly) {
        sendTerminalCommandRefusal(sendV2ControlFrame, ws, command, "this Sync socket cannot write terminal input");
        return;
      }
    } else {
      const terminal = v2.domains.get(SyncDomain.TERMINAL);
      const refusal = terminalCommandRefusal(
        ws.data.readOnly,
        terminal,
        command.value.domainGeneration,
      );
      if (refusal !== null) {
        sendTerminalCommandRefusal(sendV2ControlFrame, ws, command, refusal);
        return;
      }
    }
    const owned = clone(SyncClientFrameSchema, clientFrame).command;
    if (!isTerminalCommand(owned)) return;
    deps.onV2Command?.({
      caller: ws.data.caller,
      scope: ws.data.scope,
      deviceFingerprint: ws.data.caller.fingerprint,
      tabId: ws.data.tabId,
      readOnly: ws.data.readOnly,
      viewerKey: ws.data.viewerKey,
      remoteAddress: ws.data.remoteAddress ?? undefined,
      socketId: v2.socketId,
      command: owned,
      reply: (control) => {
        if (ws.data.v2 !== v2 || ws.data.pressureClosing) return false;
        return sendV2ControlFrame(ws, create(FirehoseFrameSchema, { frame: control }));
      },
    });
  };

  return { handleV2Command };
}

function isTerminalCommand(
  command: SyncClientFrame["command"],
): command is SyncTerminalCommand {
  return command.case === "terminalView"
    || command.case === "terminalResync"
    || command.case === "input"
    || command.case === "inputRouteClaim"
    || command.case === "terminalTransportProbe";
}

function sendTerminalCommandRefusal(
  sendV2ControlFrame: SyncV2CommandDeps["sendV2ControlFrame"],
  ws: ServerWebSocket<SyncWsData>,
  command: SyncTerminalCommand,
  reason: string,
): void {
  switch (command.case) {
    case "input":
      sendV2ControlFrame(ws, create(FirehoseFrameSchema, {
        frame: {
          case: "inputRejected",
          value: create(InputRejectedSchema, {
            sessionId: command.value.sessionId,
            inputSeq: command.value.inputSeq,
            domainGeneration: command.value.domainGeneration,
            reason,
          }),
        },
      }));
      return;
    case "inputRouteClaim":
      sendV2ControlFrame(ws, create(FirehoseFrameSchema, {
        frame: {
          case: "inputRouteResult",
          value: create(TerminalInputRouteResultSchema, {
            requestId: command.value.requestId,
            sessionId: command.value.sessionId,
            revision: command.value.revision,
            accepted: false,
            latestRevision: 0n,
            inputRouteEpoch: "",
            workerEpoch: command.value.workerEpoch,
            reason,
          }),
        },
      }));
      return;
    case "terminalTransportProbe":
      // The probe wire shape has no error field. An empty epoch is explicitly
      // non-successful to the browser's correlator and never impersonates a worker.
      sendV2ControlFrame(ws, create(FirehoseFrameSchema, {
        frame: {
          case: "terminalTransportProbeResult",
          value: create(TerminalTransportProbeResultSchema, {
            requestId: command.value.requestId,
            workerFp: command.value.workerFp,
            workerEpoch: "",
          }),
        },
      }));
      return;
    case "terminalView":
    case "terminalResync":
      return;
  }
}

/** Why a terminal command cannot be honoured on this socket, or null. */
function terminalCommandRefusal(
  readOnly: boolean,
  terminal: SyncV2DomainState | undefined,
  domainGeneration: bigint,
): string | null {
  if (readOnly) return "this Sync socket cannot write terminal input";
  if (!terminal?.ready) return "terminal domain is resubscribing; input was not sent";
  if (domainGeneration !== terminal.generation) {
    return "terminal view generation was reset; input was not sent";
  }
  return null;
}

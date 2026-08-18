// Sync v2 client-command ingress: the only path by which a browser frame
// mutates coordinator state on this socket.
//
// Every command is guarded on socket identity AND on the domain generation it
// was issued against, so a frame composed before a domain reset can never be
// admitted after it. Terminal viewport/input commands are handed to the
// injected onV2Command hook (sync-terminal-controls.ts) with a reply() that
// dies with the socket generation it was created for.
//
// Split out of sync-ws-handler.ts.

import type { ServerWebSocket } from "bun";
import { clone, create } from "@bufbuild/protobuf";
import {
  FirehoseFrameSchema,
  SyncClientFrameSchema,
  SyncDomainResetFrameSchema,
  SyncDomain,
  type FirehoseFrame,
  type SyncClientFrame,
} from "@roost/shared/proto/sync_pb";
import { consumeSyncSessionSnapshot } from "./sync-snapshot-registry.ts";
import {
  allocateDomainGeneration,
  clearV2DomainQueue,
  isLazyDomain,
} from "./sync-ws-v2-state.ts";
import type { SyncWsData } from "./sync-ws-handler.ts";

type SyncTerminalCommand = Extract<
  SyncClientFrame["command"],
  { case: "viewport" | "input" }
>;
export type SyncV2ResultControl = Extract<
  FirehoseFrame["frame"],
  {
    case:
      | "viewportAccepted"
      | "viewportRejected"
      | "viewportAmbiguous"
      | "inputAccepted"
      | "inputRejected"
      | "inputAmbiguous";
  }
>;

export interface SyncV2CommandContext {
  readonly caller: SyncWsData["caller"];
  readonly viewerKey: string;
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
        v2.announcedSessions.clear();
        v2.pendingSessionAnnouncements.clear();
        for (const sessionId of sessionIds) v2.announcedSessions.add(sessionId);
        terminalSessionIds = sessionIds;
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
        domain.subscribed = true;
        domain.ready = false;
        return;
      }
      clearV2DomainQueue(ws, domain);
      domain.subscribed = false;
      domain.ready = false;
      domain.generation = allocateDomainGeneration();
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
    if (command.case !== "viewport" && command.case !== "input") return;
    const viewerKey = ws.data.viewerKey;
    if (viewerKey === null) return;
    const terminal = v2.domains.get(SyncDomain.TERMINAL);
    if (
      !terminal?.ready
      || command.value.domainGeneration !== terminal.generation
    ) return;
    const owned = clone(SyncClientFrameSchema, clientFrame).command;
    if (owned.case !== "viewport" && owned.case !== "input") return;
    deps.onV2Command?.({
      caller: ws.data.caller,
      viewerKey,
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

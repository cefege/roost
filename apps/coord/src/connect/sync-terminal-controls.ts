// Routes Sync v2 terminal commands (view, resync, input controls) from the
// websocket layer into TerminalViewHub and processInputControl, cancelling a
// viewer's control generation on socket close.
// Guard order is load-bearing: sockets without an established viewerKey are
// refused first, then viewIds are UUID-validated, before anything reaches
// input processing.
import { create } from "@bufbuild/protobuf";
import {
  InputAcceptedSchema,
  InputAmbiguousSchema,
  InputRejectedSchema,
} from "@roost/shared/proto/sync_pb";
import { isTerminalUuid } from "@roost/shared/viewport";
import type { ConnectDeps } from "./router.ts";
import { cancelTerminalControlGeneration } from "./terminal-control-lane.ts";
import { processInputControl } from "./input-control.ts";
import type { SyncWsHandlerOptions } from "./sync-ws-handler.ts";
import type { SyncV2CommandContext } from "./sync-ws-v2-commands.ts";
import type { TerminalViewHub } from "./terminal-view-hub.ts";

export interface SyncTerminalControlHooks extends SyncWsHandlerOptions {
  onV2Command(context: SyncV2CommandContext): void;
  onV2Close(context: { viewerKey: string; socketId: string }): void;
}

export function makeSyncTerminalControlHooks(
  deps: ConnectDeps,
  terminalViews: TerminalViewHub,
): SyncTerminalControlHooks {
  return {
    terminalViews,
    onV2Command(context): void {
      if (context.command.case === "terminalView") {
        terminalViews.handleViewCommand(context.socketId, context.command.value);
        return;
      }
      if (context.command.case === "terminalResync") {
        terminalViews.handleResync(context.socketId, context.command.value);
        return;
      }
      const command = context.command.value;
      if (context.viewerKey === null) {
        context.reply({
          case: "inputRejected",
          value: create(InputRejectedSchema, {
            sessionId: command.sessionId,
            inputSeq: command.inputSeq,
            domainGeneration: command.domainGeneration,
            reason: "terminal input requires a tab-bound Sync socket",
          }),
        });
        return;
      }
      if (command.viewId !== undefined && command.viewId !== "" && !isTerminalUuid(command.viewId)) {
        context.reply({
          case: "inputRejected",
          value: create(InputRejectedSchema, {
            sessionId: command.sessionId,
            inputSeq: command.inputSeq,
            domainGeneration: command.domainGeneration,
            reason: "invalid terminal input view id",
          }),
        });
        return;
      }
      void processInputControl(deps, {
        identity: {
          viewerKey: context.viewerKey,
          callerFingerprint: context.caller.fingerprint,
          clientIp: context.remoteAddress,
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
        } else if (result.status === "rejected") {
          context.reply({
            case: "inputRejected",
            value: create(InputRejectedSchema, {
              sessionId: result.sessionId,
              inputSeq: result.inputSeq,
              domainGeneration: command.domainGeneration,
              reason: result.reason,
            }),
          });
        } else {
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
        }
      });
    },
    onV2Close({ viewerKey, socketId }): void {
      cancelTerminalControlGeneration(viewerKey, socketId);
    },
  };
}

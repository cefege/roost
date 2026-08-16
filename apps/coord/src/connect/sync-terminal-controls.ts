import type { SyncWsHandlerOptions } from "./sync-ws-handler.ts";
import type { ConnectDeps } from "./router.ts";
import {
  cancelTerminalControlGeneration,
  processInputControl,
  processViewportControl,
} from "./session-control.ts";

export type SyncTerminalCommand =
  | {
      case: "viewport";
      value: {
        sessionId: string;
        cols: number;
        rows: number;
        clientSeq: bigint;
        cause: number;
        heldCellSeq: bigint;
        domainGeneration: bigint;
      };
    }
  | {
      case: "input";
      value: {
        sessionId: string;
        inputSeq: bigint;
        data: Uint8Array;
        domainGeneration: bigint;
      };
    };

export type SyncTerminalResultControl =
  | {
      case: "viewportAccepted";
      value: {
        sessionId: string;
        clientSeq: bigint;
        domainGeneration: bigint;
        effectiveCols: number;
        effectiveRows: number;
        channelResizeSeq: bigint;
      };
    }
  | {
      case: "viewportRejected";
      value: {
        sessionId: string;
        clientSeq: bigint;
        domainGeneration: bigint;
        reason: string;
      };
    }
  | {
      case: "inputAccepted";
      value: {
        sessionId: string;
        inputSeq: bigint;
        domainGeneration: bigint;
        writtenBytes: number;
      };
    }
  | {
      case: "inputRejected";
      value: {
        sessionId: string;
        inputSeq: bigint;
        domainGeneration: bigint;
        reason: string;
      };
    }
  | {
      case: "inputAmbiguous";
      value: {
        sessionId: string;
        inputSeq: bigint;
        domainGeneration: bigint;
        writtenBytes: number;
        reason: string;
      };
    };

export interface SyncTerminalCommandContext {
  caller: { fingerprint: string; label?: string };
  viewerKey: string;
  remoteAddress?: string;
  socketId: string;
  command: SyncTerminalCommand;
  /** Socket-identity guarded by sync-ws-handler; false after close/reset. */
  reply(control: SyncTerminalResultControl): boolean;
}

/** Extra makeSyncWsHandler options consumed by Sync v2. Extending the existing
 * options type keeps this adapter source-compatible with the v1 handler during
 * the schema/hook merge; v1 simply ignores the additional properties. */
export interface SyncTerminalControlHooks extends SyncWsHandlerOptions {
  onV2Command(context: SyncTerminalCommandContext): void;
  onV2Close(context: { viewerKey: string; socketId: string }): void;
}

export function makeSyncTerminalControlHooks(deps: ConnectDeps): SyncTerminalControlHooks {
  return {
    onV2Command(context): void {
      const identity = {
        viewerKey: context.viewerKey,
        callerFingerprint: context.caller.fingerprint,
        clientIp: context.remoteAddress,
      };
      if (context.command.case === "viewport") {
        const command = context.command.value;
        void processViewportControl(deps, {
          identity,
          sessionId: command.sessionId,
          clientSeq: command.clientSeq,
          cols: command.cols,
          rows: command.rows,
          cause: command.cause,
          heldCellSeq: command.heldCellSeq,
          socketGeneration: context.socketId,
        }).then((result) => {
          if (result.status === "accepted") {
            context.reply({
              case: "viewportAccepted",
              value: {
                sessionId: result.sessionId,
                clientSeq: result.clientSeq,
                domainGeneration: command.domainGeneration,
                effectiveCols: result.cols,
                effectiveRows: result.rows,
                channelResizeSeq: result.channelResizeSeq,
              },
            });
          } else {
            context.reply({
              case: "viewportRejected",
              value: {
                sessionId: result.sessionId,
                clientSeq: result.clientSeq,
                domainGeneration: command.domainGeneration,
                reason: result.reason,
              },
            });
          }
        });
        return;
      }

      const command = context.command.value;
      void processInputControl(deps, {
        identity,
        sessionId: command.sessionId,
        inputSeq: command.inputSeq,
        data: command.data,
        socketGeneration: context.socketId,
        audit: {},
      }).then((result) => {
        if (result.status === "accepted") {
          context.reply({
            case: "inputAccepted",
            value: {
              sessionId: result.sessionId,
              inputSeq: result.inputSeq,
              domainGeneration: command.domainGeneration,
              writtenBytes: result.writtenBytes,
            },
          });
        } else if (result.status === "rejected") {
          context.reply({
            case: "inputRejected",
            value: {
              sessionId: result.sessionId,
              inputSeq: result.inputSeq,
              domainGeneration: command.domainGeneration,
              reason: result.reason,
            },
          });
        } else {
          context.reply({
            case: "inputAmbiguous",
            value: {
              sessionId: result.sessionId,
              inputSeq: result.inputSeq,
              domainGeneration: command.domainGeneration,
              writtenBytes: result.writtenBytes,
              reason: result.reason,
            },
          });
        }
      });
    },
    onV2Close({ viewerKey, socketId }): void {
      cancelTerminalControlGeneration(viewerKey, socketId);
    },
  };
}

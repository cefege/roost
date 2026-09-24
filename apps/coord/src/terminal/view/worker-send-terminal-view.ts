// Sends browser terminal-view traffic to a worker that owns its own views.
// The coordinator has already authenticated and authorized the browser socket;
// these two frames carry the command verbatim plus the identity tuple the
// worker's registry keys membership on, and the socket-closed notice that
// parks it. Called by connect/terminal-view-hub.ts, which owns the gate.

import { create } from "@bufbuild/protobuf";
import {
  CoordWorkerDownSchema,
  DTerminalViewRelaySchema,
  DTerminalViewSocketClosedSchema,
} from "@roost/protocol/proto/worker_transport_pb";
import type {
  TerminalResyncCommand,
  TerminalViewCommand,
} from "@roost/protocol/proto/sync_pb";
import { TERMINAL_STREAM_CONTROL_TIMEOUT_MS } from "../../workers/worker-send.ts";
import { currentRoutableWorker } from "../../workers/worker-send-target.ts";

/** The coordinator holds no waiter for a relayed command — the worker answers
 * with WTerminalViewState whenever its own reconciliation settles — so the
 * worker gets the whole control window the legacy stream-state hop budgets. */
const TERMINAL_VIEW_RELAY_BUDGET_MS = TERMINAL_STREAM_CONTROL_TIMEOUT_MS;

export interface TerminalViewRelayIdentity {
  readonly socketId: string;
  readonly viewerKey: string;
  readonly deviceFingerprint: string;
}

export type TerminalViewRelayCommand =
  | { readonly case: "view"; readonly value: TerminalViewCommand }
  | { readonly case: "resync"; readonly value: TerminalResyncCommand };

/** Relay one authorized browser view/resync command. Returns whether this
 * worker's transport admitted the frame; a refusal is the caller's cue to tell
 * the browser the terminal is unavailable rather than to admit local state. */
export function sendTerminalViewRelay(
  workerFp: string,
  identity: TerminalViewRelayIdentity,
  command: TerminalViewRelayCommand,
): boolean {
  const worker = currentRoutableWorker(workerFp);
  if (!worker) return false;
  try {
    return worker.send(create(CoordWorkerDownSchema, {
      frame: {
        case: "terminalViewRelay",
        value: create(DTerminalViewRelaySchema, {
          socketId: identity.socketId,
          viewerKey: identity.viewerKey,
          deviceFingerprint: identity.deviceFingerprint,
          budgetMs: TERMINAL_VIEW_RELAY_BUDGET_MS,
          command,
        }),
      },
    })) !== 0;
  } catch {
    return false;
  }
}

/** Tell the owning worker a relayed browser socket is gone so its registry can
 * park that socket's views. Fire-and-forget: the socket is already closed, and
 * the worker's own lease sweep is the backstop when this never lands. */
export function sendTerminalViewSocketClosed(
  workerFp: string,
  socketId: string,
): boolean {
  const worker = currentRoutableWorker(workerFp);
  if (!worker) return false;
  try {
    return worker.send(create(CoordWorkerDownSchema, {
      frame: {
        case: "terminalViewSocketClosed",
        value: create(DTerminalViewSocketClosedSchema, { socketId }),
      },
    })) !== 0;
  } catch {
    return false;
  }
}

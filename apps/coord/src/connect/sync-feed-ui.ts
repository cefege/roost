// Owns browser-only UI state and command projection into the Sync firehose.
// startSyncFeed supplies the authenticated browser capability and optional v2
// socket generation; worker/read-only feeds stay subscribed for delivery counts
// while dropping every live UI frame and omitting the retained UI seed.

import { create } from "@bufbuild/protobuf";
import {
  FirehoseFrameSchema,
  UiCommandFrameSchema,
  UiStateFrameSchema,
  type FirehoseFrame,
} from "@roost/shared/proto/sync_pb";
import { uiBus } from "../buses.ts";
import type { UiStateOwner } from "./ui-state-owner.ts";

export function subscribeUiFeed(options: {
  readonly browserUi: boolean;
  readonly dashboardId: string;
  readonly targetSocketId: string | null;
  push(frame: FirehoseFrame): void;
}): () => void {
  return uiBus.subscribe((message) => {
    if (
      !options.browserUi
      || message._dashboard_id !== options.dashboardId
    ) return;
    if (message.kind === "state") {
      options.push(create(FirehoseFrameSchema, {
        frame: {
          case: "uiState",
          value: create(UiStateFrameSchema, {
            fp: message.fp,
            tabId: message.tabId,
            state: message.state,
          }),
        },
      }));
      return;
    }
    if (
      message.kind === "apply"
      && options.targetSocketId !== message.targetSocketId
    ) return;
    options.push(create(FirehoseFrameSchema, {
      frame: {
        case: "uiCommand",
        value: create(UiCommandFrameSchema, {
          targetTabId: message.targetTabId,
          command: message.command,
          correlationId: message.kind === "apply" ? message.correlationId : "",
          targetSocketId: message.kind === "apply" ? message.targetSocketId : "",
        }),
      },
    }));
  }, options.dashboardId);
}

export function* uiStateSeedFrames(
  uiStates: UiStateOwner,
  dashboardId: string,
): Generator<FirehoseFrame> {
  for (const { fp, tabId, state } of uiStates.snapshot(dashboardId)) {
    yield create(FirehoseFrameSchema, {
      frame: {
        case: "uiState",
        value: create(UiStateFrameSchema, { fp, tabId, state }),
      },
    });
  }
}

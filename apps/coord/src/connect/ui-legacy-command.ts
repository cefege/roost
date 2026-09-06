// Owns canonical rebuilding and session-id extraction for the eight legacy UI commands.
// The UI handler validates these untrusted protobuf values before database lookup or bus delivery.
// Shared UTF-8 limits keep command session identities aligned with portable layout bindings.

import { create } from "@bufbuild/protobuf";
import { Code, ConnectError } from "@connectrpc/connect";
import { LAYOUT_DOCUMENT_MAX_SESSION_ID_UTF8_BYTES } from "@roost/shared/layout-document";
import {
  UiArrangeSchema,
  UiCloseTabSchema,
  UiCommandSchema,
  UiFocusPaneSchema,
  UiMoveTabSchema,
  UiNavigateSchema,
  UiPlaceSplitSchema,
  UiSelectTabSchema,
  UiSpotlightSchema,
  type UiCommand,
} from "@roost/shared/proto/sync_pb";
import { hasAtMostUtf8Bytes } from "@roost/shared/ui-state";

export function legacyUiCommandSessionIds(command: UiCommand): readonly string[] {
  const variant = command.command;
  let sessionIds: readonly string[];
  switch (variant.case) {
    case "placeSplit":
      sessionIds = [variant.value.sessionId, variant.value.anchorSessionId];
      break;
    case "selectTab":
    case "focusPane":
    case "closeTab":
    case "spotlight":
      sessionIds = [variant.value.sessionId];
      break;
    case "moveTab":
      sessionIds = [variant.value.sessionId, variant.value.destSessionId];
      break;
    case "navigate":
    case "arrange":
    case "applyLayout":
    case undefined:
      sessionIds = [];
      break;
  }
  for (const sessionId of sessionIds) {
    if (!hasAtMostUtf8Bytes(sessionId, LAYOUT_DOCUMENT_MAX_SESSION_ID_UTF8_BYTES)) {
      throw new ConnectError("invalid UI command session id", Code.InvalidArgument);
    }
  }
  return sessionIds;
}

export function canonicalLegacyUiCommand(command: UiCommand): UiCommand {
  const variant = command.command;
  switch (variant.case) {
    case "navigate":
      return create(UiCommandSchema, { command: {
        case: "navigate",
        value: create(UiNavigateSchema, { path: variant.value.path }),
      } });
    case "placeSplit":
      if (variant.value.dir !== "row" && variant.value.dir !== "col") {
        throw new ConnectError("invalid UI split direction", Code.InvalidArgument);
      }
      return create(UiCommandSchema, { command: {
        case: "placeSplit",
        value: create(UiPlaceSplitSchema, {
          sessionId: variant.value.sessionId,
          anchorSessionId: variant.value.anchorSessionId,
          dir: variant.value.dir,
          insertFirst: variant.value.insertFirst,
        }),
      } });
    case "selectTab":
      return create(UiCommandSchema, { command: {
        case: "selectTab",
        value: create(UiSelectTabSchema, { sessionId: variant.value.sessionId }),
      } });
    case "focusPane":
      return create(UiCommandSchema, { command: {
        case: "focusPane",
        value: create(UiFocusPaneSchema, { sessionId: variant.value.sessionId }),
      } });
    case "moveTab":
      return create(UiCommandSchema, { command: {
        case: "moveTab",
        value: create(UiMoveTabSchema, {
          sessionId: variant.value.sessionId,
          destSessionId: variant.value.destSessionId,
        }),
      } });
    case "arrange":
      if (
        variant.value.preset !== "even"
        && variant.value.preset !== "rows"
        && variant.value.preset !== "tiled"
        && variant.value.preset !== "main-vertical"
        && variant.value.preset !== "balance"
      ) {
        throw new ConnectError("invalid UI arrange preset", Code.InvalidArgument);
      }
      return create(UiCommandSchema, { command: {
        case: "arrange",
        value: create(UiArrangeSchema, { preset: variant.value.preset }),
      } });
    case "closeTab":
      return create(UiCommandSchema, { command: {
        case: "closeTab",
        value: create(UiCloseTabSchema, { sessionId: variant.value.sessionId }),
      } });
    case "spotlight":
      return create(UiCommandSchema, { command: {
        case: "spotlight",
        value: create(UiSpotlightSchema, {
          sessionId: variant.value.sessionId,
          off: variant.value.off,
        }),
      } });
    case "applyLayout":
    case undefined:
      throw new ConnectError("unsupported UI command", Code.InvalidArgument);
  }
}

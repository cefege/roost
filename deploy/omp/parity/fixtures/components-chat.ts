import type * as CacheMarker from "@omp-source/packages/tui/src/chat/cache-invalidation-marker";
import type * as ServedMarker from "@omp-source/packages/tui/src/chat/served-model-marker";
import type * as Collab from "@omp-source/packages/tui/src/chat/collab-prompt-message";
import type * as Recap from "@omp-source/packages/tui/src/chat/recap-notice";
import type * as Stripped from "@omp-source/packages/tui/src/chat/stripped-tool-calls-placeholder";
import type * as TodoReminder from "@omp-source/packages/tui/src/chat/todo-reminder";
import type * as Ttsr from "@omp-source/packages/tui/src/chat/ttsr-notification";
import type * as HookMessage from "@omp-source/packages/tui/src/chat/hook-message";
import type * as MessageFrame from "@omp-source/packages/tui/src/chrome/message-frame";
import type * as MessageNotice from "@omp-source/packages/tui/src/chrome/message-notice";
import type * as TextModule from "@omp-source/packages/tui/src/components/text";
import type { FixtureCase } from "../scenario";
import { componentId, shown } from "./components-support";

const chat = (file: string, name: string) => componentId(`chat/${file}`, name);
const hookText = ["Hook line one", "Hook line two", "Hook line three", "Hook line four", "Hook line five", "Hook line six", "Hook line seven"].join("\n");

export const fixtureCases: FixtureCase[] = [
  {
    id: "comp-chat-cache-invalidation-marker",
    covers: [chat("cache-invalidation-marker", "CacheInvalidationMarkerComponent")],
    async create(context) {
      const { CacheInvalidationMarkerComponent } = await context.load<typeof CacheMarker>("packages/tui/src/chat/cache-invalidation-marker.ts");
      return new CacheInvalidationMarkerComponent({ reprocessedTokens: 50900 });
    },
    checkpoints: [{ name: "cache miss divider", comparisons: [shown("reprocessed tokens", "51K tokens")] }],
  },
  {
    id: "comp-chat-served-model-marker",
    covers: [chat("served-model-marker", "ServedModelMarkerComponent")],
    async create(context) {
      const { ServedModelMarkerComponent } = await context.load<typeof ServedMarker>("packages/tui/src/chat/served-model-marker.ts");
      return new ServedModelMarkerComponent({ requested: "fixture-requested-model", served: "fixture-served-model", provider: "fixture-provider", upstreamProvider: "fixture-upstream" });
    },
    checkpoints: [{ name: "served model divider", comparisons: [shown("requested model", "requested fixture-requested-model")] }],
  },
  {
    id: "comp-chat-collab-prompt-message",
    covers: [chat("collab-prompt-message", "CollabPromptMessageComponent")],
    async create(context) {
      const { CollabPromptMessageComponent } = await context.load<typeof Collab>("packages/tui/src/chat/collab-prompt-message.ts");
      return new CollabPromptMessageComponent({ role: "custom", customType: "collab-prompt", content: "Collab fixture body", display: true, details: { from: "alice" }, timestamp: 1 } as never);
    },
    checkpoints: [{ name: "collaborator prompt", comparisons: [shown("prompt body", "Collab fixture body")] }],
  },
  {
    id: "comp-chat-recap-notice",
    covers: [
      chat("recap-notice", "RecapNotice"),
    ],
    async create(context) {
      const { RecapNotice } = await context.load<typeof Recap>("packages/tui/src/chat/recap-notice.ts");
      return new RecapNotice("Recap fixture summary of the away period");
    },
    checkpoints: [{ name: "recap text", comparisons: [shown("recap", "Recap fixture summary of the away period")] }],
  },
  {
    id: "comp-chat-stripped-tool-calls-placeholder",
    covers: [chat("stripped-tool-calls-placeholder", "StrippedToolCallsPlaceholder")],
    async create(context) {
      const { StrippedToolCallsPlaceholder } = await context.load<typeof Stripped>("packages/tui/src/chat/stripped-tool-calls-placeholder.ts");
      return new StrippedToolCallsPlaceholder(3, true);
    },
    checkpoints: [{ name: "elided tool calls", comparisons: [shown("elision label", "3 tool calls elided")] }],
  },
  {
    id: "comp-chat-todo-reminder",
    covers: [chat("todo-reminder", "TodoReminderComponent")],
    async create(context) {
      const { TodoReminderComponent } = await context.load<typeof TodoReminder>("packages/tui/src/chat/todo-reminder.ts");
      return new TodoReminderComponent([{ content: "Write fixture todo", status: "pending" }, { content: "Verify fixture todo", status: "pending" }], 2, 3);
    },
    checkpoints: [{ name: "incomplete todo reminder", comparisons: [shown("todo item", "Write fixture todo")] }],
  },
  {
    id: "comp-chat-ttsr-notification",
    covers: [chat("ttsr-notification", "TtsrNotificationComponent")],
    async create(context) {
      const { TtsrNotificationComponent } = await context.load<typeof Ttsr>("packages/tui/src/chat/ttsr-notification.ts");
      return new TtsrNotificationComponent([{ name: "fixture-rule-alpha", description: "Alpha rule description" }, { name: "fixture-rule-beta", description: "Beta rule description" }]);
    },
    checkpoints: [{ name: "applied rules", comparisons: [shown("rule name", "fixture-rule-alpha")] }],
  },
  {
    id: "comp-chat-hook-message-expand",
    covers: [chat("hook-message", "HookMessageComponent")],
    async create(context) {
      const { HookMessageComponent } = await context.load<typeof HookMessage>("packages/tui/src/chat/hook-message.ts");
      return new HookMessageComponent({ role: "hookMessage", customType: "fixture-hook", content: hookText, display: true, timestamp: 1 });
    },
    checkpoints: [
      { name: "collapsed hook message", comparisons: [shown("first line", "Hook line one")] },
      { name: "expand hook message", browserActions: [{ type: "click", selector: '[data-omp-role="omp.hook"] button' }], comparisons: [shown("first line after expand", "Hook line one")] },
    ],
  },
  {
    id: "comp-chat-framed-message-expand",
    covers: [componentId("chrome/message-frame", "FramedMessageComponent")],
    async create(context) {
      const { FramedMessageComponent } = await context.load<typeof MessageFrame>("packages/tui/src/chrome/message-frame.ts");
      return new FramedMessageComponent({ message: { customType: "fixture-frame", content: hookText }, role: "omp.fixture-frame", collapseAfterLines: 2 });
    },
    checkpoints: [
      { name: "collapsed framed message", comparisons: [shown("frame tag", "fixture-frame")] },
      { name: "expand framed message", browserActions: [{ type: "click", selector: '[data-omp-role="omp.fixture-frame"] button' }], comparisons: [shown("frame tag after expand", "fixture-frame")] },
    ],
  },
  {
    id: "comp-chat-message-notice-expand",
    covers: [componentId("chrome/message-notice", "MessageNoticeComponent")],
    async create(context) {
      const { MessageNoticeComponent } = await context.load<typeof MessageNotice>("packages/tui/src/chrome/message-notice.ts");
      const { Text } = await context.load<typeof TextModule>("packages/tui/src/components/text.ts");
      return new MessageNoticeComponent({
        role: "omp.notice.fixture",
        presentation: () => ({ header: "Fixture notice head", body: new Text("Notice body line one\nNotice body line two\nNotice body line three", 0, 0) }),
        nativePresentation: () => ({ head: "Fixture notice head", body: [new Text("Notice body line one\nNotice body line two\nNotice body line three", 0, 0)], preview: { lines: 1 } }),
      });
    },
    checkpoints: [
      { name: "collapsed notice", comparisons: [shown("notice head", "Fixture notice head")] },
      { name: "expand notice", browserActions: [{ type: "click", selector: '[data-omp-role="omp.notice.fixture"] button' }], comparisons: [shown("notice head after expand", "Fixture notice head")] },
    ],
  },
  {
    id: "comp-chat-framed-message-custom-renderer",
    covers: [componentId("chrome/message-frame", "FramedMessageComponent")],
    async create(context) {
      const { FramedMessageComponent } = await context.load<typeof MessageFrame>("packages/tui/src/chrome/message-frame.ts");
      const { Text } = await context.load<typeof TextModule>("packages/tui/src/components/text.ts");
      return new FramedMessageComponent({
        message: { customType: "fixture-custom-frame", content: "unused body" },
        role: "omp.fixture-custom-frame",
        customRenderer: () => new Text("Custom renderer fixture row", 0, 0),
      });
    },
    checkpoints: [{ name: "custom renderer output", comparisons: [shown("custom row", "Custom renderer fixture row")] }],
  },
  {
    id: "comp-chat-message-notice-inline",
    covers: [componentId("chrome/message-notice", "MessageNoticeComponent")],
    async create(context) {
      const { MessageNoticeComponent } = await context.load<typeof MessageNotice>("packages/tui/src/chrome/message-notice.ts");
      const { Text } = await context.load<typeof TextModule>("packages/tui/src/components/text.ts");
      return new MessageNoticeComponent({
        role: "omp.notice.inline-fixture",
        presentation: () => ({ header: "Inline notice head", body: new Text("Inline notice body line", 0, 0) }),
        nativePresentation: () => ({ head: "Inline notice head", body: [new Text("Inline notice body line", 0, 0)], inline: { icon: "info" } }),
      });
    },
    checkpoints: [{ name: "inline notice head", comparisons: [shown("inline head", "Inline notice head")] }],
  },
];

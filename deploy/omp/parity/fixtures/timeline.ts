import type * as Rewind from "@omp-source/packages/tui/src/overlays/rewind-selector";
import type * as CopySelector from "@omp-source/packages/tui/src/overlays/copy-selector";
import type { SessionMessageEntryLike } from "@omp-source/packages/tui/src/chat/transcript-entry";
import { callbackValue, visibleText, type FixtureCase } from "../scenario";

const component = (file: string, name: string) => `component:packages/tui/src/overlays/${file}.ts#${name}`;
function entries(): SessionMessageEntryLike[] {
  return [
    { type: "message", id: "u1", parentId: null, timestamp: "2026-11-01T12:00:00Z", message: { role: "user", content: "Find the acknowledgement", timestamp: 1 } },
    { type: "message", id: "u2", parentId: "u1", timestamp: "2026-11-01T12:01:00Z", message: { role: "user", content: "Fix the frame", timestamp: 2 } },
  ];
}

export const fixtureCases: FixtureCase[] = [
  ...(["select", "branch", "cancel"] as const).map((mode): FixtureCase => ({
    id: `picker-rewind-${mode}`,
    covers: [component("rewind-selector", "RewindSelectorComponent"), "component:packages/tui/src/chat/transcript-browser.ts#TranscriptBrowser"],
    async create(context) {
      const { RewindSelectorComponent } = await context.load<typeof Rewind>("packages/tui/src/overlays/rewind-selector.ts");
      return new RewindSelectorComponent(entries(), {
        ui: context.tui, cwd: context.workspace, requestRender: () => context.tui.requestRender(),
        siblingPaths: id => id === "u2" ? [{ rootId: "u3", entries: [{ type: "message", id: "u3", parentId: "u1", timestamp: "2026-11-01T12:02:00Z", message: { role: "user", content: "Keep the frame instead", timestamp: 3 } }] }] : [],
        onSelect: id => { context.record("rewind", id); context.done(); }, onCancel: () => { context.record("cancel", true); context.done(); },
      });
    },
    checkpoints: [
      { name: "actual transcript", comparisons: [visibleText("prompt", "(Fix the frame)", 'text="Fix the frame"', "Fix the frame")] },
      ...(mode === "branch" ? [{ name: "navigate alternate branch", terminalInput: "\x1b[C", browserActions: [{ type: "click" as const, selector: 'button:has-text("Next")' }], comparisons: [visibleText("alternate prompt", "(Keep the frame instead)", 'text="Keep the frame instead"', "Keep the frame instead")] }] : []),
      { name: mode, terminalInput: mode === "cancel" ? "\x1b" : "\r", browserActions: [{ type: "click", selector: `button:has-text("${mode === "cancel" ? "Cancel" : "Rewind"}")` }], comparisons: [callbackValue("outcome", mode === "cancel" ? "cancel" : "rewind", mode === "cancel" ? true : mode === "branch" ? "u3" : "u2")] },
    ],
  })),
  ...(["copy", "cancel"] as const).map((mode): FixtureCase => ({
    id: `picker-copy-${mode}`,
    covers: [component("copy-selector", "CopySelectorComponent"), "component:packages/tui/src/chat/transcript-browser.ts#TranscriptBrowser"],
    async create(context) {
      const { CopySelectorComponent } = await context.load<typeof CopySelector>("packages/tui/src/overlays/copy-selector.ts");
      return new CopySelectorComponent(entries(), { ui: context.tui, cwd: context.workspace, requestRender: () => context.tui.requestRender(),
        onPick: content => { context.record("copy", content); context.done(); }, onCancel: () => { context.record("cancel", true); context.done(); },
      });
    },
    checkpoints: [
      { name: "copyable transcript", comparisons: [visibleText("prompt", "(Fix the frame)", '[data-omp-picker-item="u2"] :text-is("Fix the frame")', "Fix the frame")] },
      { name: mode, terminalInput: mode === "copy" ? "\r" : "\x1b", browserActions: [{ type: "click", selector: `button:has-text("${mode === "copy" ? "Copy" : "Close"}") >> nth=0` }], comparisons: [callbackValue("outcome", mode === "copy" ? "copy" : "cancel", mode === "copy" ? "Fix the frame" : true)] },
    ],
  })),
];

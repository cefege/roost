import type * as BashExecution from "@omp-source/packages/tui/src/chat/bash-execution";
import type * as EvalExecution from "@omp-source/packages/tui/src/chat/eval-execution";
import type * as CustomMessage from "@omp-source/packages/tui/src/chat/custom-message";
import type * as HookMessage from "@omp-source/packages/tui/src/chat/hook-message";
import type * as ReadToolGroup from "@omp-source/packages/tui/src/chat/read-tool-group";
import type * as TextModule from "@omp-source/packages/tui/src/components/text";
import type * as TranscriptContainerModule from "@omp-source/packages/tui/src/chrome/transcript-container";
import type { FixtureCase } from "../scenario";
import { componentId, shown } from "./components-support";

const chat = (file: string, name: string) => componentId(`chat/${file}`, name);
const lines = (prefix: string, count: number) => Array.from({ length: count }, (_, index) => `${prefix}-${String(index + 1).padStart(2, "0")}`).join("\n");
const toggle = (role: string) => `[data-omp-role="${role}"] [aria-expanded] >> nth=0`;
// A 1x1 PNG: the image helper reads its real dimensions for the hidden-image placeholder.
const pixelPng = "iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAYAAAAfFcSJAAAADUlEQVR42mP8z8BQDwAEhQGAhKmMIQAAAABJRU5ErkJggg==";

export const fixtureCases: FixtureCase[] = [
  {
    id: "comp-chat-bash-execution",
    covers: [chat("bash-execution", "BashExecutionComponent")],
    async create(context) {
      const { BashExecutionComponent } = await context.load<typeof BashExecution>("packages/tui/src/chat/bash-execution.ts");
      const component = new BashExecutionComponent("echo bash-fixture", context.tui);
      component.setComplete(0, false, { output: lines("bash-line", 30) });
      return component;
    },
    checkpoints: [
      { name: "completed bash command", comparisons: [shown("command", "echo bash-fixture"), shown("output tail", "bash-line-30")] },
      { name: "expand bash output", browserActions: [{ type: "click", selector: toggle("omp.bash") }], comparisons: [shown("command after expand", "echo bash-fixture"), shown("output tail after expand", "bash-line-30")] },
    ],
  },
  {
    id: "comp-chat-bash-execution-images",
    covers: [chat("bash-execution", "BashExecutionComponent")],
    async create(context) {
      const { BashExecutionComponent } = await context.load<typeof BashExecution>("packages/tui/src/chat/bash-execution.ts");
      const component = new BashExecutionComponent("echo bash-image-fixture", context.tui, true);
      component.setComplete(1, false, { output: "bash-image-output", images: [{ type: "image", data: pixelPng, mimeType: "image/png" }], showImages: false });
      return component;
    },
    checkpoints: [{ name: "excluded failed bash with hidden image", comparisons: [shown("command", "echo bash-image-fixture"), shown("output", "bash-image-output"), shown("image placeholder", "image/png")] }],
  },
  {
    id: "comp-chat-eval-execution",
    covers: [chat("eval-execution", "EvalExecutionComponent")],
    async create(context) {
      const { EvalExecutionComponent } = await context.load<typeof EvalExecution>("packages/tui/src/chat/eval-execution.ts");
      const component = new EvalExecutionComponent("fixture_eval_value = 41 + 1", context.tui);
      component.appendOutput(`${lines("eval-line", 30)}\n`);
      component.setComplete(0, false, { output: lines("eval-line", 30) });
      return component;
    },
    checkpoints: [
      { name: "completed eval cell", comparisons: [shown("code", "fixture_eval_value"), shown("output tail", "eval-line-30")] },
      { name: "expand eval output", browserActions: [{ type: "click", selector: toggle("omp.eval") }], comparisons: [shown("code after expand", "fixture_eval_value"), shown("output tail after expand", "eval-line-30")] },
    ],
  },
  {
    id: "comp-chat-hook-message-custom-renderer",
    covers: [chat("hook-message", "HookMessageComponent")],
    async create(context) {
      const { HookMessageComponent } = await context.load<typeof HookMessage>("packages/tui/src/chat/hook-message.ts");
      const { Text } = await context.load<typeof TextModule>("packages/tui/src/components/text.ts");
      return new HookMessageComponent({ role: "hookMessage", customType: "fixture-hook-custom", content: "unused", display: true, timestamp: 1 }, () => new Text("Hook custom renderer row", 0, 0));
    },
    checkpoints: [{ name: "custom hook renderer output", comparisons: [shown("custom hook row", "Hook custom renderer row")] }],
  },
  {
    id: "comp-chat-custom-message-default",
    covers: [chat("custom-message", "CustomMessageComponent")],
    async create(context) {
      const { CustomMessageComponent } = await context.load<typeof CustomMessage>("packages/tui/src/chat/custom-message.ts");
      return new CustomMessageComponent({ role: "custom", customType: "fixture-custom", content: "Custom message fixture body", display: true, timestamp: 1 } as never);
    },
    checkpoints: [{ name: "default custom message", comparisons: [shown("custom type", "fixture-custom"), shown("custom body", "Custom message fixture body")] }],
  },
  {
    id: "comp-chat-custom-message-renderer",
    covers: [chat("custom-message", "CustomMessageComponent")],
    async create(context) {
      const { CustomMessageComponent } = await context.load<typeof CustomMessage>("packages/tui/src/chat/custom-message.ts");
      const { Text } = await context.load<typeof TextModule>("packages/tui/src/components/text.ts");
      return new CustomMessageComponent({ role: "custom", customType: "fixture-custom-renderer", content: "unused", display: true, timestamp: 1 } as never, () => new Text("Custom message renderer row", 0, 0));
    },
    checkpoints: [{ name: "custom message renderer output", comparisons: [shown("custom row", "Custom message renderer row")] }],
  },
  {
    id: "comp-chat-custom-message-delegation",
    covers: [chat("custom-message", "CustomMessageComponent")],
    async create(context) {
      const { CustomMessageComponent } = await context.load<typeof CustomMessage>("packages/tui/src/chat/custom-message.ts");
      return new CustomMessageComponent({ role: "custom", customType: "live-delegation", content: "Live delegation fixture body", display: true, timestamp: 1 } as never);
    },
    checkpoints: [{ name: "live delegation message", comparisons: [shown("delegation body", "Live delegation fixture body")] }],
  },
  {
    id: "comp-chat-read-tool-group",
    covers: [chat("read-tool-group", "ReadToolGroupComponent")],
    async create(context) {
      const { ReadToolGroupComponent } = await context.load<typeof ReadToolGroup>("packages/tui/src/chat/read-tool-group.ts");
      const component = new ReadToolGroupComponent({ showContentPreview: true });
      component.updateArgs({ path: "fixture-read-one.txt" }, "read-one");
      component.updateArgs({ path: "fixture-read-two.txt" }, "read-two");
      component.setArgsComplete("read-one");
      component.setExecutionStarted("read-one");
      component.updateResult({ content: [{ type: "text", text: lines("read-one-line", 30) }] }, false, "read-one");
      component.updateResult({ content: [{ type: "text", text: "fixture read failure" }], isError: true }, false, "read-two");
      component.finalize();
      return component;
    },
    checkpoints: [
      { name: "read group of two files", comparisons: [shown("first file", "fixture-read-one.txt"), shown("second file", "fixture-read-two.txt")] },
      { name: "expand read group", browserActions: [{ type: "click", selector: toggle("omp.tool.read") }], comparisons: [shown("first file after expand", "fixture-read-one.txt")] },
    ],
  },
  {
    id: "comp-chrome-transcript-block",
    covers: [componentId("chrome/transcript-container", "TranscriptBlock")],
    async create(context) {
      const { TranscriptBlock } = await context.load<typeof TranscriptContainerModule>("packages/tui/src/chrome/transcript-container.ts");
      const { Text } = await context.load<typeof TextModule>("packages/tui/src/components/text.ts");
      const block = new TranscriptBlock();
      block.addChild(new Text("Transcript block row one", 0, 0));
      block.addChild(new Text("Transcript block row two", 0, 0));
      return block;
    },
    checkpoints: [{ name: "grouped rows", comparisons: [shown("first row", "Transcript block row one"), shown("second row", "Transcript block row two")] }],
  },
  {
    id: "comp-chrome-transcript-container",
    covers: [componentId("chrome/transcript-container", "TranscriptContainer")],
    async create(context) {
      const { TranscriptContainer, TranscriptBlock } = await context.load<typeof TranscriptContainerModule>("packages/tui/src/chrome/transcript-container.ts");
      const { Text } = await context.load<typeof TextModule>("packages/tui/src/components/text.ts");
      const transcript = new TranscriptContainer();
      const block = new TranscriptBlock();
      block.addChild(new Text("Container block alpha", 0, 0));
      transcript.addChild(block);
      const second = new TranscriptBlock();
      second.addChild(new Text("Container block beta", 0, 0));
      transcript.addChild(second);
      return transcript;
    },
    checkpoints: [{ name: "transcript blocks", comparisons: [shown("first block", "Container block alpha"), shown("second block", "Container block beta")] }],
  },
];

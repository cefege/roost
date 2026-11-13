import { mkdir, writeFile } from "node:fs/promises";
import type * as AssistantModule from "@omp-source/packages/tui/src/chat/assistant-message";
import type * as ImageModule from "@omp-source/packages/tui/src/components/image";
import type * as AskModule from "@omp-source/packages/tui/src/overlays/ask-dialog";
import type { Comparison, FixtureCase } from "../scenario";
import { contentComparison, controlledGallery } from "./gallery-support";

const component = (file: string, name: string) => `component:packages/tui/src/${file}.ts#${name}`;
const pixel = "iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAQAAAC1HAwCAAAAC0lEQVR42mP8/x8AAwMCAO+aN5sAAAAASUVORK5CYII=";

/** The thought's duration is the gap between two `performance.now` reads, so the fixture fixes the clock for each update. */
function atClock(milliseconds: number, update: () => void): void {
  const realNow = performance.now;
  performance.now = () => milliseconds;
  try { update(); } finally { performance.now = realNow; }
}

const sameRowProof = (name: string, terminalText: string, terminalRelative: string, selector: string, relativeTo: string, proves: Comparison["proves"]): Comparison => ({
  name,
  terminal: { type: "screen", pattern: terminalText, read: "sameRow", relativeTo: { pattern: terminalRelative } },
  browser: { type: "dom", selector, read: "sameRow", relativeTo },
  expected: true,
  proves,
});

const sharedMediaDirectory = "/tmp/omp-parity-media";
const sharedMediaPath = `${sharedMediaDirectory}/shown-media.png`;

const usage = { input: 0, output: 0, cacheRead: 0, cacheWrite: 0, totalTokens: 0, cost: { input: 0, output: 0, cacheRead: 0, cacheWrite: 0, total: 0 } };
const reply = (content: unknown[]) => ({ role: "assistant", content, api: "anthropic-messages", provider: "anthropic", model: "fixture", usage, stopReason: "stop", timestamp: 0 }) as never;

export const fixtureCases: FixtureCase[] = [
  {
    id: "primitive-assistant-reply-thought-and-stream",
    covers: [
      component("chat/assistant-message", "AssistantMessageComponent"),
      "property:TspMarkdownProps.stream", "property:TspSectionProps.collapsible", "property:TspSectionProps.collapsed", "property:TspSectionProps.took",
    ],
    async create(context) {
      const { AssistantMessageComponent } = await context.load<typeof AssistantModule>("packages/tui/src/chat/assistant-message.ts");
      const message = new AssistantMessageComponent();
      atClock(1000, () => message.updateContent(reply([{ type: "thinking", thinking: "Weighing the fixture options" }]), { transient: true }));
      return controlledGallery(context, message, {
        "n answer": () => atClock(4000, () => message.updateContent(reply([{ type: "thinking", thinking: "Weighing the fixture options" }, { type: "text", text: "Streaming answer **bold** text" }]), { transient: true })),
      });
    },
    checkpoints: [
      { name: "thinking is live", comparisons: [contentComparison("live thought", "(Weighing the fixture options)", '[data-omp-role="omp.thinking.live"]', "Weighing the fixture options")] },
      { name: "thought folds under a streaming answer", terminalInput: "n", browserActions: [{ type: "click", selector: 'button:has-text("n answer")' }], comparisons: [
        contentComparison("streamed answer", "(Streaming answer bold text)", '[data-omp-node="m.t1"] [data-name="Markdown"]', "Streaming answer bold text"),
        sameRowProof("streamed answer is one block", "Streaming answer", "bold text", '[data-omp-node="m.t1"] [data-name="Markdown"]', '[data-omp-node="m.t1"] [data-name="Markdown"] p', [
          { entry: "property:TspMarkdownProps.stream", effect: "layout", value: true },
        ]),
        sameRowProof("folded thought keeps its head", "Weighing the fixture options", "Weighing", '[data-omp-node="m.k0"]', '[data-omp-node="m.k0/head"]', [
          { entry: "property:TspSectionProps.collapsible", effect: "layout", value: true },
          { entry: "property:TspSectionProps.collapsed", effect: "layout", value: true },
          { entry: "property:TspSectionProps.took", effect: "layout", value: 3000 },
        ]),
      ] },
    ],
  },
  {
    id: "primitive-assistant-media-path",
    covers: [
      component("chat/assistant-message", "AssistantMessageComponent"),
      "kind:media", "property:TspMediaProps.path", "property:TspMediaProps.max", "property:TspMediaProps.max.h",
    ],
    async create(context) {
      const { AssistantMessageComponent } = await context.load<typeof AssistantModule>("packages/tui/src/chat/assistant-message.ts");
      await mkdir(sharedMediaDirectory, { recursive: true });
      await writeFile(sharedMediaPath, Buffer.from(pixel, "base64"));
      const message = new AssistantMessageComponent(reply([{ type: "text", text: "Media answer text" }]));
      message.setToolResultImages("call-media", [{ type: "image", data: pixel, mimeType: "image/png" }], sharedMediaPath);
      return message;
    },
    checkpoints: [{ name: "answer with a read media file", comparisons: [
      contentComparison("answer text", "(Media answer text)", '[data-omp-node="l.t0"]', "Media answer text"),
      {
        name: "media follows the answer",
        terminal: { type: "screen", pattern: "\\[Image: image/png\\]", read: "below", relativeTo: { pattern: "Media answer text" } },
        browser: { type: "dom", selector: '[data-omp-node="l.rcall-media"]', read: "below", relativeTo: '[data-omp-node="l.t0"]' },
        expected: true,
        proves: [
          { entry: "property:TspMediaProps.path", effect: "layout", value: sharedMediaPath },
          { entry: "property:TspMediaProps.max", effect: "layout", value: { h: "20lines" } },
          { entry: "property:TspMediaProps.max.h", effect: "layout", value: "20lines" },
        ],
      },
    ] }],
  },
  {
    id: "primitive-image-bounded-cells",
    covers: ["kind:image", "property:TspImageProps.blob", "property:TspImageProps.max", "property:TspImageProps.max.w", "property:TspImageProps.max.h", component("components/image", "Image")],
    async create(context) {
      const { Image } = await context.load<typeof ImageModule>("packages/tui/src/components/image.ts");
      return new Image(pixel, "image/png", { fallbackColor: text => context.theme.fg("dim", text) }, { filename: "bounded-pixel.png", maxWidthCells: 8, maxHeightCells: 4 });
    },
    checkpoints: [{ name: "bounded image", comparisons: [
      { name: "image alternative", terminal: { type: "screen", pattern: "(bounded-pixel\\.png)", group: 1 }, browser: { type: "dom", selector: 'img[alt*="bounded-pixel.png"]', read: "attribute", attribute: "alt", pattern: "(bounded-pixel\\.png)", group: 1 }, expected: "bounded-pixel.png" },
      {
        name: "bounded image sits on its alternative row",
        terminal: { type: "screen", pattern: "\\[Image: bounded-pixel\\.png", read: "sameRow", relativeTo: { pattern: "1x1\\]" } },
        browser: { type: "dom", selector: '[data-omp-node="l"]', read: "sameRow", relativeTo: '[data-omp-node="l"] img' },
        expected: true,
        proves: [
          { entry: "property:TspImageProps.blob", effect: "layout", value: "2e2ad429a32e40140feadda331bf38c01efa0a98f368bcb2a621bedff1e3ef7d" },
          { entry: "property:TspImageProps.max", effect: "layout", value: { w: "8ch", h: "4lines" } },
          { entry: "property:TspImageProps.max.w", effect: "layout", value: "8ch" },
          { entry: "property:TspImageProps.max.h", effect: "layout", value: "4lines" },
        ],
      },
    ] }],
  },
  {
    id: "primitive-ask-dialog-rows",
    covers: ["component:packages/tui/src/overlays/ask-dialog.ts#AskDialogComponent"],
    async create(context) {
      const { AskDialogComponent } = await context.load<typeof AskModule>("packages/tui/src/overlays/ask-dialog.ts");
      return new AskDialogComponent([
        { id: "policy", question: "Choose a rows policy", options: [{ label: "Drop them" }, { label: "Park them" }], recommended: 1 },
      ], { onSubmit: () => context.done(), onCancel: () => undefined, onPrompt: async () => undefined });
    },
    checkpoints: [{ name: "ask rows", comparisons: [contentComparison("question", "(Choose a rows policy)", '[data-omp-role="omp.ask.question"]', "Choose a rows policy")] }],
  },
];

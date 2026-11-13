import type { BuiltinCase } from "./scenario";
import type { BuiltinProviderTurn } from "./builtin-requirements";
import { composer, effect } from "./builtin-steps";
import { chatCase, content, markdownContent, messageRows, settled, toolComponent } from "./chat-steps";

const pixel = "iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAQAAAC1HAwCAAAAC0lEQVR42mP8/x8AAwMCAO+jRZkAAAAASUVORK5CYII=";
const createPixel = `${JSON.stringify(process.execPath)} -e 'await Bun.write("parity-pixel.png", Buffer.from("${pixel}", "base64"))'`;

export const builtinCases: BuiltinCase[] = [{
  ...chatCase("attachment-pipeline", "Parity create the scratch PNG attachment through the local coding tool.", [
    settled("PARITY_IMAGE_READY", [markdownContent("PARITY_IMAGE_READY"), effect("builtin.session", "bash", "/messages/2/toolName"), effect("builtin.session", false, "/messages/2/isError")]),
    {
      name: "Real path paste and native upload enter the canonical image composer",
      terminalInput: "\u001b[200~{{workspace}}/parity-pixel.png\u001b[201~",
      browserActions: [{ type: "upload", selector: '[data-omp-attachments="true"] input[type="file"]', files: ["{{workspace}}/parity-pixel.png"] }],
      comparisons: [content("PARITY_IMAGE_READY"), effect("builtin.session", 1, "/draftImageBlocks"), effect("builtin.session", 0, "/imageBlocks"),
        { name: "Staged attachment chip reflects the draft image", terminal: { type: "effect", channel: "builtin.session", pointer: "/draftImageBlocks" }, browser: { type: "dom", selector: '[data-omp-role="omp.composer.chip"]', read: "count" }, expected: 1 }],
    },
    {
      ...settled("PARITY_IMAGE_ACCEPTED", [
        markdownContent("PARITY_IMAGE_ACCEPTED"),
        { name: "Real user thumbnail reflects the canonical image block", terminal: { type: "effect", channel: "builtin.session", pointer: "/imageBlocks" }, browser: { type: "dom", selector: '[data-omp-role="omp.user.images"] img', read: "count" }, expected: 1 },
        effect("builtin.session", 0, "/draftImageBlocks"), effect("builtin.provider", 1, "/imageBlocks"), messageRows("user", 2),
      ]),
      terminalInput: "\r", browserActions: [{ type: "press", selector: composer, key: "Enter" }],
    },
  ], [toolComponent, "tool:bash", "kind:image", "component:packages/tui/src/prompt/attachment-chips.ts#AttachmentChipsBand"]),
  settings: { "tools.approvalMode": "yolo" },
}];

export const providerTurns: Record<string, BuiltinProviderTurn[]> = {
  "chat-attachment-pipeline": [
    { match: { textIncludes: "Parity create the scratch PNG attachment" }, toolCalls: [{ id: "chat-create-pixel", name: "bash", arguments: { command: createPixel, timeout: 10 } }] },
    { match: {}, text: "PARITY_IMAGE_READY" },
    { match: {}, text: "PARITY_IMAGE_ACCEPTED" },
  ],
};

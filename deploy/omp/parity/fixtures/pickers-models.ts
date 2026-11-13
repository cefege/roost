//! Model picker and copy selector proof scenarios: the real overlays over fixture models and a fixture
//! transcript, proving the picker properties their describe hooks emit. Registered by pickers.ts.
import type * as CatalogBuild from "@omp-source/packages/catalog/src/build";
import type * as ModelPicker from "@omp-source/packages/tui/src/overlays/model-picker";
import type * as CopySelector from "@omp-source/packages/tui/src/overlays/copy-selector";
import { visibleText, type FixtureCase } from "../scenario";
import { modelSource } from "./pickers";
import { pickerClaims } from "./pickers-claims-models";
import { below, coversFor, literal } from "./pickers-support";

const overlays = "packages/tui/src/overlays/";
const modelClaims = pickerClaims["model-rich"]!;
const copyClaims = pickerClaims["copy-timeline"]!;
const usage = { input: 0, output: 0, cacheRead: 0, cacheWrite: 0, totalTokens: 0, cost: { input: 0, output: 0, cacheRead: 0, cacheWrite: 0, total: 0 } };

export const fixtureCases: FixtureCase[] = [
  {
    id: "picker-model-rich",
    covers: coversFor(modelClaims),
    async create(context) {
      const { buildModel } = await context.load<typeof CatalogBuild>("packages/catalog/src/build.ts");
      const { ModelPickerComponent } = await context.load<typeof ModelPicker>(`${overlays}model-picker.ts`);
      const specs = [
        { id: "alpha", name: "Alpha coder", cost: 3, window: 32000, isNew: true },
        { id: "beta", name: "Beta coder", cost: 0, window: 200000, isBeta: true },
        { id: "gamma", name: "Gamma coder", cost: 1, window: 200000 },
        { id: "delta", name: "Delta coder", cost: 2, window: 200000 },
      ];
      const models = specs.map(spec => Object.assign(buildModel({
        id: spec.id, name: spec.name, provider: "parity", api: "openai-completions", baseUrl: "http://127.0.0.1:1/v1", reasoning: false, input: ["text"],
        cost: { input: spec.cost, output: spec.cost * 2, cacheRead: 0, cacheWrite: 0 }, contextWindow: spec.window, maxTokens: 4096,
      }), { isNew: spec.isNew, isBeta: spec.isBeta }));
      return new ModelPickerComponent(context.tui, modelSource, {
        getError: () => undefined, getAvailable: () => models, getAll: () => models, refreshIfStale: async () => false,
      }, models.map(model => ({ model })), {
        onPick: () => context.done(), onPickRole: () => context.done(), onCancel: () => context.done(),
      }, { currentSelector: "parity/beta", currentContextTokens: 100000, quickRoles: [{ role: "smol", model: models[2], explicitThinkingLevel: false }], quickRoleOrder: ["smol"], taskModeKeys: ["ctrl+t"] });
    },
    checkpoints: [
      { name: "rich", comparisons: [
        visibleText("model name", "(Beta coder)", 'text="Beta coder"', "Beta coder"),
        literal("context window", "(32k) ◫", { selector: '[data-omp-picker-item="parity/alpha"]', pattern: "Ctx: (\\d+[kmb]?)" }, "32k", []),
        below("model properties", { terminal: "parity/gamma", browser: '[data-omp-picker-item="parity/gamma"]' }, { terminal: "Switch Model", browser: 'h2:text-is("Switch Model")' }, modelClaims)] },
    ],
  },
  {
    id: "picker-copy-timeline",
    covers: coversFor(copyClaims),
    async create(context) {
      const { CopySelectorComponent } = await context.load<typeof CopySelector>(`${overlays}copy-selector.ts`);
      const stamp = "2026-11-01T12:00:00Z";
      const message = (id: string, parentId: string | null, body: unknown) => ({ type: "message", id, parentId, timestamp: stamp, message: body });
      const entries = [
        message("u1", null, { role: "user", content: "Plan the build", timestamp: 1 }),
        message("a1", "u1", { role: "assistant", content: [{ type: "text", text: "Here is the plan" }], usage, timestamp: 2 }),
        message("u2", "a1", { role: "user", content: "Read the readme", timestamp: 3 }),
        message("a2", "u2", { role: "assistant", content: [{ type: "toolCall", id: "c1", name: "read", arguments: { path: "README.md" } }], usage, timestamp: 4 }),
        message("t1", "a2", { role: "toolResult", toolCallId: "c1", toolName: "read", content: [{ type: "text", text: "file body" }], isError: false, timestamp: 5 }),
        { type: "model_change", id: "m1", parentId: "t1", timestamp: stamp, model: "parity/alpha" },
        message("u3", "m1", { role: "user", content: "Ship it", timestamp: 6 }),
      ];
      return new CopySelectorComponent(entries as never, { ui: context.tui, cwd: context.workspace, requestRender: () => context.tui.requestRender(), onPick: () => context.done(), onCancel: () => context.done() });
    },
    checkpoints: [
      { name: "timeline", comparisons: [below("turn properties", { terminal: "Read the readme", browser: '[data-omp-picker-item="u2"]' }, { terminal: "Copy · pick what", browser: 'h2:text-is("Copy")' }, copyClaims)] },
    ],
  },
];

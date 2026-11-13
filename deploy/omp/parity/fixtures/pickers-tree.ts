//! Session tree proof scenario: the real tree selector over a branched fixture session, proving the
//! picker properties its describe hook emits for turns and for the label editor. Registered by pickers.ts.
import type * as TreeSelector from "@omp-source/packages/tui/src/overlays/tree-selector";
import type { FixtureCase } from "../scenario";
import { pickerClaims } from "./pickers-claims-tree";
import { below, coversFor, literal } from "./pickers-support";

const heading = { terminal: "Session Tree", browser: 'h2:text-is("Session Tree")' };
const planRow = { terminal: "user: Plan the build", browser: '[data-omp-picker-item="u1"]' };
const turns = pickerClaims["tree-turns"]!;
const labelEditor = pickerClaims["tree-label"]!;

export const fixtureCases: FixtureCase[] = [{
  id: "picker-tree-outline",
  covers: coversFor(turns, labelEditor),
  async create(context) {
    const { TreeSelectorComponent } = await context.load<typeof TreeSelector>("packages/tui/src/overlays/tree-selector.ts");
    const stamp = "2026-11-01T12:00:00Z";
    const user = (id: string, parentId: string | null, text: string, label?: string) => ({ entry: { type: "message", id, parentId, timestamp: stamp, message: { role: "user", content: text, timestamp: 1 } }, ...(label ? { label } : {}) });
    const tree = [{
      ...user("u1", null, "Plan the build"), children: [{
        entry: { type: "model_change", id: "m1", parentId: "u1", timestamp: stamp, model: "parity/alpha" }, children: [{
          entry: { type: "message", id: "a1", parentId: "m1", timestamp: stamp, message: { role: "assistant", content: [{ type: "text", text: "Here is the plan" }], timestamp: 2 } }, children: [
            { entry: { type: "message", id: "t1", parentId: "a1", timestamp: stamp, message: { role: "toolResult", toolCallId: "c1", toolName: "read", content: [{ type: "text", text: "file" }], isError: false, timestamp: 3 } }, children: [{ ...user("u2", "t1", "Run the tests"), children: [] }] },
            { ...user("u3", "a1", "Ship it", "keep"), children: [] },
          ],
        }],
      }],
    }];
    return new TreeSelectorComponent(tree as never, "u2", 40, () => context.done(), () => context.done(), () => undefined, "all", "Fixture session");
  },
  checkpoints: [
    {
      name: "turns",
      comparisons: [
        literal("title", "(Session Tree)", { selector: 'h2:text-is("Session Tree")' }, "Session Tree", []),
        literal("user turn", "user: (Plan the build)", { selector: '[data-omp-picker-item="u1"] :text-is("user: Plan the build")', pattern: "^user: (Plan the build)$" }, "Plan the build", []),
        below("turn properties", planRow, heading, turns),
      ],
    },
    {
      name: "label editor",
      terminalInput: "\x1b[76;2u",
      browserActions: [{ type: "click", selector: 'button:not([role="tab"]):has-text("Label")' }],
      comparisons: [below("label properties", planRow, heading, labelEditor)],
    },
  ],
}];

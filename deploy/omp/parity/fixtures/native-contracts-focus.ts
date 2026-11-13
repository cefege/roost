import { writeFileSync } from "node:fs";
import type * as Ui from "@omp-source/packages/tui/src/index";
import type * as Hook from "@omp-source/packages/tui/src/overlays/hook-selector";
import type { NativeAssertion, NativeContract } from "../coverage-native";
import type { BrowserAction, Checkpoint, FixtureCase, FixtureContext } from "../scenario";
import { fileState, frameContract, eventContract, eventProperty, pickerNode } from "./native-contracts-support";

const statePath = "native-focus.json";
const textarea = (position: number) => `[data-omp-node="${position === 0 ? "n.input" : "o.input"}"] textarea`;

async function createFocusHost(context: FixtureContext) {
  const { Container, Editor, Text, getEditorTheme } = await context.load<typeof Ui>("packages/tui/src/index.ts");
  const { HookSelectorComponent } = await context.load<typeof Hook>("packages/tui/src/overlays/hook-selector.ts");
  const first = new Editor(getEditorTheme());
  const second = new Editor(getEditorTheme());
  first.setText("first focus target");
  second.setText("second focus target");
  const publish = () => {
    const snapshot = { first: first.focused, second: second.focused };
    writeFileSync(`${context.workspace}/${statePath}`, JSON.stringify(snapshot));
    context.record("native-focus", snapshot);
    context.tui.requestRender();
  };
  const controls = new HookSelectorComponent("Native focus actions", ["Focus second editor", "Focus first editor", "Focus controls", "Close native fixture"], choice => {
    if (choice === "Close native fixture") { context.done(); return; }
    context.tui.setFocus(choice === "Focus second editor" ? second : choice === "Focus first editor" ? first : controls);
    publish();
  }, () => context.done());
  const root = new Container();
  root.addChild(new Text("Native focus conformance", 0, 0));
  root.addChild(first);
  root.addChild(second);
  root.addChild(controls);
  context.tui.setFocus(first);
  publish();
  return Object.assign(root, {
    handleInput: (data: string) => controls.handleInput(data),
    ownsOverlayFocusTarget: (component: unknown) => component === first || component === second || component === controls,
  });
}

const activate = (index: number): BrowserAction => ({ type: "nativeEvent", selector: pickerNode, event: { ev: "activate", item: String(index) } });

export const focusCases = (ids: { first: string; second: string }): FixtureCase[] => {
  const focusMove = (name: string, index: number, target: "first" | "second"): Checkpoint => {
    const assertions: NativeAssertion[] = [
      fileState(statePath, "/first", target === "first"), fileState(statePath, "/second", target === "second"),
      { name: `actual ${target} editor holds browser focus`, browser: { type: "dom", selector: textarea(target === "first" ? 0 : 1), read: "focused" }, expected: true },
    ];
    return {
      name, browserActions: [activate(index)], comparisons: [],
      nativeContracts: [
        frameContract("operation:focus", assertions),
        frameContract("property:TspOp.[id]", assertions, "request", "/ops/1/1", undefined, ids[target]),
        frameContract("property:TspOp.[op]", assertions, "request", "/ops/1/0", undefined, "focus"),
      ],
    };
  };
  const pointerFocus: NativeAssertion[] = [
    fileState(statePath, "/first", true, "canonical focus stays inside the modal overlay"),
    fileState(statePath, "/second", false, "second editor stays unfocused canonically"),
    { name: "actual clicked editor keeps browser focus", browser: { type: "dom", selector: textarea(1), read: "focused" }, expected: true },
  ];
  const checkpoints: Checkpoint[] = [
    focusMove("canonical focus move reaches the second editor", 0, "second"),
    focusMove("canonical focus move returns to the first editor", 1, "first"),
    {
      name: "pointer focus event on a field inside the modal overlay keeps canonical focus",
      browserActions: [{ type: "click", selector: textarea(1) }], comparisons: [],
      nativeContracts: [eventContract("focus", pointerFocus, { "/id": ids.second }), eventProperty("focus", "id", { "/id": ids.second }, ids.second, pointerFocus)],
    },
    {
      name: "canonical focus leaving every field clears the focus operation target",
      browserActions: [activate(2)], comparisons: [],
      nativeContracts: [
        frameContract("operation:focus", [fileState(statePath, "/first", false), fileState(statePath, "/second", false)]),
        frameContract("property:TspOp.[id]", [fileState(statePath, "/first", false), fileState(statePath, "/second", false)], "request", "/ops/1/1", undefined, null),
      ],
    },
  ];
  return [{ id: "native-focus-operation-and-pointer-focus-event", covers: [], create: createFocusHost, checkpoints }];
};

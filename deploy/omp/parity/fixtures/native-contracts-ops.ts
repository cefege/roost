import { writeFileSync } from "node:fs";
import type * as Ui from "@omp-source/packages/tui/src/index";
import type * as Describe from "@omp-source/packages/tui/src/native/describe";
import type * as Settle from "@omp-source/packages/tui/src/native/settle";
import type * as Hook from "@omp-source/packages/tui/src/overlays/hook-selector";
import type { NativeAssertion, NativeContract } from "../coverage-native";
import type { BrowserAction, Checkpoint, FixtureCase, FixtureContext } from "../scenario";
import { fileState, frameContract, pickerNode } from "./native-contracts-support";

const statePath = "native-ops.json";
const scrollSteps = ["line-up", "line-down", "page-up", "page-down", "start", "end"] as const;
const revealSteps = ["start", "end", "nearest"] as const;
const menu = ["Append text", "Replace text", "Style node", "Insert node", "Move node", "Delete node", ...revealSteps.map(at => `Reveal ${at}`), ...scrollSteps.map(by => `Scroll ${by}`), "Edit markdown middle", "Settle component", "Append node last", "Close native fixture"];

async function createOpsHost(context: FixtureContext) {
  const { Container, Text } = await context.load<typeof Ui>("packages/tui/src/index.ts");
  const { node } = await context.load<typeof Describe>("packages/tui/src/native/describe.ts");
  const { settleNative } = await context.load<typeof Settle>("packages/tui/src/native/settle.ts");
  const { HookSelectorComponent } = await context.load<typeof Hook>("packages/tui/src/overlays/hook-selector.ts");
  const state = { texts: { alpha: "alpha", beta: "beta" } as Record<string, string>, order: ["alpha"], wrap: undefined as string | undefined, reveal: undefined as { at: string; n: number } | undefined, scroll: undefined as { by: string; n: number } | undefined, doc: "start middle end", settled: false, revision: 0 };
  const publish = () => {
    state.revision++;
    const snapshot = { texts: state.texts, order: state.order, wrap: state.wrap ?? null, reveal: state.reveal ?? null, scroll: state.scroll ?? null, doc: state.doc, settled: state.settled, revision: state.revision };
    writeFileSync(`${context.workspace}/${statePath}`, JSON.stringify(snapshot));
    context.record("native-ops", snapshot);
    context.tui.requestRender();
  };
  class OpsStage extends Text {
    describe() {
      const children = state.order.map(id => ({
        ...node("text", { text: state.texts[id]!, ...(state.wrap === undefined || id !== "alpha" ? {} : { wrap: state.wrap as "word" }) }, undefined, id),
        ...(id === "alpha" && state.reveal ? { reveal: state.reveal } : {}),
        ...(id === "alpha" && state.scroll ? { scroll: state.scroll } : {}),
      }) as ReturnType<typeof node>);
      return node("col", {}, [node("col", { gap: "sm" }, children, "stage"), node("md", { text: state.doc }, undefined, "doc")]);
    }
  }
  class SettledNote extends Text {
    constructor() { super("settled note", 0, 0); settleNative(this); }
    describe() { return node("text", { text: "settled note" }); }
  }
  const actions: Array<() => void> = [
    () => { state.texts.alpha += " tail"; },
    () => { state.texts.alpha = "replaced alpha"; },
    () => { state.wrap = "word"; },
    () => { state.order = ["beta", "alpha"]; },
    () => { state.order = ["alpha", "beta"]; },
    () => { state.order = ["alpha"]; },
    ...revealSteps.map(at => () => { state.reveal = { at, n: (state.reveal?.n ?? 0) + 1 }; }),
    ...scrollSteps.map(by => () => { state.scroll = { by, n: (state.scroll?.n ?? 0) + 1 }; }),
    () => { state.doc = "start MID end"; },
    () => { state.settled = true; root.addChild(new SettledNote()); },
    () => { state.order = ["alpha", "beta"]; },
  ];
  const controls = new HookSelectorComponent("Native operation actions", menu, choice => {
    const index = menu.indexOf(choice);
    if (choice === "Close native fixture") { context.done(); return; }
    actions[index]?.();
    publish();
  }, () => context.done());
  const root = new Container();
  root.addChild(new OpsStage("native operation stage", 0, 0));
  root.addChild(controls);
  publish();
  return Object.assign(root, { handleInput: (data: string) => controls.handleInput(data) });
}

const activate = (index: number): BrowserAction => ({ type: "nativeEvent", selector: pickerNode, event: { ev: "activate", item: String(index) } });
const opField = (entry: string, pointer: string, value: unknown, assertions: NativeAssertion[]): NativeContract =>
  frameContract(entry, assertions, "request", pointer, undefined, value);

export const opCases = (ids: { alpha: string; beta: string; stage: string }): FixtureCase[] => {
  const state = (pointer: string, expected: unknown) => fileState(statePath, pointer, expected);
  const checkpoints: Checkpoint[] = [];
  const step = (name: string, index: number, contracts: NativeContract[]) =>
    checkpoints.push({ name, browserActions: [activate(index)], comparisons: [], nativeContracts: contracts });
  const text = (name: string, index: number, mode: "append" | "replace", payload: string, assertions: NativeAssertion[]) => step(name, index, [
    frameContract("operation:text", assertions),
    opField("property:TspOp.[op]", "/ops/0/0", "text", assertions),
    opField("property:TspOp.[id]", "/ops/0/1", ids.alpha, assertions),
    opField("property:TspOp.[mode]", "/ops/0/2", mode, assertions),
    opField("property:TspOp.[text]", "/ops/0/3", payload, assertions),
  ]);
  text("text append operation extends the actual node", 0, "append", " tail", [state("/texts/alpha", "alpha tail")]);
  text("text replace operation rewrites the actual node", 1, "replace", "replaced alpha", [state("/texts/alpha", "replaced alpha")]);
  const tag = (op: string, assertions: NativeAssertion[], index = 0) => opField("property:TspOp.[op]", `/ops/${index}/0`, op, assertions);
  const styled = [state("/wrap", "word")];
  step("set operation carries changed props", 2, [
    frameContract("operation:set", styled), tag("set", styled),
    opField("property:TspOp.[props]", "/ops/0/2", { wrap: "word" }, styled),
    opField("property:TspOp.[props][key]", "/ops/0/2/wrap", "word", styled),
  ]);
  const inserted = [state("/order", ["beta", "alpha"])];
  step("add operation inserts before an existing sibling", 3, [
    frameContract("operation:add", inserted), tag("add", inserted),
    opField("property:TspOp.[parent]", "/ops/0/2", ids.stage, inserted),
    opField("property:TspOp.[before]", "/ops/0/3", ids.alpha, inserted),
    opField("property:TspOp.[node]", "/ops/0/4", { id: ids.beta, k: "text", p: { text: "beta" } }, inserted),
  ]);
  const moved = [state("/order", ["alpha", "beta"])];
  step("move operation reorders siblings", 4, [frameContract("operation:move", moved), tag("move", moved)]);
  const removed = [state("/order", ["alpha"])];
  step("del operation removes the node", 5, [frameContract("operation:del", removed), tag("del", removed, 1), opField("property:TspOp.[id]", "/ops/1/1", ids.beta, removed)]);
  revealSteps.forEach((at, offset) => {
    const assertions = [state("/reveal/at", at), state("/reveal/n", offset + 1)];
    step(`reveal ${at} operation targets the actual node`, 6 + offset, [frameContract("operation:reveal", assertions), tag("reveal", assertions, 1), opField("property:TspOp.[where]", "/ops/1/2", at, assertions)]);
  });
  scrollSteps.forEach((by, offset) => {
    const assertions = [state("/scroll/by", by), state("/scroll/n", offset + 1)];
    step(`scroll ${by} operation targets the actual node`, 9 + offset, [frameContract("operation:scroll", assertions), tag("scroll", assertions, 1), opField("property:TspOp.[by]", "/ops/1/2", by, assertions)]);
  });
  const spliced = [state("/doc", "start MID end")];
  step("splice operation rewrites the middle of a streaming markdown node", 15, [
    frameContract("operation:splice", spliced), tag("splice", spliced),
    opField("property:TspOp.[at]", "/ops/0/2", 6, spliced),
    opField("property:TspOp.[del]", "/ops/0/3", 6, spliced),
  ]);
  const settled = [state("/settled", true)];
  step("settle operation follows a finalized component", 16, [frameContract("operation:settle", settled), tag("settle", settled, 2)]);
  const appended = [state("/order", ["alpha", "beta"])];
  step("add operation appends as the last child", 17, [opField("property:TspOp.[before]", "/ops/0/3", null, appended)]);
  return [{ id: "native-operations-node-lifecycle-reveal-and-scroll", covers: [], create: createOpsHost, checkpoints }];
};

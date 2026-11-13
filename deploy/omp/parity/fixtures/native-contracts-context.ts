import { writeFileSync } from "node:fs";
import type * as Ui from "@omp-source/packages/tui/src/index";
import type * as Describe from "@omp-source/packages/tui/src/native/describe";
import type { NativeAssertion, NativeContract } from "../coverage-native";
import type { BrowserAction, Checkpoint, FixtureCase } from "../scenario";
import type { FixtureContext } from "../scenario";
import { domText, effectState, eventContract, eventProperty, fileState } from "./native-contracts-support";

const probeSelector = '[data-omp-node$=".probe"]';
const stateSelector = '[data-omp-node$=".state"]';
const statePath = "native-context.json";
const motionLabel = (reduceMotion: boolean) => `dark=true reduceMotion=${reduceMotion}`;

async function createContextHost(context: FixtureContext) {
  const { Text } = await context.load<typeof Ui>("packages/tui/src/index.ts");
  const { node } = await context.load<typeof Describe>("packages/tui/src/native/describe.ts");
  let describes = 0;
  let appearanceRefreshes = 0;
  const terminal = context.tui.terminal;
  const refreshAppearance = terminal.refreshAppearance?.bind(terminal);
  terminal.refreshAppearance = (...args: Parameters<NonNullable<typeof refreshAppearance>>) => {
    appearanceRefreshes++;
    context.record("native-appearance-refresh", { appearanceRefreshes });
    return refreshAppearance?.(...args);
  };
  class ContextProbe extends Text {
    describe(cx: { cols: number; dark: boolean; reduceMotion: boolean }) {
      const snapshot = { cols: cx.cols, dark: cx.dark, reduceMotion: cx.reduceMotion, describes: ++describes, refreshes: appearanceRefreshes };
      writeFileSync(`${context.workspace}/${statePath}`, JSON.stringify(snapshot));
      context.record("native-context", snapshot);
      return node("col", {}, [
        node("text", { text: `cols=${cx.cols}`, wrap: "none" }, undefined, "probe"),
        node("text", { text: `dark=${cx.dark} reduceMotion=${cx.reduceMotion}`, wrap: "none" }, undefined, "state"),
      ]);
    }
  }
  return new ContextProbe("native context probe", 0, 0);
}

function send(event: Record<string, unknown> & { ev: string }): BrowserAction {
  return { type: "nativeEvent", selector: probeSelector, event };
}

const fields = (ev: string, body: Record<string, unknown>, assertions: NativeAssertion[]): NativeContract[] =>
  Object.entries(body).filter(([field]) => field !== "cell" && field !== "ids").map(([field, value]) => eventProperty(ev, field, matchFor(ev, body), value, assertions));
const matchFor = (ev: string, body: Record<string, unknown>) => ev === "gone" ? { "/ids": body.ids } : ev === "error" ? { "/msg": body.msg } : ev === "visible" ? { "/visible": body.visible } : ev === "resize" ? { "/cols": body.cols } : ev === "theme" ? { "/dark": body.dark } : { "/reduce": body.reduce };

export const contextCases = (probeId: string): FixtureCase[] => {
  const steps: Checkpoint[] = [];
  const add = (name: string, event: Record<string, unknown> & { ev: string }, assertions: NativeAssertion[]) => {
    const { ev, ...body } = event;
    steps.push({ name, browserActions: [send(event)], comparisons: [], nativeContracts: [eventContract(ev, assertions, matchFor(ev, body)), ...fields(ev, body, assertions)] });
  };
  const motion = (reduceMotion: boolean): NativeAssertion[] => [
    domText(stateSelector, motionLabel(reduceMotion)), fileState(statePath, "/reduceMotion", reduceMotion), fileState(statePath, "/dark", true),
  ];
  add("resize event delivers geometry, cell metrics and visibility to the canonical context", { ev: "resize", cols: 91, rows: 27, cell: { w: 9, h: 18 }, visible: true }, [
    domText(probeSelector, "cols=91"), fileState(statePath, "/cols", 91),
  ]);
  add("reduced motion event reaches canonical context", { ev: "motion", reduce: true }, motion(true));
  add("full motion event restores canonical context", { ev: "motion", reduce: false }, motion(false));
  add("hidden visibility event is accepted without changing the canonical context", { ev: "visible", visible: false }, motion(false));
  add("visible visibility event is accepted without changing the canonical context", { ev: "visible", visible: true }, motion(false));
  add("terminal error report is logged without changing the canonical context", { ev: "error", msg: "native contract terminal error", op: 2, s: 1 }, motion(false));
  add("gone ids stop later operations from touching the forgotten node", { ev: "gone", ids: [probeId] }, motion(false));
  add("theme event asks the terminal to refresh its appearance", { ev: "theme", dark: false }, [
    effectState("native-appearance-refresh", "/appearanceRefreshes", 1),
  ]);
  return [{ id: "native-context-presentation-error-and-gone-events", covers: [], create: createContextHost, checkpoints: steps }];
};

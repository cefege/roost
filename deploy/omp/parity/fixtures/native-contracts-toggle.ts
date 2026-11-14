import type * as Ui from "@omp-source/packages/tui/src/index";
import type * as Describe from "@omp-source/packages/tui/src/native/describe";
import type { FixtureCase, FixtureContext } from "../scenario";
import { effectState, eventProperty } from "./native-contracts-support";

const phaseId = "phase-a";

async function createToggleHost(context: FixtureContext) {
  const { Text } = await context.load<typeof Ui>("packages/tui/src/index.ts");
  const { node } = await context.load<typeof Describe>("packages/tui/src/native/describe.ts");
  let collapsed = true;
  class ChecklistHost extends Text {
    describe() {
      const phases = [{ id: phaseId, title: "Phase A", collapsed, items: [{ id: "item-a", text: "first item", status: "pending" }] }];
      return node("col", {}, [node("checklist", { phases } as never, undefined, "phases")]);
    }
    handleNativeEvent(event: { type: string; part?: string; collapsed?: boolean }) {
      if (event.type !== "toggle" || event.part !== phaseId) return;
      collapsed = event.collapsed === true;
      context.record("native-toggle", { part: event.part, collapsed });
      context.tui.requestRender();
    }
  }
  return new ChecklistHost("native checklist host", 0, 0);
}

export const toggleCases = (): FixtureCase[] => [{
  id: "native-checklist-phase-toggle-carries-its-key",
  covers: [],
  create: createToggleHost,
  checkpoints: [false, true].map(collapsed => ({
    name: `${collapsed ? "collapsing" : "expanding"} a checklist phase reports its key`,
    browserActions: [{ type: "click", selector: '[data-omp-node$=".phases"] button >> nth=0' }],
    comparisons: [],
    nativeContracts: [eventProperty("toggle", "key", { "/collapsed": collapsed }, phaseId, [
      effectState("native-toggle", "/part", phaseId),
      effectState("native-toggle", "/collapsed", collapsed),
      { name: "native-controlled disclosure state", browser: { type: "dom", selector: '[data-omp-node$=".phases"] button', read: "expanded" }, expected: !collapsed },
    ])],
  })),
}];

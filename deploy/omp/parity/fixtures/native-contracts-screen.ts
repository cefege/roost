import type * as Ui from "@omp-source/packages/tui/src/index";
import type * as Describe from "@omp-source/packages/tui/src/native/describe";
import type { FixtureCase, FixtureContext } from "../scenario";
import { effectState } from "./native-contracts-support";

const probeSelector = '[data-omp-node$=".probe"]';
const fullscreenPresentation = { overlay: true, overlayOptions: { anchor: "top-left", width: "100%", maxHeight: "100%", margin: 0, fullscreen: true } } as const;

async function createScreenHost(context: FixtureContext) {
  const { Text } = await context.load<typeof Ui>("packages/tui/src/index.ts");
  const { node } = await context.load<typeof Describe>("packages/tui/src/native/describe.ts");
  class ScreenHost extends Text {
    describe() {
      return node("col", {}, [node("text", { text: "native screen host", wrap: "none" }, undefined, "probe")]);
    }
    handleNativeEvent(event: { type: string; act?: string }) {
      if (event.type !== "action" || event.act !== "close") return;
      context.record("native-screen-closed", { closed: true });
      context.done();
    }
  }
  return new ScreenHost("native screen host", 0, 0);
}

export const screenCases = (): FixtureCase[] => [{
  id: "native-screen-close-removes-the-surface",
  covers: [],
  presentation: fullscreenPresentation,
  create: createScreenHost,
  checkpoints: [{
    name: "closing the fullscreen host removes its actual screen surface",
    browserActions: [{ type: "nativeEvent", selector: probeSelector, event: { ev: "action", act: "close" } }],
    comparisons: [],
    nativeContracts: [{
      entry: "verb:x",
      request: { verb: "x", direction: "out", occurrence: "last", match: { "/keep": false } },
      assertions: [
        { name: "actual screen surface DOM is removed", browser: { type: "dom", selector: probeSelector, read: "count" }, expected: 0 },
        effectState("native-screen-closed", "/closed", true),
      ],
    }],
  }],
}];

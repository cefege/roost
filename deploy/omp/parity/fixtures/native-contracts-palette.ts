import { writeFileSync } from "node:fs";
import type * as Ui from "@omp-source/packages/tui/src/index";
import type * as Describe from "@omp-source/packages/tui/src/native/describe";
import type * as ThemeModule from "@omp-source/packages/tui/src/theme/theme";
import type { NativeAssertion, NativeContract } from "../coverage-native";
import type { Checkpoint, FixtureCase, FixtureContext } from "../scenario";
import { fileState } from "./native-contracts-support";

const probeSelector = '[data-omp-node$=".probe"]';
const scopeSelector = "[data-omp-theme-scope]";
const statePath = "native-palette.json";
const accent = { light: "#5a8080", dark: "#febc38" } as const;

async function createPaletteHost(context: FixtureContext) {
  const { Text } = await context.load<typeof Ui>("packages/tui/src/index.ts");
  const { node } = await context.load<typeof Describe>("packages/tui/src/native/describe.ts");
  const themes = await context.load<typeof ThemeModule>("packages/tui/src/theme/theme.ts");
  let applied = "";
  class PaletteSwitcher extends Text {
    describe() {
      return node("col", {}, [node("text", { text: `theme ${applied || "initial"}`, wrap: "none" }, undefined, "probe")]);
    }
    handleNativeEvent(event: { type: string; act?: string; value?: unknown }) {
      if (event.type !== "action" || event.act !== "theme" || typeof event.value !== "string") return;
      void themes.setTheme(event.value).then(result => {
        applied = event.value as string;
        writeFileSync(`${context.workspace}/${statePath}`, JSON.stringify({ theme: applied, success: result.success, accent: themes.theme.getResolvedColors().accent }));
        context.record("native-palette", { theme: applied, success: result.success });
        this.invalidate();
      });
    }
  }
  return new PaletteSwitcher("native palette switcher", 0, 0);
}

const paletteContracts = (name: keyof typeof accent, assertions: NativeAssertion[]): NativeContract[] => [
  { entry: "verb:t", request: { verb: "t", direction: "out", occurrence: "last", match: { "/colors/accent": accent[name] } }, assertions },
  { entry: "property:TspPalette.colors", request: { verb: "t", direction: "out", occurrence: "last", match: { "/colors/accent": accent[name] }, pointer: "/colors" }, assertions },
  { entry: "property:TspPalette.colors[key]", request: { verb: "t", direction: "out", occurrence: "last", match: { "/colors/accent": accent[name] }, pointer: "/colors/accent" }, value: accent[name], assertions },
];

export const paletteCases = (): FixtureCase[] => {
  const checkpoints: Checkpoint[] = (["light", "dark"] as const).map(name => {
    const assertions: NativeAssertion[] = [
      { name: `actual scope exposes the ${name} accent as a CSS variable`, browser: { type: "dom", selector: scopeSelector, read: "css", property: "--omp-theme-accent" }, expected: accent[name] },
      fileState(statePath, "/theme", name), fileState(statePath, "/accent", accent[name]),
    ];
    return {
      name: `${name} palette push re-colors the actual surface`,
      browserActions: [{ type: "nativeEvent", selector: probeSelector, event: { ev: "action", act: "theme", value: name } }],
      comparisons: [], nativeContracts: paletteContracts(name, assertions),
    };
  });
  return [{ id: "native-palette-push-recolors-surface", covers: [], create: createPaletteHost, checkpoints }];
};

import type { FixtureCase, Checkpoint } from "../scenario";
import { callbackValue, visibleText } from "../scenario";
import { getComposerShapeOptions } from "@omp-source/packages/tui/src/overlays/composer-shape-registry";
import { CURRENT_SETUP_VERSION } from "@omp-source/packages/tui/src/setup/setup-version";
import { SETUP_OUTRO_MS } from "@omp-source/packages/tui/src/setup/scenes/outro";
import { SETUP_TICK_MS } from "@omp-source/packages/tui/src/setup/scenes/splash";
import { SYMBOL_PRESETS } from "@omp-source/packages/tui/src/theme/symbols";
import { proveSetupFixture } from "./setup-proofs";
import { createSetup } from "./setup-support";
import { attribute, escapePattern } from "./settings-support";

const down = "\x1b[B";
const listItem = (label: string): string => `[data-omp-role='omp.select'] button:has-text(${attribute(label)})`;
function previewSelection(sourceLabel: string) {
  // SelectList.sanitizeSingleLine collapses whitespace runs in every item label it draws.
  const label = sourceLabel.replace(/\s+/g, " ").trim();
  return { name: "canonical preview selection", terminal: { type: "screen" as const, pattern: `${escapePattern(SYMBOL_PRESETS.unicode["nav.cursor"])}\\s+${escapePattern(label)}`, read: "visible" as const },
    browser: { type: "dom" as const, selector: `[data-omp-role='omp.select'] [data-omp-item-selected=true]:has(button:has-text(${attribute(label)}))`, read: "visible" as const }, expected: true };
}
function completion(saved: Record<string, unknown> = {}): Checkpoint {
  return { name: "finish and persist setup version", waitForMilliseconds: SETUP_OUTRO_MS + SETUP_TICK_MS,
    comparisons: [callbackValue("setup completion", "setup-version", CURRENT_SETUP_VERSION), {
      name: "saved setup version", terminal: { type: "effect", channel: "saved-setup", pointer: "/version" }, browser: { type: "effect", channel: "saved-setup", pointer: "/version" }, expected: CURRENT_SETUP_VERSION,
    }, ...Object.entries({ providerAuthorized: false, ...saved }).map(([field, expected]) => ({
      name: `saved setup ${field}`, terminal: { type: "effect" as const, channel: "saved-setup", pointer: `/${field}` },
      browser: { type: "effect" as const, channel: "saved-setup", pointer: `/${field}` }, expected,
    }))] };
}
function cancelScene(scene: string, title: string, selector: string): FixtureCase {
  return { id: `setup-${scene}-cancel`, covers: [`scene:${scene}`, "setup:cancel"], create: context => createSetup(context, scene), checkpoints: [
    { name: "actual scene", comparisons: [visibleText("scene title", `(${escapePattern(title)})`, selector, title)] },
    { name: "cancel into outro", terminalInput: "\x1b", browserActions: [{ type: "click", selector: "button:text-is('Skip')" }], comparisons: [callbackValue("scene cancellation", "scene-finish", "skipped")] }, completion({ composer: "band", symbolPreset: "unicode", dark: "titanium", light: "light" }),
  ] };
}

export const glyphFixtureCases: FixtureCase[] = ["nerd", "unicode", "ascii"].map((preset, index) => ({
  id: `setup-glyph-${preset}`, covers: ["scene:glyphSetupScene", "scene:glyph-mode", `setup:glyph:${preset}`, "setup:preview", "setup:commit"],
  create: context => createSetup(context, "glyph-mode"), checkpoints: [
    { name: "glyph scene", comparisons: [visibleText("glyph title", "(Choose glyph mode)", "[data-omp-role='omp.setup.title']", "Choose glyph mode")] },
    { name: "preview selected glyph without committing", terminalInput: String(index + 1), browserActions: [{ type: "press", key: String(index + 1) }],
      comparisons: [previewSelection(`${index + 1}  ${["Nerd Font", "Unicode", "ASCII"][index]}`)] },
    { name: "preview and select", terminalInput: "\r", browserActions: [{ type: "click", selector: "button:text-is('Confirm')" }], comparisons: [callbackValue("saved glyph", "symbolPreset", preset)] }, completion({ symbolPreset: preset }),
  ],
}));
export const composerFixtureCases: FixtureCase[] = getComposerShapeOptions().map((shape, index) => ({
  id: `setup-composer-${shape.value}`, covers: ["scene:composerSetupScene", "scene:composer-shape", `composer:${shape.value}`, "setup:preview", "setup:commit"],
  create: context => createSetup(context, "composer-shape"), checkpoints: [
    { name: "preview", comparisons: [visibleText("preview content", "(Preview:)", "[data-omp-role='omp.setup.composer'] :text-is('Preview:')", "Preview:")] },
    { name: "preview selected composer without committing", terminalInput: String(index + 1), browserActions: [{ type: "press", key: String(index + 1) }],
      comparisons: [previewSelection(`${index + 1}  ${shape.label}`), visibleText("live composer preview", "(Preview:)", "[data-omp-role='omp.setup.composer'] :text-is('Preview:')", "Preview:")] },
    { name: "commit", terminalInput: "\r", browserActions: [{ type: "click", selector: "button:text-is('Confirm')" }], comparisons: [callbackValue("saved composer", "composer.shape", shape.value)] }, completion({ composer: shape.value }),
  ],
}));
export const modelFixtureCases: FixtureCase[] = ["alpha", "beta"].map((id, index) => ({
  id: `setup-model-${id}`, covers: ["scene:modelSetupScene", "scene:model", "setup:model:save"], create: context => createSetup(context, "model"), checkpoints: [
    { name: "model list", comparisons: [{ ...visibleText("model label", `(?:^|\\s|/)(${id})(?=\\s|$)`, `[data-omp-picker-item=${attribute(`parity-local/${id}`)}] :text-is(${attribute(`parity-local/${id}`)})`, id),
      browser: { type: "dom", selector: `[data-omp-picker-item=${attribute(`parity-local/${id}`)}] :text-is(${attribute(`parity-local/${id}`)})`, read: "text", pattern: `/(${id})$`, group: 1 } }] },
    { name: "preview selected model", terminalInput: down.repeat(index), browserActions: index ? [{ type: "click", selector: `[data-omp-picker-item=${attribute(`parity-local/${id}`)}]` }] : [],
      comparisons: [{ name: "selected model preview", terminal: { type: "screen", pattern: `${escapePattern(SYMBOL_PRESETS.unicode["nav.cursor"])}[^\\n]*\\b${id}\\b`, read: "visible" },
        browser: { type: "dom", selector: `[data-omp-picker-item=${attribute(`parity-local/${id}`)}][aria-selected=true]`, read: "visible" }, expected: true }] },
    { name: "persist selected model", terminalInput: "\r", browserActions: [{ type: "click", selector: "button:text-is('Set as default')" }], comparisons: [callbackValue("default model", "model", `parity-local/${id}`)] }, completion({ model: `parity-local/${id}` }),
  ],
}));
const themeChoices = [{ value: "theme:titanium", label: "Titanium", key: "theme.dark", expected: "titanium" }, { value: "theme:light", label: "Light", key: "theme.light", expected: "light" }, { value: "colorblind", label: "Colorblind colors", key: "colorBlindMode", expected: true }, { value: "ansi", label: "ANSI-safe", key: "symbolPreset", expected: "ascii" }];
export const themeFixtureCases: FixtureCase[] = themeChoices.map((choice, index) => ({
  id: `setup-theme-${index}`, covers: ["scene:themeSetupScene", "scene:theme", `setup:theme:${choice.value}`, "setup:preview", "setup:commit"], create: context => createSetup(context, "theme"), checkpoints: [
    { name: "theme preview", comparisons: [visibleText("preview label", "(Preview)", "[data-omp-role='omp.setup.theme.preview'] :text-is('Preview')", "Preview")] },
    { name: "preview selected theme without committing", terminalInput: String(index + 2), browserActions: [{ type: "press", key: String(index + 2) }], comparisons: [previewSelection(choice.label)] },
    { name: "commit theme", terminalInput: "\r", browserActions: [{ type: "click", selector: "button:text-is('Confirm')" }], comparisons: [
      callbackValue("theme persistence", choice.key, choice.expected),
      ...(choice.value === "ansi" ? [callbackValue("ANSI-compatible dark theme", "theme.dark", "dark-terminal")] : []),
    ] }, completion({ [choice.key.replace("theme.", "")]: choice.expected, ...(choice.value === "ansi" ? { dark: "dark-terminal" } : {}) }),
  ],
}));
export const providerFixtureCases: FixtureCase[] = [{ id: "setup-provider-local-login", covers: ["scene:providersSetupScene", "scene:providers", "setup:signin:prompt", "setup:signin:commit"], create: context => createSetup(context, "providers"), checkpoints: [
  { name: "provider list", comparisons: [visibleText("provider", "(Parity Local)", "[data-omp-picker-item]:has-text('Parity Local') :text-is('Parity Local')", "Parity Local")] },
  { name: "open local provider", terminalInput: "parity\r", browserActions: [{ type: "doubleClick", selector: "[data-omp-picker-item]:has-text('Parity Local')" }], comparisons: [visibleText("code prompt", "(Local fixture code)", "[data-omp-role='omp.login.label']", "Local fixture code")] },
  { name: "commit credentials", terminalInput: "fixture-code\r", browserActions: [{ type: "fill", selector: "[data-omp-role='omp.login.prompt'] input", value: "fixture-code" }, { type: "press", selector: "[data-omp-role='omp.login.prompt'] input", key: "Enter" }], comparisons: [callbackValue("provider refresh", "provider", "parity-local"), callbackValue("credentials survived reopening actual store", "saved-provider", true)] },
  { name: "continue after signin", terminalInput: "\x1b", browserActions: [{ type: "click", selector: "button:text-is('Continue')" }], comparisons: [callbackValue("scene completion", "scene-finish", "done")] }, completion({ providerAuthorized: true }),
]}];
export const cancelFixtureCases: FixtureCase[] = [
  cancelScene("glyph-mode", "Choose glyph mode", "[data-omp-role='omp.setup'] :text-is('Choose glyph mode')"),
  cancelScene("composer-shape", "Choose composer shape", "[data-omp-role='omp.setup'] :text-is('Choose composer shape')"),
  cancelScene("theme", "Pick a theme", "[data-omp-role='omp.setup'] :text-is('Pick a theme')"),
  cancelScene("model", "Choose your default model", "[data-omp-role='omp.setup.sheet'] :text-is('Choose your default model')"),
  cancelScene("providers", "Sign in to your providers", "[data-omp-role='omp.setup.sheet'] :text-is('Sign in to your providers')"),
];
export const transitionFixtureCases: FixtureCase[] = [
  { id: "setup-theme-system", covers: ["scene:themeSetupScene", "setup:theme:auto"], create: context => createSetup(context, "theme"), checkpoints: [
    { name: "system theme choice", comparisons: [visibleText("system choice", "(Match terminal)", `${listItem("Match terminal")} span:first-child`, "Match terminal")] },
    { name: "save system theme", terminalInput: "1\r", browserActions: [{ type: "click", selector: listItem("Match terminal") }], comparisons: [callbackValue("dark default", "theme.dark", "titanium"), callbackValue("light default", "theme.light", "light")] }, completion(),
  ] },
  { id: "setup-theme-browse-back-cancel", covers: ["scene:themeSetupScene", "setup:theme:browse", "setup:back", "setup:cancel"], create: context => createSetup(context, "theme"), checkpoints: [
    { name: "open all themes", terminalInput: "6\r", browserActions: [{ type: "click", selector: listItem("Browse all…") }], comparisons: [{ name: "all themes", terminal: { type: "screen", pattern: "(Browsing all themes)", group: 1 }, browser: { type: "dom", selector: "[data-omp-role='omp.setup.theme'] :text-matches('^Browsing all themes')", read: "text", pattern: "(Browsing all themes)", group: 1 }, expected: "Browsing all themes" }] },
    { name: "back without committing", terminalInput: "\x1b", browserActions: [{ type: "click", selector: "button:text-is('Back')" }], comparisons: [visibleText("curated choices", "(Match terminal)", `${listItem("Match terminal")} span:first-child`, "Match terminal")] },
    { name: "cancel preview", terminalInput: "3\x1b", browserActions: [{ type: "click", selector: "button:text-is('Skip')" }], comparisons: [callbackValue("scene cancellation", "scene-finish", "skipped")] },
    { ...completion(), comparisons: [...completion().comparisons, { name: "cancel leaves saved theme", terminal: { type: "effect", channel: "saved-setup", pointer: "/dark" }, browser: { type: "effect", channel: "saved-setup", pointer: "/dark" }, expected: "titanium" }] },
  ] },
  { id: "setup-model-save-error", covers: ["scene:modelSetupScene", "setup:model:error"], create: context => createSetup(context, "model", true), checkpoints: [
    { name: "model before failure", comparisons: [visibleText("model title", "(Choose your default model)", "[data-omp-role='omp.setup.sheet'] :text-is('Choose your default model')", "Choose your default model")] },
    { name: "failed persistence stays in scene", terminalInput: "\r", browserActions: [{ type: "click", selector: "button:text-is('Set as default')" }], comparisons: [visibleText("save error", "(Couldn't save the default model: Parity persistence refused)", "[data-omp-role='omp.setup.sheet'] :text-is(\"Couldn't save the default model: Parity persistence refused\")", "Couldn't save the default model: Parity persistence refused")] },
    { name: "cancel failed save", terminalInput: "\x1b", browserActions: [{ type: "click", selector: "button:text-is('Skip')" }], comparisons: [callbackValue("scene cancellation", "scene-finish", "skipped")] }, completion(),
  ] },
  { id: "setup-provider-prompt-cancel", covers: ["scene:providersSetupScene", "setup:signin:cancel"], create: context => createSetup(context, "providers"), checkpoints: [
    { name: "open local login", terminalInput: "parity\r", browserActions: [{ type: "doubleClick", selector: "[data-omp-picker-item]:has-text('Parity Local')" }], comparisons: [visibleText("prompt", "(Local fixture code)", "[data-omp-role='omp.login.label']", "Local fixture code")] },
    { name: "cancel pending code", terminalInput: "\x1b", browserActions: [{ type: "click", selector: "button:text-is('Cancel')" }], comparisons: [visibleText("provider returned", "(Sign in to your providers)", "[data-omp-role='omp.setup.sheet'] :text-is('Sign in to your providers')", "Sign in to your providers")] },
    { name: "skip login", terminalInput: "\x1b", browserActions: [{ type: "click", selector: "button:text-is('Skip')" }], comparisons: [callbackValue("scene cancellation", "scene-finish", "skipped")] }, completion(),
  ] },
];
const sceneControllers: Readonly<Record<string, string>> = {
  glyphSetupScene: "glyph.ts#GlyphSceneController", composerSetupScene: "composer.ts#ComposerSceneController", modelSetupScene: "model.ts#ModelSceneController",
  themeSetupScene: "theme.ts#ThemeSceneController", providersSetupScene: "sign-in.ts#SignInScene",
  "glyph-mode": "glyph.ts#GlyphSceneController", "composer-shape": "composer.ts#ComposerSceneController", model: "model.ts#ModelSceneController",
  theme: "theme.ts#ThemeSceneController", providers: "sign-in.ts#SignInScene",
};
const controllerFor = (covers: readonly string[]): string[] => [...new Set(covers.flatMap(entry => {
  const controller = entry.startsWith("scene:") ? sceneControllers[entry.slice("scene:".length)] : undefined;
  return controller ? [`component:packages/tui/src/setup/scenes/${controller}`] : [];
}))];
export const fixtureCases: FixtureCase[] = [...glyphFixtureCases, ...composerFixtureCases, ...modelFixtureCases, ...themeFixtureCases, ...providerFixtureCases, ...cancelFixtureCases, ...transitionFixtureCases].map(fixture => proveSetupFixture({
  ...fixture, covers: [...fixture.covers, "component:packages/tui/src/setup/wizard-overlay.ts#SetupWizardComponent", ...controllerFor(fixture.covers)],
}));

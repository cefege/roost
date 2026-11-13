import { getComposerShapeOptions } from "@omp-source/packages/tui/src/overlays/composer-shape-registry";
import type { FixtureCase, Comparison } from "../scenario";
import { attribute, caseId, createSettings, effects, escapePattern, labelControl, selectedLabel, settingDefinitions } from "./settings-support";

function previewCases(path: "composer.shape" | "snapcompact.shape"): FixtureCase[] {
  const definition = settingDefinitions.find(entry => entry.definition.path === path)?.definition;
  if (!definition || definition.type !== "submenu") throw new Error(`Missing actual preview setting ${path}`);
  const options = path === "composer.shape" ? getComposerShapeOptions() : definition.options;
  const initial = options[0];
  if (!initial) throw new Error(`Missing actual preview choices ${path}`);
  return options.map((option, index) => {
    const role = path === "composer.shape" ? "omp.preview.composer-shape" : "omp.preview.snapcompact-shape";
    const caption = path === "composer.shape" ? "Preview:" : `Sample (zoomed) · ${option.value === "auto" ? "auto →" : option.value}`;
    const preview: Comparison = {
      name: "real preview caption", terminal: { type: "screen", pattern: `(${escapePattern(caption)})`, group: 1 },
      browser: { type: "dom", selector: `[data-omp-role=${attribute(role)}]`, read: "text", pattern: `(${escapePattern(caption)})`, group: 1 }, expected: caption,
    };
    return {
      id: `settings-${caseId(path)}-real-preview-${index}`,
      covers: [`setting:${path}`, "settings:preview", "settings:cancel", `component:packages/tui/src/overlays/${path === "composer.shape" ? "composer-shape-preview.ts#ComposerShapePreview" : "snapcompact-shape-preview.ts#SnapcompactShapePreview"}`],
      create: context => createSettings(context, definition, { [path]: initial.value }),
      checkpoints: [
        { name: "initial row", comparisons: [selectedLabel(definition)] },
        { name: "highlight real preview", terminalInput: `\r${"\x1b[B".repeat(index)}`, browserActions: [
          { type: "click", selector: `button${labelControl(definition.label)}` },
          { type: "hover", selector: `[role=option]:has(span:text-is(${attribute(option.label)}))` },
        ], comparisons: [preview, ...(path === "composer.shape" ? [{ name: "real composer preview drawing", terminal: { type: "screen" as const, pattern: "(Ask anything, edit files, run tools)", group: 1 }, browser: { type: "dom" as const, selector: "[data-omp-role='omp.preview.composer-shape.drawing']", read: "text" as const, pattern: "(Ask anything, edit files, run tools)", group: 1 }, expected: "Ask anything, edit files, run tools" }] : [])] },
        { name: "cancel does not persist preview", terminalInput: "\x1b", browserActions: [{ type: "press", key: "Escape" }], comparisons: [selectedLabel(definition)] },
        { name: "commit preview choice", terminalInput: `\r${"\x1b[B".repeat(index)}\r`, browserActions: [
          { type: "click", selector: `button${labelControl(definition.label)}` },
          { type: "click", selector: `[role=option]:has(span:text-is(${attribute(option.label)}))` },
        ], comparisons: [selectedLabel(definition), ...effects(path, option.value)] },
      ],
    };
  });
}

export const composerPreviewFixtureCases = previewCases("composer.shape");
export const snapcompactPreviewFixtureCases = previewCases("snapcompact.shape");
export const fixtureCases = [...composerPreviewFixtureCases, ...snapcompactPreviewFixtureCases];

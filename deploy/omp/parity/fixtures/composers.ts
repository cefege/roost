import type { Comparison, FixtureCase } from "../scenario";
import { probe } from "../probe";
import { callbackValue } from "../scenario";
import { assertRegistry, contentComparison, controlledGallery } from "./gallery-support";

const styles = ["band", "box", "claude", "pi", "borderless", "rule", "field", "rail"] as const;
const dynamicStyle = "parity-dynamic";

// The gallery opens as a modal overlay above the session's own composer, which
// both legs keep mounted; every live-editor selector is scoped to the overlay.
const liveInput = '[role="dialog"] textarea:not([readonly])';
const previewRoot = '[role="dialog"] [data-omp-role="omp.preview.composer-shape"]';
const chromeRoot = '[role="dialog"] [data-omp-role="omp.editor"] [data-omp-composer-chrome="true"]';
const prefixDrawings: Readonly<Record<string, { text: string; styled: boolean }>> = {
  band: { text: "╰─ ", styled: true },
  box: { text: "╰─ ", styled: true },
  claude: { text: "❯ ", styled: false },
  borderless: { text: "❯ ", styled: false },
  rule: { text: "❯ ", styled: false },
  field: { text: "▐", styled: true },
  rail: { text: "▎", styled: true },
  [dynamicStyle]: { text: "╰─ ", styled: true },
};

function liveChromeComparisons(shape: string, multiline = false): Comparison[] {
  const labels = multiline ? ["Parity first", "Parity middle", "Parity last"] : ["Parity composer draft"];
  const drawing = prefixDrawings[shape];
  const comparisons: Comparison[] = [];
  if (drawing) {
    for (const [index, label] of labels.entries()) {
      if (multiline && index && !["box", "field", "rail", dynamicStyle].includes(shape)) continue;
      const value = multiline && index < 2 && ["box", dynamicStyle].includes(shape) ? "│  " : drawing.text;
      const escaped = value.replace(/[.*+?^${}()|[\]\\]/g, "\\$&");
      const row = `${chromeRoot} [data-omp-composer-chrome-part="prefix"] > div:nth-child(${index + 1})`;
      const selector = drawing.styled ? `${row} span:text-is(${JSON.stringify(value)})` : row;
      const entry = drawing.styled ? "property:TspSpan.t" : "property:TspEditorChromeRow.prefix";
      comparisons.push({
        name: `actual ${multiline ? ["first", "middle", "last"][index] : "only"} composer prefix`,
        terminal: { type: "screen", pattern: `(${escaped}) *${label}`, group: 1 },
        browser: { type: "dom", selector, read: "text" },
        expected: value,
        proves: [
          { entry, effect: "literal", value },
          ...(!multiline && shape !== dynamicStyle ? [{ entry: `composer:${shape}`, effect: "literal" as const, value }] : []),
        ],
      });
      comparisons.push({
        name: `actual ${multiline ? ["first", "middle", "last"][index] : "only"} prefix stays left of input`,
        terminal: { type: "screen", pattern: `(${escaped}) *${label}`, group: 1, read: "leftHalf" },
        browser: { type: "dom", selector, read: "leftHalf", relativeTo: chromeRoot },
        expected: true,
        proves: [{ entry, effect: "layout", value }],
      });
    }
  }
  if (shape === dynamicStyle) {
    comparisons.push({
      name: "actual registered top drawing",
      terminal: { type: "screen", pattern: `(Parity dynamic composer)[^\\n]*\\n[^\\n]*${labels[0]}`, group: 1 },
      browser: { type: "dom", selector: `${chromeRoot} [data-omp-composer-chrome-part="top"] span:text-is("Parity dynamic composer")`, read: "text" },
      expected: "Parity dynamic composer",
      proves: [{ entry: "property:TspSpan.t", effect: "literal", value: "Parity dynamic composer" }],
    });
  }
  for (const part of shape === "pi" || shape === "claude" ? ["top", "bottom"] : shape === "box" || shape === "rule" ? ["top"] : []) {
    const rule = shape === "box" ? "(╭─)" : "(─{40})";
    comparisons.push({
      name: `actual composer ${part} rule`,
      terminal: { type: "screen", pattern: rule, group: 1 },
      browser: { type: "dom", selector: `${chromeRoot} [data-omp-composer-chrome-part="${part}"]`, read: "text", pattern: rule, group: 1 },
      expected: shape === "box" ? "╭─" : "─".repeat(40),
      proves: [{ entry: `composer:${shape}`, effect: "literal", value: shape === "box" ? "╭─" : "─".repeat(40) }],
    });
  }
  if (!multiline && ["band", "box", "claude", "pi", "rule", dynamicStyle].includes(shape)) {
    comparisons.push({
      name: "editable draft sits below production preview chrome",
      terminal: {
        type: "screen", pattern: "(Parity composer draft)", group: 1, read: "below",
        relativeTo: { pattern: shape === "band" ? "(Plan)" : shape === dynamicStyle ? "(Parity dynamic composer)" : "(─+)" },
      },
      browser: { type: "dom", selector: liveInput, read: "below", relativeTo: previewRoot },
      expected: true,
    });
  }
  if (!multiline && shape === "band") {
    comparisons.push({
      name: "production status appears in live band",
      terminal: { type: "screen", pattern: "(Plan)", group: 1 },
      browser: {
        type: "dom", selector: `${chromeRoot} [data-omp-composer-chrome-part="top"] span:has-text("Plan"):not(:has(span))`,
        read: "text", pattern: "(Plan)", group: 1,
      },
      expected: "Plan",
    }, {
      name: "production band cell background",
      terminal: { type: "screen", pattern: "(Plan)", group: 1, read: "background" },
      browser: {
        type: "dom", selector: `${chromeRoot} [data-omp-composer-chrome-part="top"] span:has-text("Plan"):not(:has(span))`,
        read: "css", property: "background-color",
      },
    });
  }
  if (!multiline && (shape === "field" || shape === "rail")) {
    comparisons.push({
      name: "editable cells retain source surface paint",
      terminal: { type: "screen", pattern: "(Parity composer draft)", group: 1, read: "background" },
      browser: { type: "dom", selector: `${chromeRoot} [data-omp-composer-chrome-part="input"]`, read: "css", property: "background-color" },
    });
  }
  return comparisons;
}

// Wider than any terminal row, so the style's own truncation branch runs on the real editor.
const clippedStatus = `Clipped status chip ${"overflowing the composer top border ".repeat(6)}`.trimEnd();

const composerCase = (shape: string, variant: "live" | "clipped" | "ime-safe" = "live"): FixtureCase => ({
  id: variant === "live" ? `composer-${shape}` : `composer-${shape}-${variant === "clipped" ? "clipped-status" : "ime-safe-cursor"}`,
  covers: variant !== "live" ? [
    `component:packages/tui/src/components/composer/${shape}.ts#${shape}ComposerStyle`,
    "component:packages/tui/src/components/editor.ts#Editor",
  ] : [
    ...(shape === dynamicStyle ? [] : [`composer:${shape}`, `component:packages/tui/src/components/composer/${shape}.ts#${shape}ComposerStyle`]),
    "component:packages/tui/src/status-line/component.ts#StatusLineComponent",
    "component:packages/tui/src/overlays/composer-shape-preview.ts#ComposerShapePreview",
    "component:packages/tui/src/components/editor.ts#Editor",
    "property:TspTextProps.text",
    ...(["band", "box", "field", "rail", dynamicStyle].includes(shape) ? ["property:TspSpan.t"] : []),
    ...(["claude", "borderless", "rule"].includes(shape) ? ["property:TspEditorChromeRow.prefix"] : []),
  ],
  behaviors: shape === dynamicStyle ? ["composer:parity-dynamic"] : [],
  async create(context) {
    const registry = await context.load<typeof import("@omp-source/packages/tui/src/overlays/composer-shape-registry")>("packages/tui/src/overlays/composer-shape-registry.ts");
    const { getComposerStyle } = await context.load<typeof import("@omp-source/packages/tui/src/components/composer/registry")>("packages/tui/src/components/composer/registry.ts");
    const gallery = await context.load<typeof import("@omp-source/packages/coding-agent/src/cli/gallery-fixtures/composer")>("packages/coding-agent/src/cli/gallery-fixtures/composer.ts");
    const { ComposerShapePreview } = await context.load<typeof import("@omp-source/packages/tui/src/overlays/composer-shape-preview")>("packages/tui/src/overlays/composer-shape-preview.ts");
    const { CustomEditor } = await context.load<typeof import("@omp-source/packages/tui/src/prompt/custom-editor")>("packages/tui/src/prompt/custom-editor.ts");
    const { getEditorTheme } = await context.load<typeof import("@omp-source/packages/tui/src/theme/theme")>("packages/tui/src/theme/theme.ts");
    const { StatusLineComponent } = await context.load<typeof import("@omp-source/packages/tui/src/status-line/component")>("packages/tui/src/status-line/component.ts");
    const { createGallerySession } = await context.load<typeof import("@omp-source/packages/coding-agent/src/cli/gallery-fixtures/preview-session")>("packages/coding-agent/src/cli/gallery-fixtures/preview-session.ts");
    const { statusLineHost } = await context.load<typeof import("@omp-source/packages/coding-agent/src/modes/status-line-host")>("packages/coding-agent/src/modes/status-line-host.ts");
    const { col, text } = await context.load<typeof import("@omp-source/packages/tui/src/native/describe")>("packages/tui/src/native/describe.ts");
    assertRegistry(registry.COMPOSER_SHAPE_VALUES, styles, "composer");
    const uninstall = shape === dynamicStyle ? registry.installExtensionComposerShape({
      label: "Parity dynamic composer", description: "Runtime registered composer style",
      style: { ...getComposerStyle("box"), id: dynamicStyle, renderTop: chrome => chrome.borderColor("Parity dynamic composer") },
    }) : undefined;
    if (!gallery.getComposerGalleryInventory().includes(shape) || !gallery.getComposerGalleryEntries().some(entry => entry.id === shape)) {
      uninstall?.();
      throw new Error(`Missing registered gallery composer ${shape}`);
    }
    if (getComposerStyle(shape).id !== shape) {
      uninstall?.();
      throw new Error(`Composer ${shape} silently fell back to another style`);
    }
    const hostMode = probe.requireInteractiveContext();
    const { settings: hostSettings } = await context.load<typeof import("@omp-source/packages/coding-agent/src/config/settings")>("packages/coding-agent/src/config/settings.ts");
    const { cfgComposerShape } = await context.load<typeof import("@omp-source/packages/coding-agent/src/modes/settings")>("packages/coding-agent/src/modes/settings.ts");
    if (shape !== dynamicStyle) {
      cfgComposerShape.override(hostSettings, shape);
      if (cfgComposerShape.get(hostSettings) !== shape) throw new Error(`Host composer.shape stayed ${cfgComposerShape.get(hostSettings)} after override to ${shape}`);
      hostMode.syncComposerShape();
      if (hostMode.editor.getBorderStyle() !== shape) throw new Error(`Host composer stayed ${hostMode.editor.getBorderStyle()} after composer.shape ${shape}`);
    }
    const editor = new CustomEditor(getEditorTheme());
    editor.focused = true;
    editor.setBorderStyle(shape);
    const { cfgTuiImeSafeCursor } = await context.load<typeof import("@omp-source/packages/coding-agent/src/modes/settings")>("packages/coding-agent/src/modes/settings.ts");
    if (variant === "ime-safe") {
      cfgTuiImeSafeCursor.override(hostSettings, true);
      editor.setUseTerminalCursor(true);
      editor.setImeSafeCursorLayout(cfgTuiImeSafeCursor.get(hostSettings));
    }
    editor.setText("Parity composer draft");
    const status = new StatusLineComponent(createGallerySession(), statusLineHost);
    status.updateSettings({
      preset: "custom", leftSegments: ["pi", "model", "mode"], rightSegments: ["session_name", "context_pct"],
      separator: "powerline-thin", sessionAccent: false, contextLine: "annotated",
    });
    status.setPlanModeStatus({ enabled: true, paused: false });
    status.attachToEditor(editor, getComposerStyle(shape));
    if (variant === "clipped") {
      editor.setTopBorderProvider(undefined);
      editor.setTopBorder({ content: clippedStatus, width: clippedStatus.length });
    }
    editor.onChange = value => { context.record("composer-change", value); context.tui.requestRender(); };
    let submitted = "";
    editor.onSubmit = value => { submitted = value; context.record("composer-submit", value); context.tui.requestRender(); };
    const preview = new ComposerShapePreview(shape, { status, requestRender: () => context.tui.requestRender() });
    return controlledGallery(context, {
      render(width) { return [...preview.render(width), ...editor.render(width), submitted]; },
      describe() { return col([preview, editor, text(submitted, { role: "parity.composer.submitted" })]); },
      handleInput(data) { editor.handleInput(data); },
      handleNativeEvent(event) { editor.handleNativeEvent(event); },
      invalidate() { editor.invalidate(); },
      dispose() { status.dispose(); uninstall?.(); if (variant === "ime-safe") cfgTuiImeSafeCursor.override(hostSettings, undefined as unknown as boolean); if (shape !== dynamicStyle) { cfgComposerShape.override(hostSettings, undefined as unknown as string); hostMode.syncComposerShape(); } },
    }, {});
  },
  checkpoints: variant === "ime-safe" ? [{ name: "ime-safe cursor row", comparisons: [
    { name: "live draft", terminal: { type: "screen", pattern: "(Parity composer draft)", group: 1 }, browser: { type: "dom", selector: liveInput, read: "value" }, expected: "Parity composer draft" },
  ] }] : variant === "clipped" ? [{ name: "clipped status chip", comparisons: [
    {
      name: "clipped chip keeps its leading text inside the top border",
      terminal: { type: "screen", pattern: "(Clipped status chip)", group: 1 },
      browser: { type: "dom", selector: `${chromeRoot} [data-omp-composer-chrome-part="top"]`, read: "text", pattern: "(Clipped status chip)", group: 1 },
      expected: "Clipped status chip",
    },
    {
      name: "clipped chip ends in an ellipsis",
      terminal: { type: "screen", pattern: "Clipped status chip[^\\n]*?(…)", group: 1 },
      browser: { type: "dom", selector: `${chromeRoot} [data-omp-composer-chrome-part="top"]`, read: "text", pattern: "Clipped status chip[^\\n]*?(…)", group: 1 },
      expected: "…",
    },
  ] }] : [
    { name: "production preview and live style", comparisons: [
      { name: "live draft", terminal: { type: "screen", pattern: "(Parity composer draft)", group: 1 }, browser: { type: "dom", selector: liveInput, read: "value" }, expected: "Parity composer draft" },
      contentComparison("real preview prompt", "(Ask anything, edit files, run tools)", '[data-omp-role="omp.preview.composer-shape.drawing"]', "Ask anything, edit files, run tools"),
      ...liveChromeComparisons(shape),
    ] },
    ...(shape === dynamicStyle ? [{ name: "registered renderer rather than fallback", comparisons: [{
      ...contentComparison("dynamic chrome", "(Parity dynamic composer)", '[data-omp-role="omp.preview.composer-shape"] span:text-is("Parity dynamic composer"):not([data-omp-role="omp.preview.composer-shape.drawing"] *)', "Parity dynamic composer"),
      proves: [{ entry: "property:TspSpan.t", effect: "literal" as const, value: "Parity dynamic composer" }],
    }] }] : []),
    { name: "edit live composer", terminalInput: "\u0005!", browserActions: [{ type: "press", selector: liveInput, key: "End" }, { type: "press", selector: liveInput, key: "!" }], comparisons: [
      { name: "edited draft", terminal: { type: "screen", pattern: "(Parity composer draft!)", group: 1 }, browser: { type: "dom", selector: liveInput, read: "value" }, expected: "Parity composer draft!" },
      callbackValue("real editor mutation", "composer-change", "Parity composer draft!"),
    ] },
    { name: "submit through editor", terminalInput: "\r", browserActions: [{ type: "press", selector: liveInput, key: "Enter" }], comparisons: [
      callbackValue("real editor submit", "composer-submit", "Parity composer draft!"),
      callbackValue("draft cleared by submit", "composer-change", ""),
      { name: "submitted text rendered", terminal: { type: "screen", pattern: "(Parity composer draft!)", group: 1 },
        browser: { type: "dom", selector: '[data-omp-role="parity.composer.submitted"]', read: "text" }, expected: "Parity composer draft!",
        proves: [{ entry: "property:TspTextProps.text", effect: "literal", value: "Parity composer draft!" }] },
    ] },
    { name: "multiline production composer", terminalInput: "\u001b[200~Parity first\nParity middle\nParity last\u001b[201~",
      browserActions: [{ type: "fill", selector: liveInput, value: "Parity first\nParity middle\nParity last" }],
      comparisons: [
        ...["Parity first", "Parity middle", "Parity last"].map(value => ({
          name: `real multiline row ${value}`,
          terminal: { type: "screen" as const, pattern: `(${value})`, group: 1 },
          browser: { type: "dom" as const, selector: liveInput, read: "value" as const, pattern: `(${value})`, group: 1 },
          expected: value,
        })),
        callbackValue("real multiline editor mutation", "composer-change", "Parity first\nParity middle\nParity last"),
        ...liveChromeComparisons(shape, true),
      ] },
  ],
});

export const fixtureCases: FixtureCase[] = [
  ...[...styles, dynamicStyle].map(shape => composerCase(shape)),
  ...["box", "band"].map(shape => composerCase(shape, "clipped")),
  composerCase("box", "ime-safe"),
];

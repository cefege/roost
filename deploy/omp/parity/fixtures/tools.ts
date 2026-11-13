import type { AgentTool } from "@omp-source/packages/agent/src/types";
import type { GalleryFixture, GalleryResult } from "@omp-source/packages/coding-agent/src/cli/gallery-fixtures/types";
import type { ToolResultFrame } from "@omp-source/packages/tui/src/chat/tool-execution";
import type { VibeOp } from "@omp-source/packages/tui/src/tools/vibe";
import type { NativeUiEvent } from "@omp-source/packages/tui/src/native/node";
import type { FixtureCase, FixtureContext } from "../scenario";
import { callbackValue } from "../scenario";
import { assertRegistry, contentComparison, controlledGallery } from "./gallery-support";
import { callTokens, dedicatedFindFixture, dedicatedVibeFixture, imageFixture, successTokens, toolKeys } from "./tools-data";
import { fixtureCases as customToolCases } from "./tools-custom";
import { fixtureCases as ansiToolCases } from "./tools-ansi";
import { fixtureCases as toolEvidenceCases } from "./tools-evidence";
import { fixtureCases as agentLaneCases } from "./tools-agent-lane";
import { fixtureCases as propertyCases } from "./tools-props";
import { writeDeviceCases } from "./tools-write-devices";

function resultFrame(result: GalleryResult): ToolResultFrame {
  return {
    content: result.content.map(block => {
      if (block.type === "text" && block.text !== undefined) return { type: "text", text: block.text };
      if (block.type === "image" && block.data && block.mimeType) return { type: "image", data: block.data, mimeType: block.mimeType };
      throw new Error(`Unsupported gallery content block ${block.type}`);
    }),
    details: result.details,
    isError: result.isError,
  };
}

async function mountTool(context: FixtureContext, key: typeof toolKeys[number], image = false) {
  const { ToolExecutionComponent } = await context.load<typeof import("@omp-source/packages/tui/src/chat/tool-execution")>("packages/tui/src/chat/tool-execution.ts");
  const { toolRenderers } = await context.load<typeof import("@omp-source/packages/tui/src/tools/index")>("packages/tui/src/tools/index.ts");
  const { galleryFixtures } = await context.load<typeof import("@omp-source/packages/coding-agent/src/cli/gallery-fixtures/index")>("packages/coding-agent/src/cli/gallery-fixtures/index.ts");
  const { GALLERY_STATES } = await context.load<typeof import("@omp-source/packages/coding-agent/src/cli/gallery-cli")>("packages/coding-agent/src/cli/gallery-cli.ts");
  assertRegistry(GALLERY_STATES, ["streaming", "progress", "success", "error"], "gallery lifecycle");
  assertRegistry(Object.keys(toolRenderers), toolKeys, "tool");
  let fixture: GalleryFixture | undefined;
  if (image) fixture = imageFixture;
  else if (key.startsWith("vibe_")) fixture = dedicatedVibeFixture(key.slice(5) as VibeOp);
  else if (key === "find") fixture = dedicatedFindFixture();
  else fixture = galleryFixtures[key];
  if (!fixture || fixture.renderState) throw new Error(`Missing actual ToolExecutionComponent fixture: ${key}`);
  const renderer = toolRenderers[key];
  if (!renderer?.renderCall || !renderer.describeCall) throw new Error(`Missing production renderer: ${key}`);
  const customTool = fixture.customRendered || fixture.label || fixture.editMode ? {
    name: key, label: fixture.label ?? key, mode: fixture.editMode,
    ...(fixture.customRendered ? {
      renderCall: renderer.renderCall, renderResult: renderer.renderResult,
      describeCall: renderer.describeCall, describeResult: renderer.describeResult,
      mergeCallAndResult: renderer.mergeCallAndResult, inline: renderer.inline,
    } : {}),
  } as unknown as AgentTool : undefined;
  class FixtureTool extends ToolExecutionComponent {
    override handleNativeEvent(event: NativeUiEvent): void {
      super.handleNativeEvent(event);
      if (event.type === "toggle" && event.key === "") context.record("tool-expanded", !event.collapsed);
    }
  }
  const component = new FixtureTool(key, fixture.streamingArgs ?? fixture.args,
    { showImages: image, useBuiltInRenderer: !fixture.customRendered }, customTool, context.tui, context.workspace);
  component.setExpanded(true);
  await component.whenPreviewSettled(50);
  const recordState = (state: string) => {
    const described = component.describe({ cols: 100, dark: true, reduceMotion: true, supports: () => true, feature: () => false });
    const status = described.k === "tool" || described.k === "card" ? described.p?.status : undefined;
    context.record("tool-state", { key, state, status });
    context.tui.requestRender();
  };
  const source = fixture;
  recordState("streaming");
  const applyResult = (result: GalleryResult, partial: boolean, state: string) => {
    component.updateResult(resultFrame(result), partial);
    if (image) context.record("tool-images", component.describe({ cols: 100, dark: true, reduceMotion: true, supports: () => true, feature: () => false }).c?.filter(child => "k" in child && child.k === "image").length ?? 0);
    recordState(state);
  };
  const mounted = await controlledGallery(context, component, {
    progress() { component.updateArgs(source.args); component.setArgsComplete(); component.setExecutionStarted(); recordState("progress"); },
    live() { applyResult(source.result, true, "live"); },
    success() { applyResult(source.result, false, "success"); },
    reject() {
      const message = `Parity ${key} failure`;
      applyResult({ content: [{ type: "text", text: message }], isError: true,
        ...(key === "resolve" || key === "reject" ? { details: { action: key === "reject" ? "reject" : "apply", reason: message } } : {}),
      }, false, "error");
    },
    expand() { component.handleNativeEvent({ type: "toggle", key: "", collapsed: false }); },
    collapse() { component.handleNativeEvent({ type: "toggle", key: "", collapsed: true }); },
    finish() { component.stopAnimation(); context.done(); },
  });
  const dispose = mounted.dispose;
  mounted.dispose = () => { component.stopAnimation(); dispose?.(); };
  return mounted;
}

function stateComparison(key: string, state: string, status: string) {
  return callbackValue("component lifecycle", "tool-state", { key, state, status });
}

const streamingTokens: Partial<Record<typeof toolKeys[number], string>> = { recall: "bun vs node", web_search: "bun vs node" };

const rendererComponents: Partial<Record<typeof toolKeys[number], string[]>> = {
  ast_grep: ["ast-grep.ts#astGrepToolRenderer.renderResult.matchLines"],
  find: ["find.ts#findToolRenderer.renderResult.hitLines"],
  grep: ["grep.ts#grepToolRenderer.renderResult.matchLines"],
  eval: ["eval.ts#evalToolRenderer.renderCall", "eval.ts#evalToolRenderer.renderResult@1057:37042", "eval.ts#evalToolRenderer.renderResult@1213:41963", "json-tree.ts#renderJsonTreeLines.tree"],
  read: ["read.ts#readToolRenderer.renderResult"],
  resolve: ["resolve.ts#resolveRenderer.renderResult"],
  vibe_list: ["vibe.ts#linesComponent"],
  web_search: ["web-search.ts#renderSearchResult.sourceTree"],
  wait: ["wait.ts#jobsRenderResult", "wait.ts#jobsRenderResult.render.agentLines", "wait.ts#jobsRenderResult.render.itemLines"],
  think: ["think.ts#thinkToolRenderer"],
};

export const fixtureCases: FixtureCase[] = toolKeys.map(key => {
  const selector = `[data-omp-role="omp.tool.${key}"]`;
  const token = callTokens[key];
  const pattern = `(${token.replace(/[.*+?^${}()|[\]\\]/g, "\\$&")})`;
  const streamingToken = streamingTokens[key] ?? token;
  const streamingPattern = `(${streamingToken.replace(/[.*+?^${}()|[\]\\]/g, "\\$&")})`;
  const successToken = successTokens[key];
  const successPattern = `(${successToken.replace(/[.*+?^${}()|[\]\\]/g, "\\$&")})`;
  const title = key === "eval" ? "load config" : key === "read" ? "Read" : key === "edit" ? "Edit" : undefined;
  return {
    id: `tool-${key}`,
    covers: [`tool:${key}`, "kind:tool", "component:packages/tui/src/chat/tool-execution.ts#ToolExecutionComponent",
      ...(key === "eval" ? ["kind:code", "kind:json"] : key === "bash" ? ["kind:ansi"] : key === "edit" ? ["kind:diff"] : []),
      ...(title ? ["property:TspToolProps.title"] : []),
      ...(key === "think" || key === "write" ? [`component:packages/tui/src/tools/${key}.ts#${key}ToolRenderer`] : []),
      ...(rendererComponents[key] ?? []).map(entry => `component:packages/tui/src/tools/${entry}`),
      ...(key === "vibe_list" ? ["kind:badge", "property:TspBadgeProps.text"] : []),
    ],
    create: context => mountTool(context, key),
    checkpoints: [
      { name: "streaming arguments", comparisons: [contentComparison("partial call content", streamingPattern, selector, streamingToken), stateComparison(key, "streaming", "pending")] },
      ...(title ? [{ name: "source-owned tool verb", comparisons: [{
        ...contentComparison("actual tool verb", `(${title})`, `${selector} strong:text-is("${title}")`, title),
        proves: [{ entry: "property:TspToolProps.title", effect: "literal" as const, value: title }],
      }] }] : []),
      ...(key === "eval" ? [{ name: "production code input", comparisons: [contentComparison("source code", "(import json)", selector, "import json")] }] : []),
      { name: "execution started", terminalInput: "p", browserActions: [{ type: "click", selector: 'button:text-is("progress")' }], comparisons: [contentComparison("running call content", pattern, selector, token), stateComparison(key, "progress", "running")] },
      ...(key === "think" ? [] : [{ name: "streaming tool output", terminalInput: "l", browserActions: [{ type: "click" as const, selector: 'button:text-is("live")' }], comparisons: [stateComparison(key, "live", "running"), contentComparison("live result content", successPattern, selector, successToken)] }]),
      { name: "successful result", terminalInput: "s", browserActions: [{ type: "click", selector: 'button:text-is("success")' }], comparisons: [stateComparison(key, "success", "done"),
        contentComparison("settled production rendering", successPattern, selector, successToken, [{ entry: `tool:${key}`, effect: "literal", value: successToken }])] },
      ...(key === "eval" ? [{ name: "production JSON view", comparisons: [contentComparison("structured JSON key", "(@ai-sdk/anthropic)", selector, "@ai-sdk/anthropic")] }] : []),
      ...(key === "edit" ? [{ name: "production diff", comparisons: [contentComparison("changed source", "(const limit = args\\.limit \\?\\? 4000;)", selector, "const limit = args.limit ?? 4000;")] }] : []),
      ...(key === "vibe_list" ? [{ name: "production worker badge", comparisons: [{
        ...contentComparison("worker CLI badge", "(fast)", `${selector} [data-omp-node]:not(:has([data-omp-node])):has(> span:text-is("fast"))`, "fast"),
        proves: [{ entry: "property:TspBadgeProps.text", effect: "literal" as const, value: "fast" }],
      }] }] : []),
      ...(["bash", "eval", "read", "edit", "apply_patch", "write", "grep", "ast_grep", "find"].includes(key) ? [
        { name: "collapsed result", terminalInput: "c", browserActions: [{ type: "click" as const, selector: `${selector} button[aria-expanded="true"]` }], comparisons: [callbackValue("collapse applied", "tool-expanded", false), contentComparison("collapsed call remains visible", pattern, selector, token)] },
        { name: "expanded result", terminalInput: "e", browserActions: [{ type: "click" as const, selector: `${selector} button[aria-expanded="false"]` }], comparisons: [callbackValue("expand applied", "tool-expanded", true), contentComparison("expanded result content", successPattern, selector, successToken)] },
      ] : []),
      ...(key === "think" ? [] : [{ name: "failed result", terminalInput: "r", browserActions: [{ type: "click" as const, selector: 'button:text-is("reject")' }], comparisons: [contentComparison("failure body", `(Parity ${key} failure)`, selector, `Parity ${key} failure`), stateComparison(key, "error", "error")] }]),
    ],
  };
});

fixtureCases.push({
  id: "tool-read-image", covers: ["kind:image", "property:TspToolProps.target"],
  create: context => mountTool(context, "read", true),
  checkpoints: [
    { name: "image call", comparisons: [{
      ...contentComparison("image path", "(parity-pixel\\.png)", '[data-omp-role="omp.tool.read"]', "parity-pixel.png"),
      proves: [{ entry: "property:TspToolProps.target", effect: "literal", value: "parity-pixel.png" }],
    }] },
    { name: "image result", terminalInput: "ps", browserActions: [{ type: "click", selector: 'button:text-is("progress")' }, { type: "click", selector: 'button:text-is("success")' }], comparisons: [
      { ...contentComparison("rendered image attachment", "(parity-pixel\\.png)", '[data-omp-role="omp.tool.read"]', "parity-pixel.png"),
        proves: [{ entry: "property:TspToolProps.target", effect: "literal", value: "parity-pixel.png" }] },
      { name: "image element", terminal: { type: "effect", channel: "tool-images" }, browser: { type: "dom", selector: '[data-omp-role="omp.tool.read"] img', read: "count" }, expected: 1 },
    ] },
  ],
});

fixtureCases.push(...customToolCases);
fixtureCases.push(...ansiToolCases);
fixtureCases.push(...toolEvidenceCases);
fixtureCases.push(...agentLaneCases);
fixtureCases.push(...propertyCases);
fixtureCases.push(...writeDeviceCases);

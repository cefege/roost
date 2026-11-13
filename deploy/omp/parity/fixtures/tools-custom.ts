import type { AgentTool, AgentToolResult } from "@omp-source/packages/agent/src/types";
import type { ToolDefinition } from "@omp-source/packages/coding-agent/src/extensibility/extensions/types";
import type { NativeUiEvent } from "@omp-source/packages/tui/src/native/node";
import type { RenderResultOptions } from "@omp-source/packages/tui/src/tools/renderer";
import type { Theme } from "@omp-source/packages/tui/src/theme/theme";
import type { Checkpoint, Comparison, FixtureCase, FixtureContext } from "../scenario";
import { callbackValue } from "../scenario";
import { contentComparison, controlledGallery } from "./gallery-support";

type CustomMode = "render-only" | "mcp-render-only" | "merged" | "mixed-call" | "mixed-result";
type Failure = "factory-call" | "factory-result" | "render-call" | "render-result" | "describe-call" | "describe-result";
interface CustomCase { mode: CustomMode; failure?: Failure }
interface Details { payload: string }
interface Arguments { target: string }

const executionComponent = "component:packages/tui/src/chat/tool-execution.ts#ToolExecutionComponent";
const customCases: CustomCase[] = [
  { mode: "render-only" }, { mode: "mcp-render-only" }, { mode: "merged" },
  { mode: "mixed-call" }, { mode: "mixed-result" },
  ...(["factory-call", "factory-result", "render-call", "render-result", "describe-call", "describe-result"] as const)
    .map(failure => ({ mode: "render-only" as const, failure })),
];

function caseName(spec: CustomCase): string {
  return spec.failure ?? spec.mode;
}

function toolName(spec: CustomCase): string {
  return spec.mode === "mcp-render-only" ? "mcp__parity_distinctive" : `parity_custom_${caseName(spec).replaceAll("-", "_")}`;
}

function marker(spec: CustomCase): string {
  return `CUSTOM-${caseName(spec).toUpperCase()}`;
}

function selector(spec: CustomCase): string {
  return `[data-omp-role="omp.tool.${toolName(spec)}"]`;
}

function snapshot(args: Arguments, options: RenderResultOptions, payload?: string) {
  return { target: args.target, expanded: options.expanded, isPartial: options.isPartial, ...(payload ? { payload } : {}) };
}

function callText(spec: CustomCase, args: Arguments, options: RenderResultOptions): string {
  return `${marker(spec)} CALL ${args.target} ${options.expanded ? "expanded" : "compact"}`;
}

function resultText(spec: CustomCase, options: RenderResultOptions, payload: string): string {
  return `${marker(spec)} RESULT ${options.isPartial ? "partial" : "final"} ${options.expanded ? "expanded" : "compact"} ${payload}`;
}

async function mountCustomTool(context: FixtureContext, spec: CustomCase) {
  const { ToolExecutionComponent } = await context.load<typeof import("@omp-source/packages/tui/src/chat/tool-execution")>("packages/tui/src/chat/tool-execution.ts");
  const { Text } = await context.load<typeof import("@omp-source/packages/tui/src/components/text")>("packages/tui/src/components/text.ts");
  const { Container } = await context.load<typeof import("@omp-source/packages/tui/src/tui")>("packages/tui/src/tui.ts");
  const { type: schema } = await context.load<typeof import("@omp-source/packages/ai/src/index")>("packages/ai/src/index.ts");
  const parameters = schema({ target: "string" });
  const pending: Arguments = { target: "pending-target" };
  let args = pending;
  let callFactories = 0;
  const fail = (stage: string): never => {
    context.record(`custom-${stage}-failure`, marker(spec));
    throw new Error(marker(spec));
  };
  const body = (stage: "call" | "result", content: string, options: RenderResultOptions) => {
    const root = new Container();
    const text = new Text(content, 0, 0);
    if (spec.failure === `render-${stage}`) return text.setStyleFn(() => fail(`render-${stage}`));
    root.addChild(text);
    if (options.expanded) root.addChild(new Text(`${marker(spec)} ${stage.toUpperCase()} EXPANSION-DETAIL`, 0, 0));
    return root;
  };
  const frame = (payload: string): AgentToolResult<Details> => ({
    content: [{ type: "text", text: `RAW-NOT-CUSTOM-${payload}` }], details: { payload },
  });
  const definition: ToolDefinition<typeof parameters, Details> & { mergeCallAndResult: boolean } = {
    name: toolName(spec), label: `Distinctive ${caseName(spec)}`, description: "Deterministic custom tool renderer parity fixture",
    parameters, mergeCallAndResult: spec.mode === "merged",
    ...(spec.mode === "mcp-render-only" ? { mcpServerName: "parity", mcpToolName: "distinctive", mcpReadOnlyHint: true } : {}),
    async execute(_toolCallId, params, _signal, onUpdate) {
      context.record("custom-execute", params.target);
      onUpdate?.(frame("live-payload"));
      return frame("final-payload");
    },
    renderCall(params, options) {
      callFactories++;
      context.record("custom-render-call", snapshot(params, options));
      if (spec.failure === "factory-call") return fail("factory-call");
      return body("call", callText(spec, params, options), options);
    },
    renderResult(result, options, _theme, params) {
      const payload = result.details?.payload;
      if (!payload || !params) throw new Error("Missing real custom renderer result or call arguments");
      context.record("custom-render-result", snapshot(params, options, payload));
      if (spec.failure === "factory-result") return fail("factory-result");
      return body("result", resultText(spec, options, payload), options);
    },
  };
  if (spec.mode === "mixed-call" || spec.failure === "describe-call") {
    definition.describeCall = (params, options) => {
      context.record("custom-describe-call", snapshot(params, options));
      if (spec.failure === "describe-call") return fail("describe-call");
      return { head: definition.label, body: [body("call", callText(spec, params, options), options)] };
    };
  }
  if (spec.mode === "mixed-result" || spec.failure === "describe-result") {
    definition.describeResult = (result, options, params) => {
      const payload = result.details?.payload;
      if (!payload || !params) throw new Error("Missing real custom describer result or call arguments");
      context.record("custom-describe-result", snapshot(params, options, payload));
      if (spec.failure === "describe-result") return fail("describe-result");
      return { head: definition.label, body: [body("result", resultText(spec, options, payload), options)] };
    };
  }
  const tool: AgentTool<typeof parameters, Details, Theme> & Pick<typeof definition, "describeCall" | "describeResult" | "mergeCallAndResult"> = {
    ...definition,
    async execute(toolCallId, params, signal, onUpdate) {
      return definition.execute(toolCallId, params, signal, onUpdate, context.extensionContext);
    },
  };
  class CustomExecution extends ToolExecutionComponent {
    override handleNativeEvent(event: NativeUiEvent): void {
      super.handleNativeEvent(event);
      if (event.type === "toggle" && event.key === "") context.record("custom-expanded", !event.collapsed);
    }
  }
  const component = new CustomExecution(tool.name, pending, { useBuiltInRenderer: false, showImages: false }, tool as AgentTool, context.tui, context.workspace);
  component.setExpanded(true);
  const applyResult = (payload: string, partial: boolean) => {
    const callsBeforeResult = callFactories;
    component.updateResult(frame(payload), partial);
    context.record("custom-merged-call-suppressed", callFactories === callsBeforeResult);
    context.record("custom-call-factory-delta", callFactories - callsBeforeResult);
    context.tui.requestRender();
  };
  const mounted = await controlledGallery(context, component, {
    progress() {
      args = { target: "complete-target" };
      component.updateArgs(args);
      component.setArgsComplete();
      component.setExecutionStarted();
    },
    live() { applyResult("live-payload", true); },
    success() { applyResult("final-payload", false); },
    collapse() { component.handleNativeEvent({ type: "toggle", key: "", collapsed: true }); },
    expand() { component.handleNativeEvent({ type: "toggle", key: "", collapsed: false }); },
    finish() { component.stopAnimation(); context.done(); },
  });
  const dispose = mounted.dispose;
  mounted.dispose = () => { component.stopAnimation(); dispose?.(); };
  return mounted;
}

function visible(spec: CustomCase, name: string, value: string): Comparison {
  const pattern = `(${value.replace(/[.*+?^${}()|[\]\\]/g, "\\$&")})`;
  return {
    ...contentComparison(name, pattern, `${selector(spec)} [data-omp-node]:text-is("${value}"):not(:has([data-omp-node])), ${selector(spec)} [data-omp-node]:not(:has([data-omp-node])):has(span:text-is("${value}"))`, value),
    proves: [{ entry: "property:TspSpan.t", effect: "literal", value }],
  };
}

function disclosure(spec: CustomCase, expanded: boolean): Comparison {
  const checkpoint = expanded ? "actual native expansion callback" : "actual native collapse callback";
  const name = expanded ? "expanded native disclosure" : "collapsed native disclosure";
  return {
    name, terminal: { type: "screen", pattern: `(${marker(spec)} RESULT)`, group: 1, read: "visible" },
    browser: { type: "dom", selector: `${selector(spec)} button[aria-expanded="${expanded}"]`, read: "visible" }, expected: true,
    proves: [{ entry: "property:TspToolProps.collapsed", effect: "interaction", value: !expanded,
      assertionGroups: [{ comparisons: [
        { checkpoint, comparison: name },
        { checkpoint, comparison: expanded ? "expand callback value" : "collapse callback value" },
        { checkpoint, comparison: expanded ? "expanded custom detail" : "collapsed renderer content" },
      ] }] }],
  };
}

function rendererCallback(stage: "call" | "result", target: string, expanded: boolean, partial: boolean, payload?: string): Comparison {
  return callbackValue(`${stage} renderer received real lifecycle options`, `custom-render-${stage}`,
    snapshot({ target }, { expanded, isPartial: partial }, payload));
}

function describeCallback(spec: CustomCase, stage: "call" | "result", partial: boolean, payload?: string): Comparison[] {
  if (spec.mode !== `mixed-${stage}`) return [];
  return [{
    name: `${stage} semantic hook runs only on native leg`,
    terminal: { type: "effect", channel: `custom-render-${stage}` },
    browser: { type: "effect", channel: `custom-describe-${stage}` },
    expected: snapshot({ target: stage === "call" ? "pending-target" : "complete-target" }, { expanded: true, isPartial: partial }, payload),
  }];
}

function lifecycleCheckpoints(spec: CustomCase): Checkpoint[] {
  const pendingOptions = { expanded: true, isPartial: true };
  const finalOptions = { expanded: true, isPartial: false };
  return [
    { name: "custom pending renderer", comparisons: [
      visible(spec, "distinctive pending call", callText(spec, { target: "pending-target" }, pendingOptions)),
      rendererCallback("call", "pending-target", true, true), ...describeCallback(spec, "call", true),
    ] },
    { name: "completed arguments reach custom call", terminalInput: "p", browserActions: [{ type: "click", selector: 'button:text-is("progress")' }], comparisons: [
      visible(spec, "updated custom arguments", callText(spec, { target: "complete-target" }, pendingOptions)),
      rendererCallback("call", "complete-target", true, true),
    ] },
    { name: "partial custom result", terminalInput: "l", browserActions: [{ type: "click", selector: 'button:text-is("live")' }], comparisons: [
      visible(spec, "distinctive streaming result", resultText(spec, pendingOptions, "live-payload")),
      rendererCallback("result", "complete-target", true, true, "live-payload"),
      ...describeCallback(spec, "result", true, "live-payload"),
      ...(spec.mode === "merged" ? [callbackValue("merged call factory is suppressed", "custom-merged-call-suppressed", true), {
        name: "merged native body omits call", terminal: { type: "effect" as const, channel: "custom-call-factory-delta" },
        browser: { type: "dom" as const, selector: `${selector(spec)} :text-is("${marker(spec)} CALL complete-target expanded")`, read: "count" as const },
        normalize: "number" as const, expected: 0,
      }] : []),
    ] },
    { name: "final custom result", terminalInput: "s", browserActions: [{ type: "click", selector: 'button:text-is("success")' }], comparisons: [
      visible(spec, "distinctive settled result", resultText(spec, finalOptions, "final-payload")),
      rendererCallback("result", "complete-target", true, false, "final-payload"),
      ...describeCallback(spec, "result", false, "final-payload"),
    ] },
    { name: "actual native collapse callback", terminalInput: "c", browserActions: [{ type: "click", selector: `${selector(spec)} button[aria-expanded="true"]` }], comparisons: [
      disclosure(spec, false),
      callbackValue("collapse callback value", "custom-expanded", false),
      rendererCallback("result", "complete-target", false, false, "final-payload"),
      visible(spec, "collapsed renderer content", resultText(spec, { expanded: false, isPartial: false }, "final-payload")),
    ] },
    { name: "actual native expansion callback", terminalInput: "e", browserActions: [{ type: "click", selector: `${selector(spec)} button[aria-expanded="false"]` }], comparisons: [
      disclosure(spec, true),
      callbackValue("expand callback value", "custom-expanded", true),
      rendererCallback("result", "complete-target", true, false, "final-payload"),
      visible(spec, "expanded custom detail", `${marker(spec)} RESULT EXPANSION-DETAIL`),
    ] },
  ];
}

function failureCheckpoints(spec: CustomCase): Checkpoint[] {
  const failure = spec.failure;
  if (!failure) throw new Error("Missing custom tool failure scenario");
  const stage = failure.endsWith("call") ? "call" : "result";
  const isDescribe = failure.startsWith("describe-");
  const isFactory = failure.startsWith("factory-");
  const prefix = isDescribe ? "Tool description failed" : `Tool ${stage} renderer failed`;
  const renderedText = `${marker(spec)} ${stage.toUpperCase()} ${stage === "call" ? "pending-target expanded" : "final expanded final-payload"}`;
  const comparisons: Comparison[] = isDescribe ? [contentComparison("custom renderer content survives describe failure", `(${renderedText})`, selector(spec), renderedText), {
    name: "native describe failure remains visible while ANSI renderer succeeds",
    terminal: {
      type: "screen",
      pattern: `(${marker(spec)} ${stage.toUpperCase()} ${stage === "call" ? "pending-target expanded" : "final expanded final-payload"})`,
      group: 1,
    },
    browser: { type: "dom", selector: selector(spec), read: "text", pattern: `(${prefix}: Error: ${marker(spec)})`, group: 1 },
    expectedByAdapter: {
      reason: "native-descriptor-failure",
      terminal: `${marker(spec)} ${stage.toUpperCase()} ${stage === "call" ? "pending-target expanded" : "final expanded final-payload"}`,
      browser: `${prefix}: Error: ${marker(spec)}`,
    },
  }, {
    name: "actual describe-hook failure boundary",
    terminal: { type: "effect", channel: `custom-render-${stage}` },
    browser: { type: "effect", channel: `custom-describe-${stage}` },
    expected: snapshot({ target: "pending-target" }, { expanded: true, isPartial: stage === "call" }, stage === "result" ? "final-payload" : undefined),
  }] : [
    visible(spec, "real custom renderer failure diagnostic", `${prefix}: Error: ${marker(spec)}`),
    callbackValue("throwing renderer callback executed", `custom-${isFactory ? "factory" : "render"}-${stage}-failure`, marker(spec)),
    rendererCallback(stage, "pending-target", true, stage === "call", stage === "result" ? "final-payload" : undefined),
  ];
  return [{
    name: `visible ${failure} failure from ToolExecutionComponent`,
    ...(stage === "result" ? { terminalInput: "s", browserActions: [{ type: "click" as const, selector: 'button:text-is("success")' }] } : {}),
    comparisons,
  }];
}

export const fixtureCases: FixtureCase[] = customCases.map(spec => ({
  id: `tool-custom-${caseName(spec)}`,
  covers: [executionComponent, "component:packages/tui/src/chat/tool-execution.ts#SafeToolRendererComponent",
    ...(spec.failure?.startsWith("describe-") ? [] : ["property:TspSpan.t"]),
    ...(!spec.failure ? ["property:TspToolProps.collapsed"] : [])],
  behaviors: [spec.failure ? `tool:custom-${spec.failure}-boundary` : `tool:custom-${spec.mode}-lifecycle`],
  create: context => mountCustomTool(context, spec),
  checkpoints: spec.failure ? failureCheckpoints(spec) : lifecycleCheckpoints(spec),
}));

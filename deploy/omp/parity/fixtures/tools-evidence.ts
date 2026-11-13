import type { NativeUiEvent } from "@omp-source/packages/tui/src/native/node";
import type { ToolResultFrame } from "@omp-source/packages/tui/src/chat/tool-execution";
import type * as ToolExecutionModule from "@omp-source/packages/tui/src/chat/tool-execution";
import type { EvalToolDetails } from "@omp-source/packages/tui/src/tools/eval";
import type { Comparison, FixtureCase, FixtureContext } from "../scenario";
import { callbackValue } from "../scenario";
import { contentComparison, controlledGallery } from "./gallery-support";

const bash = '[data-omp-role="omp.tool.bash"]';
const evaluation = '[data-omp-role="omp.tool.eval"]';
const command = "printf parity-tool-output";
const code = "print(73)";
const evalTitle = "Inspect scalar JSON";
const literal = (name: string, value: string, selector: string, entry: string): Comparison => ({
  ...contentComparison(name, `(${value.replace(/[.*+?^${}()|[\]\\]/g, "\\$&")})`, selector, value),
  proves: [{ entry, effect: "literal", value }],
});
const present = (name: string, label: string, selector: string): Comparison => ({
  name, terminal: { type: "screen", pattern: `(${label})`, group: 1, read: "visible" },
  browser: { type: "dom", selector, read: "visible" }, expected: true,
});

async function mountEvidence(context: FixtureContext, tool: "bash" | "eval") {
  const { ToolExecutionComponent } = await context.load<typeof ToolExecutionModule>("packages/tui/src/chat/tool-execution.ts");
  class ObservableTool extends ToolExecutionComponent {
    override handleNativeEvent(event: NativeUiEvent): void {
      super.handleNativeEvent(event);
      if (event.type === "toggle" && event.key === "") context.record("evidence-expanded", !event.collapsed);
    }
  }
  const component = new ObservableTool(tool, tool === "bash" ? { command } : { language: "python", code, title: evalTitle },
    { showImages: false }, undefined, context.tui, context.workspace);
  component.setExpanded(true);
  const apply = (text: string, partial: boolean, failed = false) => {
    let frame: ToolResultFrame = { content: [{ type: "text", text }], details: { exitCode: failed ? 7 : 0 }, isError: failed };
    if (tool === "eval") {
      const details: EvalToolDetails = { cells: [{ index: 0, language: "python", code, title: evalTitle, output: "73",
        views: [{ text: "73", view: { kind: "json", value: 73 } }], status: partial ? "running" : "complete" }] };
      frame = { content: [{ type: "text", text: "73" }], details, isError: false };
    }
    component.updateResult(frame, partial);
    context.record("evidence-result", { text: tool === "eval" ? "73" : text, partial, failed });
    context.tui.requestRender();
  };
  await component.whenPreviewSettled(50);
  const mounted = await controlledGallery(context, component, {
    progress() { component.setArgsComplete(); component.setExecutionStarted(); },
    live() { apply("PARITY_LIVE_OUTPUT", true); },
    success() { apply("PARITY_FINAL_OUTPUT", false); },
    reject() { apply("PARITY_ERROR_OUTPUT", false, true); },
    collapse() { component.handleNativeEvent({ type: "toggle", key: "", collapsed: true }); },
    expand() { component.handleNativeEvent({ type: "toggle", key: "", collapsed: false }); },
  });
  const dispose = mounted.dispose;
  mounted.dispose = () => { component.stopAnimation(); dispose?.(); };
  return mounted;
}

function shellInput(checkpoint: string): Comparison[] {
  return [
    present("source tool owner", command, bash),
    { ...contentComparison("complete shell command", `(${command.replace(/[.*+?^${}()|[\]\\]/g, "\\$&")})`, `${bash} [data-omp-role="omp.tool.bash.command"]`, command),
      proves: [
        { entry: "property:TspToolProps.command", effect: "literal", value: command, target: { checkpoint, comparison: "source tool owner" } },
      ] },
  ];
}

export const fixtureCases: FixtureCase[] = [{
  id: "tool-bash-source-properties",
  covers: ["tool:bash", "kind:code", "kind:ansi", "component:packages/tui/src/chat/tool-execution.ts#ToolExecutionComponent",
    "property:TspAnsiProps.text", "property:TspToolProps.command",
    "property:TspToolProps.collapsed"],
  behaviors: ["tool:real-source-lifecycle", "tool:partial-output", "tool:collapse-callback"],
  create: context => mountEvidence(context, "bash"),
  checkpoints: [
    { name: "pending source properties", comparisons: shellInput("pending source properties") },
    { name: "running source properties", terminalInput: "p", browserActions: [{ type: "click", selector: 'button:text-is("progress")' }],
      comparisons: shellInput("running source properties") },
    { name: "partial source output", terminalInput: "l", browserActions: [{ type: "click", selector: 'button:text-is("live")' }], comparisons: [
      literal("exact partial ANSI scalar", "PARITY_LIVE_OUTPUT", `${bash} [data-omp-role="omp.tool.bash.output"]`, "property:TspAnsiProps.text"),
      callbackValue("partial result reaches component", "evidence-result", { text: "PARITY_LIVE_OUTPUT", partial: true, failed: false }),
    ] },
    { name: "final source output", terminalInput: "s", browserActions: [{ type: "click", selector: 'button:text-is("success")' }], comparisons: [
      literal("exact final ANSI scalar", "PARITY_FINAL_OUTPUT", `${bash} [data-omp-role="omp.tool.bash.output"]`, "property:TspAnsiProps.text"),
      callbackValue("final result reaches component", "evidence-result", { text: "PARITY_FINAL_OUTPUT", partial: false, failed: false }),
    ] },
    { name: "collapsed source tool", terminalInput: "c", browserActions: [{ type: "click", selector: `${bash} button[aria-expanded="true"]` }], comparisons: [
      { ...present("collapsed tool remains visible", command, `${bash} button[aria-expanded="false"]`),
        proves: [{ entry: "property:TspToolProps.collapsed", effect: "interaction", value: true,
          assertionGroups: [{ comparisons: [
            { checkpoint: "collapsed source tool", comparison: "collapsed tool remains visible" },
            { checkpoint: "collapsed source tool", comparison: "real collapse callback" },
          ] }] }] },
      callbackValue("real collapse callback", "evidence-expanded", false),
    ] },
    { name: "expanded source tool", terminalInput: "e", browserActions: [{ type: "click", selector: `${bash} button[aria-expanded="false"]` }], comparisons: [
      { ...present("expanded tool remains visible", command, `${bash} button[aria-expanded="true"]`),
        proves: [{ entry: "property:TspToolProps.collapsed", effect: "interaction", value: false,
          assertionGroups: [{ comparisons: [
            { checkpoint: "expanded source tool", comparison: "expanded tool remains visible" },
            { checkpoint: "expanded source tool", comparison: "real expand callback" },
            { checkpoint: "expanded source tool", comparison: "expanded final ANSI scalar" },
          ] }] }] },
      literal("expanded final ANSI scalar", "PARITY_FINAL_OUTPUT", `${bash} [data-omp-role="omp.tool.bash.output"]`, "property:TspAnsiProps.text"),
      callbackValue("real expand callback", "evidence-expanded", true),
    ] },
    { name: "failed source output", terminalInput: "r", browserActions: [{ type: "click", selector: 'button:has-text("reject")' }], comparisons: [
      literal("exact error ANSI scalar", "PARITY_ERROR_OUTPUT", `${bash} [data-omp-role="omp.tool.bash.output"]`, "property:TspAnsiProps.text"),
      callbackValue("failed result reaches component", "evidence-result", { text: "PARITY_ERROR_OUTPUT", partial: false, failed: true }),
    ] },
  ],
}, {
  id: "tool-eval-source-properties",
  covers: ["tool:eval", "kind:code", "kind:json", "component:packages/tui/src/chat/tool-execution.ts#ToolExecutionComponent",
    "property:TspToolProps.title", "property:TspJsonProps.value"],
  behaviors: ["tool:source-produced-json-scalar"],
  create: context => mountEvidence(context, "eval"),
  checkpoints: [
    { name: "source eval input", comparisons: [
      contentComparison("complete cell source", `(${code.replace(/[.*+?^${}()|[\]\\]/g, "\\$&")})`, `${evaluation} [data-omp-role="omp.tool.eval.input"]`, code),
      literal("source cell title", evalTitle, `${evaluation} strong:text-is("${evalTitle}")`, "property:TspToolProps.title"),
    ] },
    { name: "source scalar JSON result", terminalInput: "ps", browserActions: [
      { type: "click", selector: 'button:text-is("progress")' }, { type: "click", selector: 'button:text-is("success")' },
    ], comparisons: [
      { name: "actual JSON scalar", terminal: { type: "screen", pattern: "^\\s*(?:[│┃]\\s*)?(?:└ • )?(value: 73)\\s*(?:[│┃]\\s*)?$", flags: "m", group: 1 },
        browser: { type: "dom", selector: `${evaluation} [data-omp-node]:has(> code:text-is("73"))`, read: "text" },
        normalize: "whitespace", expected: "value: 73", proves: [{ entry: "property:TspJsonProps.value", effect: "literal", value: 73 }] },
    ] },
  ],
}];

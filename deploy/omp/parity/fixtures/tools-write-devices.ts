import type * as ToolExecutionModule from "@omp-source/packages/tui/src/chat/tool-execution";
import type { Comparison, FixtureCase, FixtureContext } from "../scenario";
import { controlledGallery } from "./gallery-support";

const WRITE = '[data-omp-role="omp.tool.write"]';
const COMPONENT = "component:packages/tui/src/tools/write.ts#writeToolRenderer";

interface DeviceSpec { action: string; device: string; title: string; body: string }

const deviceSpecs: DeviceSpec[] = [
  { action: "issue", device: "report_issue", title: "Report Tool Issue", body: "PARITY_REPORT_ISSUE_BODY" },
  { action: "resolve", device: "resolve", title: "Resolve", body: "PARITY_RESOLVE_DEVICE_BODY" },
  { action: "deny", device: "reject", title: "Reject", body: "PARITY_REJECT_DEVICE_BODY" },
];

async function mountDeviceWrite(context: FixtureContext) {
  const { ToolExecutionComponent } = await context.load<typeof ToolExecutionModule>("packages/tui/src/chat/tool-execution.ts");
  const component = new ToolExecutionComponent("write", { path: "age" }, { showImages: false, useBuiltInRenderer: true }, undefined, context.tui, context.workspace);
  component.setExpanded(true);
  const actions = Object.fromEntries(deviceSpecs.map(spec => [spec.action, () => {
    component.updateArgs({ path: `xd://${spec.device}`, content: spec.body });
    component.setArgsComplete();
    context.tui.requestRender();
  }]));
  return controlledGallery(context, component, actions);
}

function comparison(name: string, value: string): Comparison {
  const pattern = `(${value.replace(/[.*+?^${}()|[\]\\]/g, "\\$&")})`;
  return {
    name,
    terminal: { type: "screen", pattern, group: 1 },
    browser: { type: "dom", selector: WRITE, read: "text", pattern, group: 1 },
    expected: value, normalize: "whitespace",
  };
}

export const writeDeviceCases: FixtureCase[] = [{
  id: "tool-write-devices",
  covers: [COMPONENT],
  create: mountDeviceWrite,
  checkpoints: deviceSpecs.map(spec => ({
    name: `${spec.device} device write call preview`,
    terminalInput: spec.action[0]!,
    browserActions: [{ type: "click" as const, selector: `button:text-is("${spec.action}")` }],
    comparisons: [comparison(`${spec.device} title`, spec.title), comparison(`${spec.device} first content line`, spec.body)],
  })),
}];

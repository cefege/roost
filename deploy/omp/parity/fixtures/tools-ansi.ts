import type * as ToolExecutionModule from "@omp-source/packages/tui/src/chat/tool-execution";
import type { FixtureCase } from "../scenario";
import { contentComparison, controlledGallery } from "./gallery-support";

const role = '[data-omp-role="omp.tool.bash"]';
const href = "https://example.invalid/parity-ansi";
const output = `${"x".repeat(4100)}ANSI_LONG_LINE_SENTINEL\n\u001b[38;2;12;34;56m\u001b[48;2;78;90;123mANSI_TRUECOLOR\u001b[0m\n\u001b[1;3;4mANSI_STYLES\u001b[0m\n\u001b]8;;${href}\u0007ANSI_LINK_ONE\nANSI_LINK_TWO\u001b]8;;\u0007\nCR_OLD_SUFFIX\rCR_NEW\nBS_OLD\b\b\bNEW\nWIDE_界\bX\nCOMBINING_e\u0301\bX\nANSI_FINAL_SENTINEL`;

export const fixtureCases: FixtureCase[] = [{
  id: "tool-bash-ansi-boundaries",
  covers: ["tool:bash", "kind:ansi", "component:packages/tui/src/chat/tool-execution.ts#ToolExecutionComponent",
    "property:TspAnsiProps.text"],
  behaviors: ["ansi:carriage-return", "ansi:backspace", "ansi:wide-cells", "ansi:combining-clusters", "ansi:untruncated-output"],
  async create(context) {
    const { ToolExecutionComponent } = await context.load<typeof ToolExecutionModule>("packages/tui/src/chat/tool-execution.ts");
    const component = new ToolExecutionComponent("bash", { command: "parity-ansi-probe" }, { showImages: false }, undefined, context.tui, context.workspace);
    component.setExpanded(true);
    component.setArgsComplete();
    component.setExecutionStarted();
    component.updateResult({ content: [{ type: "text", text: output }], details: { exitCode: 0 }, isError: false }, false);
    await component.whenPreviewSettled(50);
    const mounted = await controlledGallery(context, component, {});
    const dispose = mounted.dispose;
    mounted.dispose = () => { component.stopAnimation(); dispose?.(); };
    return mounted;
  },
  checkpoints: [{
    name: "Untruncated ANSI output keeps its text",
    comparisons: [
      contentComparison("long line suffix beyond 4000 characters", "(ANSI_LONG_LINE_SENTINEL)", role, "ANSI_LONG_LINE_SENTINEL"),
      contentComparison("last output line", "(ANSI_FINAL_SENTINEL)", role, "ANSI_FINAL_SENTINEL"),
      contentComparison("carriage return is dropped, not applied as an overwrite", "(CR_OLD_SUFFIXCR_NEW)", role, "CR_OLD_SUFFIXCR_NEW"),
      contentComparison("backspace is dropped, not applied as an overwrite", "(BS_OLDNEW)", role, "BS_OLDNEW"),
      contentComparison("wide glyph and the following byte keep their text", "(WIDE_界X)", role, "WIDE_界X"),
      contentComparison("combining cluster keeps its mark", "(COMBINING_e\u0301)X", role, "COMBINING_e\u0301"),
    ],
  }],
}];

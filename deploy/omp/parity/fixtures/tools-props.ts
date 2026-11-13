import type * as BashExecution from "@omp-source/packages/tui/src/chat/bash-execution";
import type * as ToolExecutionModule from "@omp-source/packages/tui/src/chat/tool-execution";
import type { AgentTool } from "@omp-source/packages/agent/src/types";
import type { GalleryFixture } from "@omp-source/packages/coding-agent/src/cli/gallery-fixtures/types";
import type * as Wait from "@omp-source/packages/tui/src/tools/wait";
import type * as Interactive from "@omp-source/packages/tui/src/tools/bash-interactive";
import type * as TerminalOutput from "@omp-source/packages/tui/src/tools/terminal-output";
import type { Comparison, FixtureCase, FixtureContext } from "../scenario";
import type * as Ui from "@omp-source/packages/tui/src/index";
import { controlledGallery } from "./gallery-support";

const USER_BASH = '[data-omp-role="omp.bash"]';
const USER_COMMAND = "printf parity-user-command";
const USER_OUTPUT = "PARITY_USER_OUTPUT";
const GENERIC_TOOL = "parity_named_tool";
const TOOL_PROPS = "component:packages/tui/src/chat/tool-execution.ts#ToolExecutionComponent";

type UserBashSettlement = "failed" | "cancelled";

async function mountUserBash(context: FixtureContext, settlement: UserBashSettlement) {
  const { BashExecutionComponent } = await context.load<typeof BashExecution>("packages/tui/src/chat/bash-execution.ts");
  const component = new BashExecutionComponent(USER_COMMAND, context.tui, false);
  component.setExpanded(true);
  const settle = () => {
    component.appendOutput(`${USER_OUTPUT}\n`);
    component.setComplete(settlement === "failed" ? 7 : undefined, settlement === "cancelled", { output: USER_OUTPUT });
    context.record("settled", settlement);
  };
  const mounted = await controlledGallery(context, component, settlement === "failed" ? { fail: settle } : { cancel: settle });
  const dispose = mounted.dispose;
  mounted.dispose = () => { component.dispose(); dispose?.(); };
  return mounted;
}

async function mountGalleryTool(context: FixtureContext, key: string, resultDetails: Record<string, unknown>) {
  const { ToolExecutionComponent } = await context.load<typeof ToolExecutionModule>("packages/tui/src/chat/tool-execution.ts");
  const { toolRenderers } = await context.load<typeof import("@omp-source/packages/tui/src/tools/index")>("packages/tui/src/tools/index.ts");
  const { galleryFixtures } = await context.load<typeof import("@omp-source/packages/coding-agent/src/cli/gallery-fixtures/index")>("packages/coding-agent/src/cli/gallery-fixtures/index.ts");
  const fixture: GalleryFixture | undefined = galleryFixtures[key];
  const renderer = toolRenderers[key];
  if (!fixture || fixture.renderState || !renderer) throw new Error(`Missing actual ToolExecutionComponent fixture: ${key}`);
  const customTool = fixture.customRendered || fixture.label || fixture.editMode ? {
    name: key, label: fixture.label ?? key, mode: fixture.editMode,
    ...(fixture.customRendered ? {
      renderCall: renderer.renderCall, renderResult: renderer.renderResult,
      describeCall: renderer.describeCall, describeResult: renderer.describeResult,
      mergeCallAndResult: renderer.mergeCallAndResult, inline: renderer.inline,
    } : {}),
  } as unknown as AgentTool : undefined;
  const component = new ToolExecutionComponent(key, fixture.args, { showImages: false, useBuiltInRenderer: !fixture.customRendered }, customTool, context.tui, context.workspace);
  component.setExpanded(true);
  await component.whenPreviewSettled(50);
  const mounted = await controlledGallery(context, component, {
    success() {
      component.setArgsComplete(); component.setExecutionStarted();
      component.updateResult({ content: fixture.result.content.map(block => ({ type: "text" as const, text: block.text ?? "" })), details: { ...(fixture.result.details as Record<string, unknown> | undefined), ...resultDetails }, isError: false }, false);
      context.tui.requestRender();
    },
  });
  const dispose = mounted.dispose;
  mounted.dispose = () => { component.stopAnimation(); dispose?.(); };
  return mounted;
}

const runningUserBash: Comparison = {
  name: "user shell command is shown while running",
  terminal: { type: "screen", pattern: "\\$ (printf parity-user-command)", group: 1 },
  browser: { type: "dom", selector: `${USER_BASH} [data-omp-role="omp.tool.bash.command"]`, read: "text", pattern: "(printf parity-user-command)", group: 1 },
  expected: "printf parity-user-command",
  normalize: "whitespace",
};

const failedExit: Comparison = {
  name: "failed user command exit code",
  terminal: { type: "screen", pattern: "exit (\\d+)", group: 1 },
  browser: { type: "dom", selector: `${USER_BASH} span.bg-destructive:text-is("exit 7")`, read: "text", pattern: "exit (\\d+)", group: 1 },
  expected: 7, normalize: "number",
  proves: [{ entry: "property:TspToolProps.exit", effect: "literal", value: 7 }],
};

const cancelledNote: Comparison = {
  name: "cancelled user command marker",
  terminal: { type: "screen", pattern: "(cancelled)", group: 1 },
  browser: { type: "dom", selector: `${USER_BASH} span.bg-muted:text-is("cancelled")`, read: "text", pattern: "(cancelled)", group: 1 },
  expected: "cancelled", normalize: "whitespace",
  proves: [
    { entry: "property:TspToolProps.note", effect: "literal", value: "cancelled" },
    { entry: "property:TspToolProps.status", effect: "literal", value: "cancelled" },
  ],
};

const genericName: Comparison = {
  name: "unregistered tool is titled by its name",
  terminal: { type: "screen", pattern: `(${GENERIC_TOOL})`, group: 1 },
  browser: { type: "dom", selector: `[data-omp-role="omp.tool.${GENERIC_TOOL}"] strong:text-is("${GENERIC_TOOL}")`, read: "text", pattern: `(${GENERIC_TOOL})`, group: 1 },
  expected: GENERIC_TOOL, normalize: "whitespace",
  proves: [{ entry: "property:TspToolProps.name", effect: "literal", value: GENERIC_TOOL }],
};

const WRITE_TOOL = '[data-omp-role="omp.tool.write"]';

const writeMeta: Comparison = {
  name: "write line count shares the target row",
  terminal: { type: "screen", pattern: "(16 lines)", group: 1, relativeTo: { pattern: "parse-sel\\.test\\.ts" }, read: "sameRow" },
  browser: { type: "dom", selector: `${WRITE_TOOL} span.text-xs:text-is("16 lines")`, relativeTo: `${WRITE_TOOL} span.font-mono:has(> span.font-semibold:text-is("parse-sel.test.ts"))`, read: "sameRow" },
  expected: true,
  proves: [{ entry: "property:TspToolProps.meta", effect: "layout", value: ["16 lines"] }],
};

const EDIT_TOOL = '[data-omp-role="omp.tool.edit"]';
const multiFileResults = [
  { path: "packages/alpha-module.ts", firstChangedLine: 3, diff: "@@ -3,1 +3,1 @@\n-const alphaOld = 1;\n+const alphaNew = 2;" },
  { path: "packages/beta-module.ts", firstChangedLine: 5, diff: "@@ -5,1 +5,1 @@\n-const betaOld = 1;\n+const betaNew = 2;" },
];

const multiFileComparisons: Comparison[] = ["alpha-module.ts", "beta-module.ts", "alphaNew = 2", "betaNew = 2"].map(token => ({
  name: `multi-file edit shows ${token}`,
  terminal: { type: "screen", pattern: `(${token.replace(/[.]/g, "\\.")})`, group: 1 },
  browser: { type: "dom", selector: EDIT_TOOL, read: "text", pattern: `(${token.replace(/[.]/g, "\\.")})`, group: 1 },
  expected: token, normalize: "whitespace",
}));

const IRC_BODY = "PARITY_IRC_BODY";
const CONSOLE_OUTPUT = "PARITY_PTY_OUTPUT";
const CONSOLE_COMMAND = "parity-console-command";
const CONSOLE = '[data-omp-role="omp.overlay.console"]';

const ircBody: Comparison = {
  name: "peer message body",
  terminal: { type: "screen", pattern: `(${IRC_BODY})`, group: 1 },
  browser: { type: "dom", selector: '[data-omp-role="omp.irc.incoming"]', read: "text", pattern: `(${IRC_BODY})`, group: 1 },
  expected: IRC_BODY, normalize: "whitespace",
};

const consoleStatus = (name: string, state: string): Comparison => ({
  name,
  terminal: { type: "screen", pattern: `\\[(${state})\\]`, group: 1 },
  browser: { type: "dom", selector: CONSOLE, read: "text", pattern: `\\[(${state})\\]`, group: 1 },
  expected: state, normalize: "whitespace",
});

const consoleOutput: Comparison = {
  name: "interactive console screen",
  terminal: { type: "screen", pattern: `(${CONSOLE_OUTPUT})`, group: 1 },
  browser: { type: "dom", selector: CONSOLE, read: "text", pattern: `(${CONSOLE_OUTPUT})`, group: 1 },
  expected: CONSOLE_OUTPUT, normalize: "whitespace",
};

export const fixtureCases: FixtureCase[] = [
  {
    id: "tool-wait-irc-message-card",
    covers: ["component:packages/tui/src/tools/wait.ts#createIrcMessageCard"],
    async create(context) {
      const { createIrcMessageCard } = await context.load<typeof Wait>("packages/tui/src/tools/wait.ts");
      const card = createIrcMessageCard({ kind: "incoming", from: "parity-peer", body: IRC_BODY }, () => true, context.theme);
      return controlledGallery(context, card as Parameters<typeof controlledGallery>[1], {});
    },
    checkpoints: [{ name: "incoming peer message", comparisons: [ircBody] }],
  },
  {
    id: "tool-bash-interactive-console",
    covers: ["component:packages/tui/src/tools/bash-interactive.ts#BashInteractiveOverlayComponent"],
    async create(context) {
      const { Container } = await context.load<typeof Ui>("packages/tui/src/index.ts");
      const { BashInteractiveOverlayComponent } = await context.load<typeof Interactive>("packages/tui/src/tools/bash-interactive.ts");
      const { loadXtermTerminal } = await context.load<typeof TerminalOutput>("packages/tui/src/tools/terminal-output.ts");
      const overlay = new BashInteractiveOverlayComponent(CONSOLE_COMMAND, context.theme, () => 40, await loadXtermTerminal(), { resize() {} });
      overlay.setHandlers(
        data => { overlay.appendOutput(`ECHO_${data}\r\n`); },
        () => overlay.setComplete({ exitCode: undefined, cancelled: true, timedOut: false }),
        () => {},
      );
      overlay.appendOutput(`${CONSOLE_OUTPUT}\r\n`);
      await overlay.flushOutput();
      const handle = context.tui.showOverlay(overlay, {});
      return Object.assign(new Container(), { dispose: () => handle.hide() });
    },
    checkpoints: [
      { name: "running console", comparisons: [consoleOutput, consoleStatus("running state badge", "running")] },
      { name: "forwarded input", terminalInput: "q", comparisons: [{
        name: "forwarded keystroke echo",
        terminal: { type: "screen", pattern: "(ECHO_q)", group: 1 },
        browser: { type: "dom", selector: CONSOLE, read: "text", pattern: "(ECHO_q)", group: 1 },
        expected: "ECHO_q", normalize: "whitespace",
      }] },
      { name: "force kill", terminalInput: "\u001b[27u", browserActions: [{ type: "click", selector: `${CONSOLE} button:text-is("Force kill")` }], comparisons: [consoleStatus("killed state badge", "killed")] },
    ],
  },
  {
    id: "tool-edit-multi-file",
    covers: ["component:packages/tui/src/tools/edit.ts#renderMultiFileResult"],
    create: context => mountGalleryTool(context, "edit", { perFileResults: multiFileResults, path: undefined, diff: undefined }),
    checkpoints: [{ name: "settled multi-file edit", terminalInput: "s", browserActions: [{ type: "click", selector: 'button:text-is("success")' }], comparisons: multiFileComparisons }],
  },
  {
    id: "tool-prop-write-meta",
    covers: ["kind:tool", TOOL_PROPS, "property:TspToolProps.meta"],
    create: context => mountGalleryTool(context, "write", {}),
    checkpoints: [{ name: "settled write", terminalInput: "s", browserActions: [{ type: "click", selector: 'button:text-is("success")' }], comparisons: [writeMeta] }],
  },
  {
    id: "tool-prop-user-bash-failed",
    covers: ["kind:tool", "property:TspToolProps.exit"],
    create: context => mountUserBash(context, "failed"),
    checkpoints: [
      { name: "running user command", comparisons: [runningUserBash] },
      { name: "failed user command", terminalInput: "f", browserActions: [{ type: "click", selector: 'button:text-is("fail")' }], comparisons: [failedExit] },
    ],
  },
  {
    id: "tool-prop-user-bash-cancelled",
    covers: ["kind:tool", "property:TspToolProps.note", "property:TspToolProps.status"],
    create: context => mountUserBash(context, "cancelled"),
    checkpoints: [
      { name: "running user command", comparisons: [runningUserBash] },
      { name: "cancelled user command", terminalInput: "c", browserActions: [{ type: "click", selector: 'button:text-is("cancel")' }], comparisons: [cancelledNote] },
    ],
  },
  {
    id: "tool-prop-generic-name",
    covers: ["kind:tool", TOOL_PROPS, "property:TspToolProps.name"],
    async create(context) {
      const { ToolExecutionComponent } = await context.load<typeof ToolExecutionModule>("packages/tui/src/chat/tool-execution.ts");
      const component = new ToolExecutionComponent(GENERIC_TOOL, { target: "alpha" }, { showImages: false }, undefined, context.tui, context.workspace);
      component.setExpanded(true);
      component.setArgsComplete(); component.setExecutionStarted();
      component.updateResult({ content: [{ type: "text", text: "PARITY_GENERIC_RESULT" }], details: {}, isError: false }, false);
      const mounted = await controlledGallery(context, component, {});
      const dispose = mounted.dispose;
      mounted.dispose = () => { component.stopAnimation(); dispose?.(); };
      return mounted;
    },
    checkpoints: [{ name: "settled unregistered tool", comparisons: [genericName] }],
  },
];

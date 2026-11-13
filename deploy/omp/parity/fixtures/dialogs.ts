import type * as AskDialog from "@omp-source/packages/tui/src/overlays/ask-dialog";
import type * as ApprovalDialog from "@omp-source/packages/tui/src/overlays/approval-dialog";
import type * as HookEditor from "@omp-source/packages/tui/src/overlays/hook-editor";
import type * as HookInput from "@omp-source/packages/tui/src/overlays/hook-input";
import type * as LoginDialog from "@omp-source/packages/tui/src/overlays/login-dialog";
import type * as OAuthSelector from "@omp-source/packages/tui/src/overlays/oauth-selector";
import { callbackValue, visibleText, type FixtureCase } from "../scenario";
import { fixtureCases as panelCases } from "./panel-dialogs";
import { fixtureCases as quotaCases } from "./quota-dialogs";
import { fixtureCases as reviewCases } from "./review-dialogs";
import { fixtureCases as controllerCases } from "./controller-apps";
import { pickerQueryProof } from "./pickers";

const overlay = "packages/tui/src/overlays/";
const component = (file: string, name: string) => `component:${overlay}${file}.ts#${name}`;
const escape = "\x1b";

export const fixtureCases: FixtureCase[] = [
  ...panelCases,
  ...quotaCases,
  ...reviewCases,
  ...controllerCases,
  ...(["commit", "cancel"] as const).map((mode): FixtureCase => ({
    id: `dialog-hook-input-${mode}`,
    covers: [component("hook-input", "HookInputComponent"), "component:packages/tui/src/components/form.ts#TextFormField", component("hook-input", "KeyLegend")],
    async create(context) {
      const { HookInputComponent } = await context.load<typeof HookInput>(`${overlay}hook-input.ts`);
      return new HookInputComponent("Fixture branch name", "fixture-branch", value => { context.record("value", value); context.done(); }, () => { context.record("cancel", true); context.done(); }, { tui: context.tui });
    },
    checkpoints: [
      { name: "input prompt", comparisons: [visibleText("title", "(Fixture branch name)", '[data-omp-role="omp.hook-input.title"]', "Fixture branch name")] },
      { name: "edit draft", terminalInput: "frame-repair", browserActions: [{ type: "fill", selector: '[data-omp-role="omp.hook-input.field"] input', value: "frame-repair" }], comparisons: [{ name: "draft", terminal: { type: "screen", pattern: "(frame-repair)", group: 1 }, browser: { type: "dom", selector: '[data-omp-role="omp.hook-input.field"] input', read: "value" }, expected: "frame-repair" }] },
      { name: mode, terminalInput: mode === "commit" ? "\r" : "\x1b", browserActions: [{ type: "click", selector: `button:has-text("${mode === "commit" ? "Submit" : "Cancel"}")` }], comparisons: [callbackValue("outcome", mode === "commit" ? "value" : "cancel", mode === "commit" ? "frame-repair" : true)] },
    ],
  })),
  {
    id: "dialog-ask-review",
    covers: [component("ask-dialog", "AskDialogComponent")],
    async create(context) {
      const { AskDialogComponent } = await context.load<typeof AskDialog>(`${overlay}ask-dialog.ts`);
      return new AskDialogComponent([
        { id: "policy", question: "Choose a frame policy", options: [{ label: "Drop them" }, { label: "Park them" }], recommended: 1 },
        { id: "flag", question: "Ship behind a flag?", options: [{ label: "Yes" }, { label: "No" }] },
      ], {
        onSubmit: result => {
          context.record("answers", result.kind === "submit" ? result.results.map(answer => answer.selectedOptions) : result.kind);
          context.done();
        },
        onCancel: () => context.record("cancel", true),
        onPrompt: async () => undefined,
      });
    },
    checkpoints: [
      { name: "question information", comparisons: [visibleText("question", "(Choose a frame policy)", '[data-omp-role="omp.ask.question"]', "Choose a frame policy")] },
      { name: "recommended answer advances", terminalInput: "\r", browserActions: [{ type: "click", selector: 'button:has-text("Next")' }], comparisons: [visibleText("next question", "(Ship behind a flag\\?)", '[data-omp-role="omp.ask.question"]', "Ship behind a flag?")] },
      { name: "select other answer", terminalInput: "\x1b[B\r", browserActions: [{ type: "click", selector: '[data-omp-role="omp.ask.options"] [data-omp-node$="option:1"]' }], comparisons: [visibleText("review", "(Park them)", 'text="Park them"', "Park them")] },
      { name: "commit answers", terminalInput: "\r", browserActions: [{ type: "click", selector: 'button:has-text("Submit")' }], comparisons: [callbackValue("chosen answers", "answers", [["Park them"], ["No"]])] },
    ],
  },
  {
    id: "dialog-ask-cancel",
    covers: [component("ask-dialog", "AskDialogComponent")],
    async create(context) {
      const { AskDialogComponent } = await context.load<typeof AskDialog>(`${overlay}ask-dialog.ts`);
      return new AskDialogComponent([{ id: "policy", question: "Cancel without an answer", options: [{ label: "Keep" }] }], {
        onSubmit: result => context.record("unexpected-answer", result),
        onCancel: () => { context.record("cancel", true); context.done(); },
        onPrompt: async () => undefined,
      });
    },
    checkpoints: [
      { name: "question", comparisons: [visibleText("question", "(Cancel without an answer)", '[data-omp-role="omp.ask.question"]', "Cancel without an answer")] },
      { name: "skip", terminalInput: escape, browserActions: [{ type: "click", selector: 'button:has-text("Skip")' }], comparisons: [callbackValue("cancelled", "cancel", true)] },
    ],
  },
  ...(["approve", "deny", "session"] as const).map((answer): FixtureCase => ({
    id: `dialog-approval-${answer}`,
    covers: [component("approval-dialog", "ApprovalDialogComponent"), "property:TspTextProps.text"],
    behaviors: ["approval:armed-choice", `approval:${answer}`],
    async create(context) {
      const { ApprovalDialogComponent, APPROVE_ARM_MS } = await context.load<typeof ApprovalDialog>(`${overlay}approval-dialog.ts`);
      const complete = (value: string) => { context.record("decision", value); context.done(); };
      const dialog = new ApprovalDialogComponent("Allow tool: bash\nCommand: printf parity", {
        onApprove: () => complete("approve"), onDeny: () => complete("deny"),
      }, { session: { label: "Approve bash for this session", onApprove: () => complete("session") } });
      const armed = Promise.withResolvers<void>();
      setTimeout(armed.resolve, APPROVE_ARM_MS);
      await armed.promise;
      return dialog;
    },
    checkpoints: [
      { name: "tool and command", comparisons: [{ ...visibleText("command", "(Command: printf parity)", '[data-omp-role="omp.approval.details"] [data-omp-node$="0"]', "Command: printf parity"), proves: [{ entry: "property:TspTextProps.text", effect: "literal", value: "Command: printf parity" }] }] },
      { name: "decision", terminalInput: answer === "deny" ? escape : answer === "session" ? "\x1b[B\r" : "\r", browserActions: [{ type: "click", selector: answer === "session" ? '[data-omp-role="omp.approval.choice"]:has-text("Approve bash for this session")' : `button:has-text("${answer === "approve" ? "Approve" : "Deny"}")` }], comparisons: [callbackValue("decision", "decision", answer)] },
    ],
  })),
  ...(["commit", "cancel"] as const).map((outcome): FixtureCase => ({
    id: `dialog-hook-editor-${outcome}`,
    covers: [component("hook-editor", "HookEditorComponent")],
    async create(context) {
      const { HookEditorComponent } = await context.load<typeof HookEditor>(`${overlay}hook-editor.ts`);
      return new HookEditorComponent(context.tui, "Review note", "Original note", value => { context.record("submitted", value); context.done(); }, () => { context.record("cancel", true); context.done(); }, { promptStyle: true });
    },
    checkpoints: [
      { name: "prefilled editor", comparisons: [{ name: "prefill", terminal: { type: "screen", pattern: "(Original note)", group: 1 }, browser: { type: "dom", selector: '[data-omp-role="omp.overlay.hook-editor"] textarea', read: "value" }, expected: "Original note" }] },
      ...(outcome === "commit" ? [{ name: "edit", terminalInput: "\x01\x0bRevised note", browserActions: [{ type: "fill" as const, selector: '[data-omp-role="omp.overlay.hook-editor"] textarea', value: "Revised note" }], comparisons: [{ name: "draft", terminal: { type: "screen" as const, pattern: "(Revised note)", group: 1 }, browser: { type: "dom" as const, selector: '[data-omp-role="omp.overlay.hook-editor"] textarea', read: "value" as const }, expected: "Revised note" }] }] : []),
      { name: outcome, terminalInput: outcome === "commit" ? "\r" : escape, browserActions: [{ type: "click", selector: `button:has-text("${outcome === "commit" ? "Submit" : "Cancel"}")` }], comparisons: [callbackValue("outcome", outcome === "commit" ? "submitted" : "cancel", outcome === "commit" ? "Revised note" : true)] },
    ],
  })),
  ...(["commit", "cancel"] as const).map((outcome): FixtureCase => ({
    id: `dialog-login-manual-${outcome}`,
    covers: [component("login-dialog", "LoginDialogComponent")],
    async create(context) {
      const { LoginDialogComponent } = await context.load<typeof LoginDialog>(`${overlay}login-dialog.ts`);
      const dialog = new LoginDialogComponent(context.tui, "parity-provider", (success, message) => { context.record("complete", { success, message }); context.done(); }, url => context.record("open", url));
      void dialog.showManualInput("Paste fixture authorization code").then(code => { context.record("code", code); context.done(); }, () => undefined);
      return dialog;
    },
    checkpoints: [
      { name: "manual prompt", comparisons: [visibleText("prompt", "(Paste fixture authorization code)", '[data-omp-role="omp.login.label"]', "Paste fixture authorization code")] },
      { name: outcome, terminalInput: outcome === "commit" ? "fixture-code\r" : escape, browserActions: outcome === "commit" ? [{ type: "fill", selector: '[data-omp-role="omp.login.prompt"] input', value: "fixture-code" }, { type: "click", selector: 'button:has-text("Continue")' }] : [{ type: "click", selector: 'button:has-text("Cancel")' }], comparisons: [callbackValue("outcome", outcome === "commit" ? "code" : "complete", outcome === "commit" ? "fixture-code" : { success: false, message: "Login cancelled" })] },
    ],
  })),
  ...(["commit", "cancel"] as const).map((mode): FixtureCase => ({
    id: `dialog-nested-note-${mode}`,
    covers: [component("ask-dialog", "AskDialogComponent"), component("hook-editor", "HookEditorComponent")],
    async create(context) {
      const { AskDialogComponent } = await context.load<typeof AskDialog>(`${overlay}ask-dialog.ts`);
      const { HookEditorComponent } = await context.load<typeof HookEditor>(`${overlay}hook-editor.ts`);
      return new AskDialogComponent([{ id: "policy", question: "Keep the fixture frame?", options: [{ label: "Keep" }] }], {
        onSubmit: result => { context.record("answer", result.kind === "submit" ? result.results.map(answer => ({ options: answer.selectedOptions, note: answer.note ?? null })) : result.kind); context.done(); },
        onCancel: () => { context.record("cancel", true); context.done(); },
        onPrompt: (prompt, prefill) => {
          const answer = Promise.withResolvers<string | undefined>();
          const close = (value: string | undefined) => { mounted.hide(); context.record("nested-close", value === undefined ? "cancel" : "commit"); answer.resolve(value); };
          const editor = new HookEditorComponent(context.tui, prompt.title, prefill, close, () => close(undefined), { promptStyle: true, question: prompt.question });
          const mounted = context.tui.showOverlay(editor, { anchor: "center", width: "60%", maxHeight: "70%", margin: 1 });
          context.tui.setFocus(editor);
          context.tui.requestRender();
          return answer.promise;
        },
      });
    },
    checkpoints: [
      { name: "owning question", comparisons: [visibleText("question", "(Keep the fixture frame\\?)", '[data-omp-role="omp.ask.question"]', "Keep the fixture frame?")] },
      { name: "open nested editor", terminalInput: "n", browserActions: [{ type: "click", selector: 'button:has-text("Note")' }], comparisons: [visibleText("nested question", "(Keep the fixture frame\\?)", '[data-omp-role="omp.overlay.hook-editor"] [data-omp-role="omp.ask.question"]', "Keep the fixture frame?")] },
      { name: "edit nested draft", terminalInput: "Fixture note", browserActions: [{ type: "fill", selector: '[data-omp-role="omp.overlay.hook-editor"] textarea', value: "Fixture note" }], comparisons: [{ name: "draft", terminal: { type: "screen", pattern: "(Fixture note)", group: 1 }, browser: { type: "dom", selector: '[data-omp-role="omp.overlay.hook-editor"] textarea', read: "value" }, expected: "Fixture note" }] },
      { name: "close only nested owner", terminalInput: mode === "commit" ? "\r" : "\x1b", browserActions: [{ type: "click", selector: `[data-omp-role="omp.overlay.hook-editor"] button:has-text("${mode === "commit" ? "Submit" : "Cancel"}")` }], comparisons: [callbackValue("nested outcome", "nested-close", mode), visibleText("parent remains", "(Keep the fixture frame\\?)", '[data-omp-role="omp.ask.question"]', "Keep the fixture frame?")] },
      { name: "commit parent answer", terminalInput: "\r", browserActions: [{ type: "click", selector: 'button:has-text("Submit")' }], comparisons: [callbackValue("parent answer", "answer", [{ options: ["Keep"], note: mode === "commit" ? "Fixture note" : null }])] },
    ],
  })),
  ...(["select", "cancel"] as const).map((mode): FixtureCase => ({
    id: `dialog-oauth-login-${mode}`,
    covers: [component("oauth-selector", "OAuthSelectorComponent"), "property:TspPickerProps.query"],
    behaviors: ["picker:oauth-provider", `picker:${mode}`],
    async create(context) {
      const { OAuthSelectorComponent } = await context.load<typeof OAuthSelector>(`${overlay}oauth-selector.ts`);
      return new OAuthSelectorComponent("login", { credentials: { has: () => false }, keys: { source: () => undefined } }, id => { context.record("provider", id); context.done(); }, () => { context.record("cancel", true); context.done(); });
    },
    checkpoints: [
      { name: "search actual provider registry", terminalInput: "anthropic", browserActions: [{ type: "fill", selector: 'input[placeholder="Search providers…"]', value: "anthropic" }], comparisons: [pickerQueryProof("anthropic", 'input[placeholder="Search providers…"]')] },
      { name: mode, terminalInput: mode === "select" ? "\r" : "\x1b", browserActions: [{ type: "press", key: mode === "select" ? "Enter" : "Escape", selector: 'input[placeholder="Search providers…"]' }], comparisons: [callbackValue("outcome", mode === "select" ? "provider" : "cancel", mode === "select" ? "anthropic" : true)] },
    ],
  })),
  {
    id: "dialog-oauth-empty-logout-cancel",
    covers: [component("oauth-selector", "OAuthSelectorComponent"), "property:TspPickerProps.empty"],
    behaviors: ["picker:empty-credentials-cancel"],
    async create(context) {
      const { OAuthSelectorComponent } = await context.load<typeof OAuthSelector>(`${overlay}oauth-selector.ts`);
      return new OAuthSelectorComponent("logout", { credentials: { has: () => false }, keys: { source: () => undefined } }, id => context.record("unexpected-provider", id), () => { context.record("cancel", true); context.done(); });
    },
    checkpoints: [
      { name: "empty account source", comparisons: [{ ...visibleText("empty", "(No stored provider credentials to log out)", 'text="No stored provider credentials to log out"', "No stored provider credentials to log out"), proves: [{ entry: "property:TspPickerProps.empty", effect: "literal", value: "No stored provider credentials to log out" }] }] },
      { name: "cancel", terminalInput: escape, browserActions: [{ type: "press", key: "Escape" }], comparisons: [callbackValue("cancelled", "cancel", true)] },
    ],
  },
];

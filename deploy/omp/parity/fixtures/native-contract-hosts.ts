import { writeFileSync } from "node:fs";
import type * as Ui from "@omp-source/packages/tui/src/index";
import type * as Prompt from "@omp-source/packages/tui/src/prompt/custom-editor";
import type * as Hook from "@omp-source/packages/tui/src/overlays/hook-selector";
import type * as ImageModule from "@omp-source/packages/tui/src/components/image";
import type * as Settle from "@omp-source/packages/tui/src/native/settle";
import type * as Advisor from "@omp-source/packages/tui/src/overlays/advisor-config";
import { modelSource } from "./pickers";
import type { FixtureContext } from "../scenario";

export type EditorHostKind = "composer" | "editor" | "input";
export const editorSelector = '[data-omp-node="layer.ov-l"] [data-omp-role="omp.editor"] textarea';
export const imageSelector = 'img[alt*="native-contract.png"]';
export const PNG_BASE64 = "iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAQAAAC1HAwCAAAAC0lEQVR42mP8/x8AAwMCAO+aN5sAAAAASUVORK5CYII=";

export async function createEditorHost(context: FixtureContext, kind: EditorHostKind = "composer", disabled = false) {
  const { Container, Editor, Input, Text, getEditorTheme } = await context.load<typeof Ui>("packages/tui/src/index.ts");
  const { HookSelectorComponent } = await context.load<typeof Hook>("packages/tui/src/overlays/hook-selector.ts");
  const { CustomEditor } = await context.load<typeof Prompt>("packages/tui/src/prompt/custom-editor.ts");
  const root = new Container();
  const editor = kind === "composer" ? new CustomEditor(context.tui, getEditorTheme(), context.keybindings) : kind === "editor" ? new Editor(getEditorTheme()) : new Input();
  if (editor instanceof CustomEditor) {
    editor.setSpellingFeatures({ typoDetection: false, autocorrect: false, autocomplete: "off" });
    editor.composerState = () => ({ running: false });
  }
  const submissions: string[] = [];
  const snapshot = () => {
    const state = { text: editor instanceof Input ? editor.getValue() : editor.getText(), submissions: [...submissions], disabled: "disableSubmit" in editor ? editor.disableSubmit : false, submissionWired: editor.onSubmit !== undefined };
    writeFileSync(`${context.workspace}/native-editor-state.json`, JSON.stringify(state));
    context.record("native-editor-state", state);
    context.tui.requestRender();
  };
  const submitted = (text: string) => { submissions.push(text); snapshot(); };
  editor.onSubmit = submitted;
  if (editor instanceof Editor) editor.onChange = snapshot;
  if ("disableSubmit" in editor) editor.disableSubmit = disabled;
  if (editor instanceof Input) editor.setValue("displaced local draft"); else editor.setText("displaced local draft");
  editor.focused = true;
  const controls = new HookSelectorComponent("Native contract controller actions", ["Lock submission", "Unlock submission", "Disconnect submission", "Reconnect submission", "Recall displaced draft", "Close native fixture"], choice => {
    if (choice === "Close native fixture") { context.done(); return; }
    if (choice === "Lock submission" && "disableSubmit" in editor) editor.disableSubmit = true;
    if (choice === "Unlock submission" && "disableSubmit" in editor) editor.disableSubmit = false;
    if (choice === "Disconnect submission") editor.onSubmit = undefined;
    if (choice === "Reconnect submission") editor.onSubmit = submitted;
    if (choice === "Recall displaced draft") editor.handleInput("\x1b[A");
    snapshot();
  }, () => context.done());
  root.addChild(new Text(`Native-only ${kind} controller conformance`, 0, 0));
  root.addChild(editor);
  if (kind !== "input") root.addChild(controls);
  snapshot();
  return Object.assign(root, { handleInput: (data: string) => editor.handleInput(data) });
}

export async function createBlobHost(context: FixtureContext) {
  const { Container, Text } = await context.load<typeof Ui>("packages/tui/src/index.ts");
  const { settleNative, isNativeSettled } = await context.load<typeof Settle>("packages/tui/src/native/settle.ts");
  const { Image } = await context.load<typeof ImageModule>("packages/tui/src/components/image.ts");
  const { HookSelectorComponent } = await context.load<typeof Hook>("packages/tui/src/overlays/hook-selector.ts");
  const root = new Container();
  const imagePath = `${context.workspace}/native-contract.png`;
  writeFileSync(imagePath, Buffer.from(PNG_BASE64, "base64"));
  const image = new Image(PNG_BASE64, "image/png", { fallbackColor: text => text }, { filename: "native-contract.png", sourcePath: imagePath, maxWidthCells: 4, maxHeightCells: 2 }, { widthPx: 1, heightPx: 1 });
  const controls = new HookSelectorComponent("Native blob owner actions", ["Mount native image", "Remove native image", "Move native image to beginning", "Finalize native image", "Close native fixture"], choice => {
    if (choice === "Close native fixture") { context.done(); return; }
    if (choice === "Mount native image") root.addChild(image);
    if (choice === "Remove native image") root.removeChild(image);
    if (choice === "Move native image to beginning") { root.removeChild(image); root.children.unshift(image); }
    if (choice === "Finalize native image") settleNative(image);
    context.record("native-image-state", { mounted: root.children.includes(image), index: root.children.indexOf(image), settled: isNativeSettled(image), image: image.debugState() });
    context.tui.requestRender();
  }, () => context.done());
  root.addChild(new Text("Native-only blob transport conformance", 0, 0));
  root.addChild(controls);
  return Object.assign(root, { handleInput: (data: string) => controls.handleInput(data) });
}

export async function createDisclosureHost(context: FixtureContext) {
  const { Disclosure, Text } = await context.load<typeof Ui>("packages/tui/src/index.ts");
  let materialized = 0;
  return new Disclosure({
    summary: new Text("Native disclosure summary", 0, 0),
    collapsedBody: new Text("Canonical collapsed preview", 0, 0),
    body: () => {
      context.record("native-disclosure-materialized", ++materialized);
      return new Text("Canonical expanded body", 0, 0);
    },
  });
}

export async function createAdvisorHost(context: FixtureContext) {
  const { AdvisorConfigOverlayComponent } = await context.load<typeof Advisor>("packages/tui/src/overlays/advisor-config.ts");
  const doc: Advisor.WatchdogConfigDoc = { advisors: [{ name: "Native reviewer", enabled: true, instructions: "Keep the native contract owner" }] };
  return new AdvisorConfigOverlayComponent(context.tui, {
    getAvailableModels: () => [], browserSource: modelSource,
    defaultToolNames: new Set(["read", "grep", "glob"]),
    scopedModels: [], availableToolNames: ["read", "grep", "glob"],
  }, "project", doc, {
    loadDoc: async () => structuredClone(doc),
    save: async (scope, saved) => {
      const receipt = { scope, doc: saved };
      writeFileSync(`${context.workspace}/native-advisor.json`, JSON.stringify(receipt));
      context.record("native-advisor-saved", receipt);
    },
    close: () => { context.record("native-advisor-closed", true); context.done(); },
    notify: message => context.record("native-advisor-notice", message),
    requestRender: () => context.tui.requestRender(),
  });
}

import type * as EditorModule from "@omp-source/packages/tui/src/components/editor";
import type * as ThemeModule from "@omp-source/packages/tui/src/theme/tui-adapters";
import type { FixtureCase } from "../scenario";
import { callbackValue } from "../scenario";
import { contentComparison } from "./gallery-support";

const component = "component:packages/tui/src/components/editor.ts#Editor";
const field = '[data-omp-role="omp.field"] textarea';
const drawing = '[data-omp-role="omp.field"] [id$="-mirror"]';
const multiline = 'const face = "😀";\nconst val\nTAIL';
const completed = 'const face = "😀";\nconst value = 42; \nTAIL';

const surface = '[id^="omp-view-"][data-omp-surface]';
const nativeEditor = '[data-omp-node="l.input"] [data-omp-native-editor]';
const ghost = `${drawing} span.absolute.whitespace-pre-wrap`;
/** The Editor drops the space left after a completed word when a newline is inserted at the caret. */
const expanded = 'const face = "😀";\nconst value = 42;\n\nTAIL';
const caretOffset = multiline.indexOf("\nTAIL");
export const fixtureCases: FixtureCase[] = [
  {
    id: "primitive-editor-multiline-caret-completion",
    covers: ["kind:editor", "property:TspEditorProps.ghost", "property:TspEditorProps.cursor", component],
    async create(context) {
      const { Editor } = await context.load<typeof EditorModule>("packages/tui/src/components/editor.ts");
      const { getEditorTheme } = await context.load<typeof ThemeModule>("packages/tui/src/theme/tui-adapters.ts");
      const editor = new Editor(getEditorTheme());
      editor.setText(multiline);
      editor.setMaxHeight(6);
      const cursor = multiline.indexOf("\nTAIL");
      editor.applyHostEdit({ from: 0, to: 0, text: "", cursor, len: multiline.length });
      editor.setTextAssistProvider({
        getWordCompletion(lines, line, col) { return lines[line] === "const val" && col === 9 ? "ue = 42;" : null; },
        wordCompletionFeedback(_lines, _line, _col, suggestion, accepted) { context.record("word-completion", { suggestion, accepted }); },
      });
      editor.onChange = text => context.record("editor-change", text);
      editor.onSubmit = text => {
        context.record("submitted-prompt", text);
        context.record("submitted-buffer", editor.getText());
      };
      return editor;
    },
    checkpoints: [
      { name: "completion appears at nonempty middle-line caret", comparisons: [
        { name: "multiline accessible native value", terminal: { type: "screen", pattern: '(const face = "😀";)[^\\n]*\\n[^\\n]*const value = 42;[^\\n]*\\n[^\\n]*TAIL', group: 1 }, browser: { type: "dom", selector: field, read: "value", pattern: '(const face = "😀";)', group: 1 }, expected: 'const face = "😀";' },
        { name: "inline ghost suffix", terminal: { type: "screen", pattern: "(?<=const val)(ue = 42;)", group: 1 }, browser: { type: "dom", selector: ghost, read: "text" }, expected: "ue = 42;", normalize: "whitespace", proves: [{ entry: "property:TspEditorProps.ghost", effect: "literal", value: "ue = 42;" }] },
        contentComparison("following logical line survives", "(TAIL)", `${drawing}`, "TAIL"),
        { name: "completion ghost stays in the composer area", terminal: { type: "screen", pattern: "ue = 42;", read: "bottomHalf" }, browser: { type: "dom", selector: ghost, read: "bottomHalf", relativeTo: surface }, expected: true, proves: [{ entry: "property:TspEditorProps.cursor", effect: "layout", value: caretOffset }] },
      ] },
      { name: "accept completion without submitting", terminalInput: "\t", browserActions: [{ type: "press", selector: field, key: "Tab" }], comparisons: [
        callbackValue("actual word assistance feedback", "word-completion", { suggestion: "ue = 42;", accepted: true }),
        callbackValue("original Editor resulting buffer", "editor-change", completed),
        { name: "accessible completed value", terminal: { type: "effect", channel: "editor-change" }, browser: { type: "dom", selector: field, read: "value" }, expected: completed },
      ] },
      { name: "modified Enter follows actual editor newline binding", terminalInput: "\x1b[13;2u", browserActions: [{ type: "press", selector: field, key: "Shift+Enter" }], comparisons: [
        callbackValue("native newline effect", "editor-change", expanded),
        { name: "native multiline buffer", terminal: { type: "effect", channel: "editor-change" }, browser: { type: "dom", selector: field, read: "value" }, expected: expanded },
      ] },
      { name: "Enter submits that same expanded multiline draft", terminalInput: "\r", browserActions: [{ type: "press", selector: field, key: "Enter" }], comparisons: [
        callbackValue("actual Editor submit callback", "submitted-prompt", expanded),
        { name: "canonical submit clears browser value", terminal: { type: "effect", channel: "submitted-buffer" }, browser: { type: "dom", selector: field, read: "value" }, expected: "" },
      ] },
    ],
  },
  {
    id: "primitive-editor-diagnostics-and-language",
    covers: ["property:TspEditorProps.decor", component],
    async create(context) {
      const { Editor } = await context.load<typeof EditorModule>("packages/tui/src/components/editor.ts");
      const { getEditorTheme, highlightCode } = await context.load<typeof ThemeModule>("packages/tui/src/theme/tui-adapters.ts");
      const editor = new Editor(getEditorTheme());
      editor.setText('mispelled\nconst total = 42;');
      editor.describeLanguage = () => "typescript";
      editor.decorateText = (text, location) => location.line === 0 ? context.theme.fg("error", `\x1b[4m${text}\x1b[24m`) : highlightCode(text, "typescript", context.theme).join("\n");
      editor.describeDecorations = () => [{ from: 0, to: 9, s: "typo" }];
      return editor;
    },
    checkpoints: [{ name: "canonical decorations and syntax retain source text", comparisons: [
      contentComparison("diagnostic underlined run", "(mispelled)", `${drawing} .underline.decoration-wavy`, "mispelled"),
      contentComparison("syntax keyword", "(const)", `${drawing} [class*="--syntax-keyword"]`, "const"),
      contentComparison("syntax number", "(42)", `${drawing} [class*="--syntax-number"]`, "42"),
      { name: "decorations never mutate accessible buffer", terminal: { type: "screen", pattern: "(mispelled)[^\\n]*\\n[^\\n]*const total = 42;", group: 1 }, browser: { type: "dom", selector: field, read: "value", pattern: "^(mispelled)\\nconst total = 42;$", group: 1 }, expected: "mispelled" },
      { name: "diagnostic run is underlined on both interfaces", terminal: { type: "screen", pattern: "mispelled", read: "underline" }, browser: { type: "dom", selector: `${drawing} .underline.decoration-wavy`, read: "underline" }, expected: true, proves: [{ entry: "property:TspEditorProps.decor", effect: "appearance", value: [{ from: 0, to: 9, s: "typo" }] }] },
    ] }],
  },
  {
    id: "primitive-editor-rich-placeholder",
    covers: ["property:TspEditorProps.placeholder", component],
    async create(context) {
      const { Editor } = await context.load<typeof EditorModule>("packages/tui/src/components/editor.ts");
      const { getEditorTheme } = await context.load<typeof ThemeModule>("packages/tui/src/theme/tui-adapters.ts");
      const editor = new Editor(getEditorTheme());
      editor.placeholder = () => "\x1b[3mWrite a native note\x1b[23m";
      editor.describePlaceholder = () => [{ t: "Write a native note", s: "em" }];
      return editor;
    },
    checkpoints: [{ name: "empty editor rich placeholder", comparisons: [
      contentComparison("italic placeholder", "(Write a native note)", `${drawing} .italic`, "Write a native note"),
      { name: "placeholder is rendered italic on both interfaces", terminal: { type: "screen", pattern: "Write a native note", read: "italic" }, browser: { type: "dom", selector: `${drawing} .italic`, read: "italic" }, expected: true, proves: [{ entry: "property:TspEditorProps.placeholder", effect: "appearance", value: [{ t: "Write a native note", s: "em" }] }] },
    ] }],
  },
  {
    id: "primitive-editor-authoritative-anchor-only",
    covers: ["property:TspEditorProps.anchor", component],
    async create(context) {
      const { Editor } = await context.load<typeof EditorModule>("packages/tui/src/components/editor.ts");
      const { getEditorTheme } = await context.load<typeof ThemeModule>("packages/tui/src/theme/tui-adapters.ts");
      const editor = new Editor(getEditorTheme());
      editor.setVimMode(true);
      editor.setText("A😀BC");
      editor.handleInput("\x1b");
      editor.onVimModeChange = mode => {
        const cursor = editor.getCursor();
        const lines = editor.getText().split("\n");
        const lineStart = lines.slice(0, cursor.line).reduce((offset, line) => offset + line.length + 1, 0);
        context.record("visual-state", { mode, lineStart, cursor: lineStart + cursor.col });
      };
      editor.onAltEnter = () => editor.handleInput(editor.vimMode === "normal" ? "V" : "\x1b");
      return editor;
    },
    checkpoints: [
      { name: "initial normal-mode value", comparisons: [{ name: "Unicode buffer", terminal: { type: "screen", pattern: "(A😀BC)", group: 1 }, browser: { type: "dom", selector: field, read: "value" }, expected: "A😀BC" }] },
      { name: "linewise selection changes anchor without moving text or caret", terminalInput: "\x1b\r", browserActions: [{ type: "press", selector: field, key: "Alt+Enter" }], comparisons: [
        { name: "authoritative line-start anchor", terminal: { type: "effect", channel: "visual-state", pointer: "/lineStart" }, browser: { type: "dom", selector: field, read: "selectionStart" }, expected: 0 },
        { name: "unchanged UTF-16 caret", terminal: { type: "effect", channel: "visual-state", pointer: "/cursor" }, browser: { type: "dom", selector: field, read: "selectionEnd" }, expected: 4 },
        callbackValue("actual visual controller mode", "visual-state", { mode: "visual-line", lineStart: 0, cursor: 4 }),
        { name: "selected line stays in the composer area", terminal: { type: "screen", pattern: "A😀BC", read: "bottomHalf" }, browser: { type: "dom", selector: nativeEditor, read: "bottomHalf", relativeTo: surface }, expected: true, proves: [{ entry: "property:TspEditorProps.anchor", effect: "layout", value: 0 }] },
      ] },
      { name: "remove selection without a text or caret edit", terminalInput: "\x1b\r", browserActions: [{ type: "press", selector: field, key: "Alt+Enter" }], comparisons: [
        { name: "collapsed authoritative caret", terminal: { type: "effect", channel: "visual-state", occurrence: 1, pointer: "/cursor" }, browser: { type: "dom", selector: field, read: "selectionStart" }, expected: 4 },
        { name: "unchanged accessible buffer", terminal: { type: "screen", pattern: "(A😀BC)", group: 1 }, browser: { type: "dom", selector: field, read: "value" }, expected: "A😀BC" },
      ] },
    ],
  },
  {
    id: "primitive-editor-pty-input-owner",
    covers: ["property:TspEditorProps.mode", component],
    async create(context) {
      const { Editor } = await context.load<typeof EditorModule>("packages/tui/src/components/editor.ts");
      const { getEditorTheme } = await context.load<typeof ThemeModule>("packages/tui/src/theme/tui-adapters.ts");
      const editor = new Editor(getEditorTheme());
      editor.setText("Draft preserved during PTY");
      editor.nativeKeyPassthrough = true;
      let received = "";
      const detach = context.tui.addInputListener(data => {
        if (context.tui.getFocused() !== editor) return undefined;
        received += data;
        if (received.endsWith("\r")) {
          context.record("pty-input", received);
          editor.nativeKeyPassthrough = false;
          detach();
          context.tui.requestRender();
        }
        return { consume: true };
      }, { prepend: true });
      editor.onChange = text => context.record("after-pty-edit", text);
      return Object.assign(editor, { dispose: detach });
    },
    checkpoints: [
      { name: "preserved draft while PTY owns input", comparisons: [
        { name: "original draft", terminal: { type: "screen", pattern: "(Draft preserved during PTY)", group: 1 }, browser: { type: "dom", selector: field, read: "value" }, expected: "Draft preserved during PTY" },
        { name: "draft stays in the composer area while PTY owns input", terminal: { type: "screen", pattern: "Draft preserved during PTY", read: "bottomHalf" }, browser: { type: "dom", selector: nativeEditor, read: "bottomHalf", relativeTo: surface }, expected: true, proves: [{ entry: "property:TspEditorProps.mode", effect: "layout", value: "PTY" }] },
      ] },
      { name: "printable control and Enter reach foreground input listener", terminalInput: "y\u000f\r", browserActions: [{ type: "press", selector: field, key: "y" }, { type: "press", selector: field, key: "Control+o" }, { type: "press", selector: field, key: "Enter" }], comparisons: [
        callbackValue("raw PTY bytes", "pty-input", "y\u000f\r"),
        { name: "raw keys never edit the preserved draft", terminal: { type: "screen", pattern: "(Draft preserved during PTY)", group: 1 }, browser: { type: "dom", selector: field, read: "value" }, expected: "Draft preserved during PTY" },
      ] },
      { name: "native editing resumes after owner detaches", terminalInput: "\u0001\u000bResumed draft", browserActions: [{ type: "fill", selector: field, value: "Resumed draft" }], comparisons: [
        { name: "original Editor receives resumed edits", terminal: { type: "screen", pattern: "(Resumed draft)", group: 1 }, browser: { type: "dom", selector: field, read: "value" }, expected: "Resumed draft" },
      ] },
    ],
  },
  {
    id: "primitive-editor-backward-anchor",
    covers: ["property:TspEditorProps.anchor", component],
    async create(context) {
      const { Editor } = await context.load<typeof EditorModule>("packages/tui/src/components/editor.ts");
      const { getEditorTheme } = await context.load<typeof ThemeModule>("packages/tui/src/theme/tui-adapters.ts");
      const editor = new Editor(getEditorTheme());
      editor.setVimMode(true);
      editor.setText("A😀BC");
      editor.handleInput("\x1b");
      editor.handleInput("vh");
      editor.onVimModeChange = mode => {
        const cursor = editor.getCursor();
        const line = editor.getText().split("\n")[cursor.line] ?? "";
        context.record("backward-selection", {
          mode, cursor: cursor.col, lineEnd: line.length,
          direction: cursor.col < line.length ? "backward" : "forward",
        });
      };
      editor.onAltEnter = () => editor.handleInput("V");
      return editor;
    },
    checkpoints: [
      { name: "existing reversed visual selection", comparisons: [{ name: "original Unicode text", terminal: { type: "screen", pattern: "(A😀BC)", group: 1 }, browser: { type: "dom", selector: field, read: "value" }, expected: "A😀BC" }] },
      { name: "changing visual anchor preserves backward caret", terminalInput: "\x1b\r", browserActions: [{ type: "press", selector: field, key: "Alt+Enter" }], comparisons: [
        { name: "backward selection caret", terminal: { type: "effect", channel: "backward-selection", pointer: "/cursor" }, browser: { type: "dom", selector: field, read: "selectionStart" }, expected: 3 },
        { name: "line-end anchor", terminal: { type: "effect", channel: "backward-selection", pointer: "/lineEnd" }, browser: { type: "dom", selector: field, read: "selectionEnd" }, expected: 5 },
        { name: "backward native direction", terminal: { type: "effect", channel: "backward-selection", pointer: "/direction" }, browser: { type: "dom", selector: field, read: "selectionDirection" }, expected: "backward" },
        { name: "reversed selection stays in the composer area", terminal: { type: "screen", pattern: "A😀BC", read: "bottomHalf" }, browser: { type: "dom", selector: nativeEditor, read: "bottomHalf", relativeTo: surface }, expected: true, proves: [{ entry: "property:TspEditorProps.anchor", effect: "layout", value: 5 }] },
      ] },
    ],
  },
  {
    id: "primitive-editor-vim-motions-delete-undo-replace",
    covers: [component],
    async create(context) {
      const { Editor } = await context.load<typeof EditorModule>("packages/tui/src/components/editor.ts");
      const { getEditorTheme } = await context.load<typeof ThemeModule>("packages/tui/src/theme/tui-adapters.ts");
      const editor = new Editor(getEditorTheme());
      editor.setVimMode(true);
      editor.setText("one two");
      editor.handleInput("\x1b");
      editor.onChange = text => context.record("vim-buffer", text);
      return editor;
    },
    checkpoints: [
      { name: "normal mode preserves the initial draft", comparisons: [
        { name: "actual modal editor buffer", terminal: { type: "screen", pattern: "(one two)", group: 1 }, browser: { type: "dom", selector: field, read: "value" }, expected: "one two" },
      ] },
      { name: "normal-mode motion and word deletion are not inserted as text", terminalInput: "0dw", browserActions: [
        { type: "press", selector: field, key: "0" },
        { type: "press", selector: field, key: "d" },
        { type: "press", selector: field, key: "w" },
      ], comparisons: [
        callbackValue("canonical Vim deletion", "vim-buffer", "two"),
        { name: "rendered deleted word", terminal: { type: "screen", pattern: "(two)", group: 1 }, browser: { type: "dom", selector: field, read: "value" }, expected: "two" },
      ] },
      { name: "normal-mode undo uses the same editor history", terminalInput: "u", browserActions: [{ type: "press", selector: field, key: "u" }], comparisons: [
        callbackValue("canonical Vim undo", "vim-buffer", "one two"),
        { name: "restored draft", terminal: { type: "screen", pattern: "(one two)", group: 1 }, browser: { type: "dom", selector: field, read: "value" }, expected: "one two" },
      ] },
      { name: "replace mode overwrites instead of inserting keystrokes", terminalInput: "0RX", browserActions: [
        { type: "press", selector: field, key: "0" },
        { type: "press", selector: field, key: "R" },
        { type: "press", selector: field, key: "X" },
      ], comparisons: [
        callbackValue("canonical Vim replacement", "vim-buffer", "Xne two"),
        { name: "replaced draft", terminal: { type: "screen", pattern: "(Xne two)", group: 1 }, browser: { type: "dom", selector: field, read: "value" }, expected: "Xne two" },
      ] },
      { name: "replace-mode Backspace restores the overwritten character", terminalInput: "\x7f", browserActions: [{ type: "press", selector: field, key: "Backspace" }], comparisons: [
        callbackValue("canonical replace rollback", "vim-buffer", "one two"),
        { name: "original character restored", terminal: { type: "screen", pattern: "(one two)", group: 1 }, browser: { type: "dom", selector: field, read: "value" }, expected: "one two" },
      ] },
    ],
  },
];

import { createHash } from "node:crypto";
import type { NativeAssertion, NativeContract, WireReference } from "../coverage-native";
import type { BrowserAction, FixtureCase } from "../scenario";
import { contextCases } from "./native-contracts-context";
import { focusCases } from "./native-contracts-focus";
import { screenCases } from "./native-contracts-screen";
import { paletteCases } from "./native-contracts-palette";
import { helloCases } from "./native-contracts-hello";
import { galleryCases } from "./native-contracts-kinds";
import { opCases } from "./native-contracts-ops";
import { toggleCases } from "./native-contracts-toggle";
import { eventContract, frameContract, pickerNode } from "./native-contracts-support";
import { createAdvisorHost, createBlobHost, createDisclosureHost, createEditorHost, editorSelector, imageSelector, PNG_BASE64 } from "./native-contract-hosts";

const initialDraft = "displaced local draft";
const plainEditor = '[data-omp-node="layer.ov-l"] [data-omp-role="omp.field"] textarea';
const plainInput = '[data-omp-node="layer.ov-l"] input';
const activateControl = (index: number): BrowserAction => ({ type: "nativeEvent", selector: pickerNode, event: { ev: "activate", item: String(index) } });
const state = (pointer: string, expected: unknown): NativeAssertion => ({ name: `canonical controller ${pointer}`, browser: { type: "file", path: "native-editor-state.json", pointer }, expected });
const value = (expected: string, selector = editorSelector): NativeAssertion => ({ name: "actual HTML draft", browser: { type: "dom", selector, read: "value" }, expected });

function sendContracts(text: string, assertions: NativeAssertion[], selector = editorSelector): NativeContract[] {
  const request: WireReference = { verb: "e", direction: "in", occurrence: "last", match: { "/ev": "send", "/text": text } };
  return [
    eventContract("send", assertions, { "/text": text }, selector),
    { entry: "verb:e", request, assertions },
    { entry: "property:TspEvent.ev", request: { ...request, pointer: "/ev" }, value: "send", assertions },
    { entry: "property:TspEvent.text", request: { ...request, pointer: "/text" }, value: text, assertions },
    { entry: "property:TspEvent.sf", request: { ...request, pointer: "/sf" }, assertions },
    { entry: "property:TspEvent.id", request: { ...request, pointer: "/id" }, assertions },
  ];
}

function editorFrame(assertions: NativeAssertion[]): NativeContract {
  return frameContract("verb:f", assertions);
}

function queryContract(entry: string, pointer?: string, expected?: unknown): NativeContract {
  return {
    entry, witness: "request",
    request: { verb: "q", direction: "out", occurrence: "last", match: { "/q": "blobs", "/ids": [blobId] }, ...(pointer ? { pointer } : {}) },
    response: { verb: "b", direction: "out", occurrence: "last" },
    ...(expected === undefined ? {} : { value: expected }),
    assertions: imageAssertions,
  };
}

function openContract(entry: string, pointer?: string, expected?: unknown): NativeContract {
  return {
    entry,
    request: { verb: "o", direction: "out", occurrence: "last", ...(pointer ? { pointer } : {}) },
    response: { verb: "e", direction: "in", occurrence: "last", match: { "/ev": "ack" } },
    correlate: [{ request: "/id", response: "/sf" }],
    ...(expected === undefined ? {} : { value: expected }),
    assertions: imageAssertions,
  };
}

function editContracts(edit: { from: number; to: number; text: string; cursor: number; len: number; n: string }, assertions: NativeAssertion[]): NativeContract[] {
  const request: WireReference = { verb: "e", direction: "in", occurrence: "last", match: { "/ev": "edit", "/n": edit.n } };
  const property = (field: keyof typeof edit): NativeContract => ({ entry: `property:TspEvent.${field}`, request: { ...request, pointer: `/${field}` }, value: edit[field], assertions });
  return [eventContract("edit", assertions, { "/n": edit.n }, plainEditor), property("from"), property("to"), property("text"), property("cursor"), property("len"), property("n")];
}

const edited = "native Δ😀";
const acceptedEdit = { from: 0, to: initialDraft.length, text: edited, cursor: edited.length, len: initialDraft.length, n: "native-accepted" };
const staleEdit = { from: 0, to: 0, text: "REJECTED", cursor: 8, len: initialDraft.length, n: "native-stale" };
const imageAssertions: NativeAssertion[] = [
  { name: "real image decoded from delivered blob", browser: { type: "dom", selector: imageSelector, read: "naturalWidth" }, expected: 1 },
  { name: "real image decoded height", browser: { type: "dom", selector: imageSelector, read: "naturalHeight" }, expected: 1 },
  { name: "real image controller mounted", browser: { type: "effect", channel: "native-image-state", pointer: "/mounted" }, expected: true },
];

const blobId = createHash("sha256").update(Buffer.from(PNG_BASE64, "base64")).digest("hex");
export const fixtureCases: FixtureCase[] = [
  ...contextCases("l.probe"),
  ...helloCases(),
  ...paletteCases(),
  ...screenCases(),
  ...galleryCases({ gallery: "l", framePointer: "/ops/0/4/c/0", nodeId: kind => ["overlay", "picker"].includes(kind) ? `l.^gallery-${kind}` : `l.gallery-${kind}` }),
  ...toggleCases(),
  ...focusCases({ first: "n.input", second: "o.input" }),
  ...opCases({ stage: "m.stage", alpha: "m.stage/alpha", beta: "m.stage/beta" }),
  {
    id: "native-composer-atomic-send-and-draft-recall", covers: [],
    create: context => createEditorHost(context),
    checkpoints: [
      { name: "blank atomic send preserves draft and makes no submission", browserActions: [{ type: "nativeEvent", selector: editorSelector, event: { ev: "send", text: " \n " } }], comparisons: [], nativeContracts: sendContracts(" \n ", [value(initialDraft), state("/text", initialDraft), state("/submissions", [])]) },
      { name: "atomic payload replaces rather than appends draft", browserActions: [{ type: "nativeEvent", selector: editorSelector, event: { ev: "send", text: "actual atomic payload" } }], comparisons: [], nativeContracts: sendContracts("actual atomic payload", [value(""), state("/text", ""), state("/submissions", ["actual atomic payload"])]) },
      { name: "displaced local draft remains available for canonical history recall", browserActions: [activateControl(4)], comparisons: [], nativeContracts: [eventContract("activate", [value(initialDraft), state("/text", initialDraft), state("/submissions", ["actual atomic payload"])], { "/item": "4" })] },
    ],
  },
  {
    id: "native-composer-send-readiness-and-rejection", covers: [],
    create: context => createEditorHost(context),
    checkpoints: [
      { name: "submission lock changes native readiness without disabling editing", browserActions: [activateControl(0)], comparisons: [], nativeContracts: [editorFrame([value(initialDraft), state("/disabled", true), { name: "editing remains writable", browser: { type: "dom", selector: editorSelector, read: "readonly" }, expected: false }])] },
      { name: "locked atomic send has no callback or mutation", browserActions: [{ type: "nativeEvent", selector: editorSelector, event: { ev: "send", text: "blocked atomic payload" } }], comparisons: [], nativeContracts: sendContracts("blocked atomic payload", [value(initialDraft), state("/submissions", []), state("/disabled", true)]) },
      { name: "unlock restores advertised send readiness", browserActions: [activateControl(1)], comparisons: [], nativeContracts: [editorFrame([value(initialDraft), state("/disabled", false), state("/submissionWired", true)])] },
      { name: "removing submit callback revokes readiness", browserActions: [activateControl(2)], comparisons: [], nativeContracts: [editorFrame([value(initialDraft), state("/submissionWired", false)])] },
      { name: "unwired atomic send is not treated as Enter", browserActions: [{ type: "nativeEvent", selector: editorSelector, event: { ev: "send", text: "unwired atomic payload" } }], comparisons: [], nativeContracts: sendContracts("unwired atomic payload", [value(initialDraft), state("/submissions", []), state("/submissionWired", false)]) },
      { name: "restoring canonical callback enables atomic send", browserActions: [activateControl(3)], comparisons: [], nativeContracts: [editorFrame([value(initialDraft), state("/submissionWired", true)])] },
      { name: "restored controller submits exactly once", browserActions: [{ type: "nativeEvent", selector: editorSelector, event: { ev: "send", text: "restored atomic payload" } }], comparisons: [], nativeContracts: sendContracts("restored atomic payload", [value(""), state("/submissions", ["restored atomic payload"])]) },
    ],
  },
  ...(["editor", "input"] as const).map((kind): FixtureCase => {
    const selector = kind === "editor" ? plainEditor : plainInput;
    return {
      id: `native-${kind}-does-not-opt-into-atomic-send`, covers: [],
      create: context => createEditorHost(context, kind),
      checkpoints: [{ name: "keyboard submit callback does not opt a generic field into atomic send", browserActions: [{ type: "nativeEvent", selector, event: { ev: "send", text: `forbidden ${kind} payload` } }], comparisons: [], nativeContracts: sendContracts(`forbidden ${kind} payload`, [value(initialDraft, selector), state("/submissions", []), state("/submissionWired", true)], selector) }],
    };
  }),
  {
    id: "native-editor-exact-edit-stale-rejection-and-undo", covers: [],
    create: context => createEditorHost(context, "editor"),
    checkpoints: [
      { name: "exact UTF-16 edit reaches production Editor", browserActions: [{ type: "nativeEvent", selector: plainEditor, event: { ev: "edit", ...acceptedEdit } }], comparisons: [], nativeContracts: [...editContracts(acceptedEdit, [value(edited, plainEditor), state("/text", edited), state("/submissions", [])]), frameContract("operation:splice", [value(edited, plainEditor), state("/text", edited)])] },
      { name: "stale-length edit is rejected by canonical controller", browserActions: [{ type: "nativeEvent", selector: plainEditor, event: { ev: "edit", ...staleEdit } }], comparisons: [], nativeContracts: editContracts(staleEdit, [value(edited, plainEditor), state("/text", edited), state("/submissions", [])]) },
      { name: "appending text generates the canonical append operation", browserActions: [{ type: "nativeEvent", selector: plainEditor, event: { ev: "edit", from: edited.length, to: edited.length, text: " + tail", cursor: edited.length + 7, len: edited.length, n: "native-append" } }], comparisons: [], nativeContracts: [frameContract("operation:text", [value(`${edited} + tail`, plainEditor), state("/text", `${edited} + tail`)])] },
      { name: "native undo uses canonical undo history", browserActions: [{ type: "nativeEvent", selector: plainEditor, event: { ev: "undo" } }, { type: "nativeEvent", selector: plainEditor, event: { ev: "undo" } }], comparisons: [], nativeContracts: [eventContract("undo", [value(initialDraft, plainEditor), state("/text", initialDraft), state("/submissions", [])], {}, plainEditor)] },
    ],
  },
  {
    id: "native-blob-cache-query-reply-and-inline-fallback", covers: [],
    create: createBlobHost,
    checkpoints: [
      { name: "canonical image query after negotiating the actual ACP blob cache responder receives production cache reply and real image bytes", browserActions: [{ type: "nativeOpen", features: ["send", "undo", "scroll", "blobs"] }, activateControl(0)], comparisons: [], nativeContracts: [
        { entry: "verb:b", witness: "request", request: { verb: "b", direction: "out", occurrence: "last" }, response: { verb: "q", direction: "out", occurrence: "last", match: { "/q": "blobs", "/ids": [blobId] } }, assertions: imageAssertions },
        queryContract("query:blobs"),
        queryContract("verb:q"),
        queryContract("property:TspQuery.q", "/q", "blobs"),
        queryContract("property:TspQuery.ids", "/ids", [blobId]),
        frameContract("operation:add", imageAssertions),
        frameContract("verb:f", imageAssertions),
        frameContract("property:TspFrame.sf", imageAssertions, "request", "/sf"),
        frameContract("property:TspFrame.s", imageAssertions, "request", "/s"),
        frameContract("property:TspFrame.ops", imageAssertions, "request", "/ops"),
        frameContract("event:ack", imageAssertions, "response"),
        frameContract("property:TspEvent.s", imageAssertions, "response", "/s"),
        openContract("verb:o"),
        openContract("property:TspOpen.id", "/id"),
        openContract("property:TspOpen.mode", "/mode", "inline"),
        openContract("property:TspOpen.title", "/title", "omp"),
        openContract("property:TspOpen.role", "/role"),
      ] },
      { name: "moving a live image keeps its actual bytes and component identity", browserActions: [activateControl(2)], comparisons: [], nativeContracts: [frameContract("operation:move", [...imageAssertions, { name: "real component child index moved", browser: { type: "effect", channel: "native-image-state", pointer: "/index" }, expected: 0 }])] },
      { name: "finalizing an actual live component emits canonical settle hint", browserActions: [activateControl(3)], comparisons: [], nativeContracts: [frameContract("operation:settle", [...imageAssertions, { name: "canonical component finalized", browser: { type: "effect", channel: "native-image-state", pointer: "/settled" }, expected: true }])] },
      { name: "removing image updates actual controller and HTML", browserActions: [activateControl(1)], comparisons: [], nativeContracts: [eventContract("activate", [{ name: "native image removed", browser: { type: "dom", selector: imageSelector, read: "count" }, expected: 0 }, { name: "real owner removed image", browser: { type: "effect", channel: "native-image-state", pointer: "/mounted" }, expected: false }], { "/item": "1" }), frameContract("operation:del", [{ name: "removed image has no native HTML node", browser: { type: "dom", selector: imageSelector, read: "count" }, expected: 0 }, { name: "canonical owner removed image", browser: { type: "effect", channel: "native-image-state", pointer: "/index" }, expected: -1 }])] },
    ],
  },
  {
    id: "native-picker-select-then-confirm-preserves-controller-ownership", covers: [],
    create: context => createEditorHost(context, "composer", true),
    checkpoints: [
      {
        name: "select highlights a real option without applying it",
        browserActions: [{ type: "nativeEvent", selector: pickerNode, event: { ev: "select", item: "1" } }], comparisons: [],
        nativeContracts: [
          eventContract("select", [value(initialDraft), state("/disabled", true), state("/submissionWired", true)], { "/item": "1" }, pickerNode),
          { entry: "property:TspEvent.item", request: { verb: "e", direction: "in", occurrence: "last", match: { "/ev": "select", "/item": "1" }, pointer: "/item" }, value: "1", assertions: [state("/disabled", true)] },
        ],
      },
      {
        name: "confirm applies only the canonical selected option",
        browserActions: [{ type: "click", selector: `${pickerNode} button:has-text("Select")` }], comparisons: [],
        nativeContracts: [
          eventContract("action", [value(initialDraft), state("/disabled", false), state("/submissions", [])], { "/act": "confirm" }),
          { entry: "property:TspEvent.act", request: { verb: "e", direction: "in", occurrence: "last", match: { "/ev": "action", "/act": "confirm" }, pointer: "/act" }, value: "confirm", assertions: [state("/disabled", false), state("/submissions", [])] },
          editorFrame([value(initialDraft), state("/disabled", false)]),
          frameContract("operation:set", [value(initialDraft), state("/disabled", false)]),
        ],
      },
    ],
  },
  {
    id: "native-disclosure-controlled-toggle-lazy-ownership", covers: [],
    create: createDisclosureHost,
    checkpoints: [
      ...([false, true, false] as const).map((collapsed, index) => {
        const assertions: NativeAssertion[] = [
          { name: "actual controller-owned disclosure state", browser: { type: "dom", selector: "button[aria-expanded]", read: "expanded" }, expected: !collapsed },
          { name: "actual expanded body ownership", browser: { type: "dom", selector: 'text="Canonical expanded body"', read: "count" }, expected: collapsed ? 0 : 1 },
          { name: "canonical lazy body is materialized once across toggles", browser: { type: "effect", channel: "native-disclosure-materialized" }, expected: 1 },
        ];
        return {
          name: `canonical disclosure toggle ${index + 1}`, browserActions: [{ type: "click" as const, selector: "button[aria-expanded]" }], comparisons: [],
          nativeContracts: [
            eventContract("toggle", assertions, { "/collapsed": collapsed }, "button[aria-expanded]"),
            { entry: "property:TspEvent.collapsed", request: { verb: "e" as const, direction: "in" as const, occurrence: "last" as const, match: { "/ev": "toggle", "/collapsed": collapsed }, pointer: "/collapsed" }, value: collapsed, assertions },
          ],
        };
      }),
    ],
  },
  {
    id: "native-advisor-typed-change-commit-and-validation", covers: [],
    create: createAdvisorHost,
    checkpoints: [
      ...([
        { item: "toggleEnabled", value: false, field: "enabled", expected: false },
        { item: "toggleEnabled", value: "invalid-enabled-value", field: "enabled", expected: false },
        { item: "name", value: "Configured native reviewer", field: "name", expected: "Configured native reviewer" },
        { item: "reviewInterval", value: 3, field: "reviewInterval", expected: 3 },
        { item: "reviewInterval", value: 0, field: "reviewInterval", expected: 3 },
        { item: "reviewInterval", value: null, field: "reviewInterval", expected: null },
        { item: "tools", value: ["read", "glob"], field: "tools", expected: ["read", "glob"] },
      ] as const).map(change => {
        const assertions: NativeAssertion[] = [
          { name: "actual advisor stays available", browser: { type: "dom", selector: '[role="switch"][aria-label="Enabled"]', read: "checked" }, expected: false },
        ];
        if (change.value === null) {
          assertions.push(
            { name: "null restores canonical interval default", browser: { type: "dom", selector: 'input[type="number"][aria-label="Review interval"]', read: "value" }, expected: "1" },
            { name: "reset still commits to owning scope", browser: { type: "file", path: "native-advisor.json", pointer: "/scope" }, expected: "project" },
          );
        } else {
          assertions.push({ name: "canonical app persists the actual typed value", browser: { type: "file", path: "native-advisor.json", pointer: `/doc/advisors/0/${change.field}` }, expected: change.expected });
        }
        const request: WireReference = { verb: "e", direction: "in", occurrence: "last", match: { "/ev": "change", "/item": change.item, "/value": change.value } };
        return {
          name: `canonical typed change ${change.item} ${JSON.stringify(change.value)}`,
          browserActions: [
            { type: "nativeEvent" as const, selector: '[role="switch"][aria-label="Enabled"]', event: { ev: "change", item: change.item, value: change.value } },
            { type: "click" as const, selector: 'button:text-is("Save")' },
          ], comparisons: [],
          nativeContracts: [
            eventContract("change", assertions, { "/item": change.item, "/value": change.value }, '[role="switch"][aria-label="Enabled"]'),
            { entry: "property:TspEvent.value", request: { ...request, pointer: "/value" }, value: change.value, assertions },
            eventContract("action", assertions, { "/act": "save" }),
          ],
        };
      }),
    ],
  },
];

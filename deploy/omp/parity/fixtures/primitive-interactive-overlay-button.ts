import type * as LoginDialog from "@omp-source/packages/tui/src/overlays/login-dialog";
import type * as Review from "@omp-source/packages/tui/src/overlays/plan-review-overlay";
import type * as ApprovalDialog from "@omp-source/packages/tui/src/overlays/approval-dialog";
import type * as AskDialog from "@omp-source/packages/tui/src/overlays/ask-dialog";
import type * as TextModule from "@omp-source/packages/tui/src/components/text";
import type * as RowModule from "@omp-source/packages/tui/src/components/layout/row";
import type * as TuiModule from "@omp-source/packages/tui/src/tui";
import type { Comparison, FixtureCase, FixtureContext } from "../scenario";
import { contentComparison } from "./gallery-support";

const overlay = "packages/tui/src/overlays/";
const dialogHead = '[role="dialog"] h3';

/** The real plan-review overlay: its heading is the overlay's `head`, shown above the plan body inside the same sheet. */
const planReviewHead: FixtureCase = {
  id: "primitive-overlay-plan-review-head",
  covers: ["kind:overlay", "kind:button", `component:${overlay}plan-review-overlay.ts#PlanReviewOverlay`],
  async create(context) {
    const { PlanReviewOverlay } = await context.load<typeof Review>(`${overlay}plan-review-overlay.ts`);
    return new PlanReviewOverlay("# Fixture review\n\nPreserve fixture state.", { options: ["Fixture approve", "Fixture revise"], promptTitle: "Fixture decision" }, { onPick: label => { context.record("pick", label); context.done(); }, onCancel: () => { context.record("cancel", true); context.done(); }, onCopyPlan: content => { context.record("copy", content); } });
  },
  checkpoints: [{ name: "plan review sheet", comparisons: [
    contentComparison("plan body inside the sheet", "(Preserve fixture state\\.)", '[data-omp-role="omp.plan.body"] p', "Preserve fixture state."),
  ] }],
};

/** The real login sheet with an authorization step and a manual-code field. */
const loginSheet: FixtureCase = {
  id: "primitive-overlay-login-sheet",
  covers: ["kind:overlay", "kind:button", "property:TspOverlayProps.head", `component:${overlay}login-dialog.ts#LoginDialogComponent`],
  async create(context) {
    const { LoginDialogComponent } = await context.load<typeof LoginDialog>(`${overlay}login-dialog.ts`);
    const dialog = new LoginDialogComponent(context.tui, "parity-provider", (success, message) => { context.record("complete", { success, message }); context.done(); }, url => context.record("open", url));
    dialog.showAuth("https://auth.parity.example/authorize?state=parity", "Approve fixture access in your browser");
    void dialog.showManualInput("Paste fixture authorization code").then(code => { context.record("code", code); context.done(); }, () => undefined);
    return dialog;
  },
  checkpoints: [{ name: "authorization sheet", comparisons: [
    contentComparison("manual prompt", "(Paste fixture authorization code)", '[data-omp-role="omp.login.label"]', "Paste fixture authorization code"),
    contentComparison("authorization instructions", "(Approve fixture access in your browser)", '[data-omp-role="omp.login.lead"]', "Approve fixture access in your browser"),
    contentComparison("sheet owner", "(Approve fixture access in your browser)", '[data-omp-node^="layer.ov-"]:has-text("Approve fixture access")', "Approve fixture access in your browser"),
    contentComparison("sheet head", "(Login to parity-provider)", dialogHead, "Login to parity-provider", [{ entry: "property:TspOverlayProps.head", effect: "literal", value: "Login to parity-provider", target: { checkpoint: "authorization sheet", comparison: "sheet owner" } }]),
  ] }],
};


/** The real approval dialog: its Deny and Approve action buttons carry the same words as the terminal's choice rows. */
const approvalButtons: FixtureCase = {
  id: "primitive-button-approval-text",
  covers: ["kind:button", "property:TspButtonProps.text", `component:${overlay}approval-dialog.ts#ApprovalDialogComponent`],
  async create(context) {
    const { ApprovalDialogComponent, APPROVE_ARM_MS } = await context.load<typeof ApprovalDialog>(`${overlay}approval-dialog.ts`);
    const dialog = new ApprovalDialogComponent("Allow tool: bash\nCommand: printf parity", { onApprove: () => { context.record("decision", "approve"); context.done(); }, onDeny: () => { context.record("decision", "deny"); context.done(); } });
    const armed = Promise.withResolvers<void>();
    setTimeout(armed.resolve, APPROVE_ARM_MS);
    await armed.promise;
    return dialog;
  },
  checkpoints: [{ name: "action bar", comparisons: [
    contentComparison("deny button", "(Deny)", '[data-omp-role="omp.actions"] button:has-text("Deny")', "Deny", [{ entry: "property:TspButtonProps.text", effect: "literal", value: "Deny", target: { checkpoint: "action bar", comparison: "deny button" } }]),
    contentComparison("approve button", "(Approve)", '[data-omp-role="omp.actions"] button:has-text("Approve")', "Approve", [{ entry: "property:TspButtonProps.text", effect: "literal", value: "Approve", target: { checkpoint: "action bar", comparison: "approve button" } }]),
  ] }],
};

/** The real ask dialog: its Note action button carries the keycap the terminal's footer hint shows. */
const askNoteKey: FixtureCase = {
  id: "primitive-button-ask-note-key",
  covers: ["kind:button", "property:TspButtonProps.keys", `component:${overlay}ask-dialog.ts#AskDialogComponent`],
  async create(context) {
    const { AskDialogComponent } = await context.load<typeof AskDialog>(`${overlay}ask-dialog.ts`);
    return new AskDialogComponent([{ id: "note", question: "Add a note?", options: [{ label: "Yes" }, { label: "No" }] }], {
      onSubmit: () => { context.record("answer", true); context.done(); },
      onCancel: () => { context.record("cancel", true); context.done(); },
      onPrompt: async () => undefined,
    });
  },
  checkpoints: [{ name: "note action", comparisons: [
    contentComparison("note keycap", "(n) note", '[data-omp-node="l.actions/note"] kbd:not(:has(kbd))', "n", [{ entry: "property:TspButtonProps.keys", effect: "content", value: ["n"], parts: [{ pointer: "/0", effect: "literal", comparison: { checkpoint: "note action", comparison: "note keycap" } }] }]),
  ] }],
};

const surface = '[id^="omp-view-"][data-omp-surface]';
const panel = '[data-omp-overlay="nonmodal"]:has-text("LQ")';
const leaf = (marker: string) => `${panel} [data-omp-node]:not(:has([data-omp-node])):has-text("${marker}")`;
type Proof = NonNullable<Comparison["proves"]>[number];
const ownerReference = { checkpoint: "placed layer", comparison: "layer owner" };
const claim = (entry: string, value: unknown): Proof => ({ entry: `property:TspOverlayProps.${entry}`, effect: "layout", value, target: ownerReference }) as Proof;
const marginClaim = (side: string, value: number): Proof => ({ entry: `property:TspOverlayMargin.${side}`, effect: "layout", value, target: ownerReference }) as Proof;
const layerOwner: Comparison = { name: "layer owner", terminal: { type: "screen", pattern: "LQ", read: "visible" }, browser: { type: "dom", selector: panel, read: "insideViewport", relativeTo: surface }, expected: true };

function half(marker: string, read: "leftHalf" | "rightHalf" | "topHalf" | "bottomHalf", expected: boolean, proves: Proof[] = []): Comparison {
  return { name: `${marker} ${read}`, terminal: { type: "screen", pattern: marker, read }, browser: { type: "dom", selector: leaf(marker), read, relativeTo: surface }, expected, ...(proves.length ? { proves } : {}) };
}
function cell(marker: string, axis: "column" | "row", expected: number, proves: Proof[] = []): Comparison {
  return { name: `${marker} ${axis}`, terminal: { type: "screen", pattern: marker, read: axis }, browser: { type: "dom", selector: leaf(marker), read: axis === "column" ? "rectX" : "rectY", relativeTo: surface, unit: "cells" }, expected, ...(proves.length ? { proves } : {}) };
}
function gap(marker: string, read: "rightGap" | "bottomGap", expected: number, proves: Proof[] = []): Comparison {
  return { name: `${marker} ${read}`, terminal: { type: "screen", pattern: marker, read }, browser: { type: "dom", selector: leaf(marker), read, relativeTo: surface, unit: "cells" }, expected, ...(proves.length ? { proves } : {}) };
}

/** A real ask dialog owns the view while a passive layer with the given placement floats above it, so the overlay placement reaches the page through the dialog's own render path. */
function askOverlayGeometry(id: string, options: TuiModule.OverlayOptions, comparisons: Comparison[]): FixtureCase {
  return {
    id: `primitive-overlay-ask-${id}`,
    covers: ["kind:overlay", `component:${overlay}ask-dialog.ts#AskDialogComponent`],
    async create(context: FixtureContext) {
      const [{ AskDialogComponent }, { Text }, { Row }, { Container }] = await Promise.all([
        context.load<typeof AskDialog>(`${overlay}ask-dialog.ts`),
        context.load<typeof TextModule>("packages/tui/src/components/text.ts"),
        context.load<typeof RowModule>("packages/tui/src/components/layout/row.ts"),
        context.load<typeof TuiModule>("packages/tui/src/tui.ts"),
      ]);
      const dialog = new AskDialogComponent([{ id: "policy", question: "Keep the placement fixture?", options: [{ label: "Keep" }] }], {
        onSubmit: () => { context.record("answer", true); context.done(); },
        onCancel: () => { context.record("cancel", true); context.done(); },
        onPrompt: async () => undefined,
      });
      const content = new Container();
      for (const [left, right] of [["LQ", "RQ"], ["MQ", "NQ"], ["BQ", "EQ"]]) {
        content.addChild(new Row({ children: [{ content: new Text(left, 0, 0), width: 2 }, { content: new Text("", 0, 0), grow: 1 }, { content: new Text(right, 0, 0), width: 2 }] }));
      }
      context.tui.showOverlay(content, options);
      const host = new Container();
      host.addChild(dialog);
      return host;
    },
    checkpoints: [{ name: "placed layer", comparisons: [layerOwner, ...comparisons] }],
  };
}

const anchors = ["center", "top-left", "top-right", "top-center", "bottom-left", "bottom-right", "bottom-center", "left-center", "right-center"] as const;
const anchorCases = anchors.map(anchor => {
  const horizontal = anchor.includes("left") ? [true, false] : anchor.includes("right") ? [false, true] : [true, true];
  const vertical = anchor.startsWith("top") ? [true, false] : anchor.startsWith("bottom") ? [false, true] : [true, true];
  return askOverlayGeometry(`anchor-${anchor}`, { anchor, width: 20, margin: anchor.startsWith("bottom") ? { bottom: 13 } : 0 }, [
    half("LQ", "leftHalf", horizontal[0]!, [claim("anchor", anchor)]), half("RQ", "rightHalf", horizontal[1]!),
    half("LQ", "topHalf", vertical[0]!), half("BQ", "bottomHalf", vertical[1]!),
  ]);
});
const geometryCases: FixtureCase[] = [
  ...anchorCases,
  askOverlayGeometry("width", { anchor: "top-left", width: 24, margin: 0 }, [cell("LQ", "column", 0), cell("RQ", "column", 22, [claim("width", 24)])]),
  askOverlayGeometry("min-width", { anchor: "top-left", width: 12, minWidth: 42, margin: 0 }, [cell("LQ", "column", 0), cell("RQ", "column", 40, [claim("minWidth", 42)])]),
  askOverlayGeometry("absolute-cell", { anchor: "bottom-right", width: 20, row: 3, col: 7, margin: 0 }, [cell("LQ", "row", 3, [claim("row", 3)]), cell("LQ", "column", 7, [claim("col", 7)])]),
  askOverlayGeometry("offset", { anchor: "top-left", width: 20, offsetX: 9, offsetY: 4, margin: 0 }, [cell("LQ", "column", 9, [claim("offsetX", 9)]), cell("LQ", "row", 4, [claim("offsetY", 4)])]),
  askOverlayGeometry("uniform-margin", { anchor: "top-left", width: 20, margin: 3 }, [cell("LQ", "column", 3, [claim("margin", 3)]), cell("LQ", "row", 3)]),
  askOverlayGeometry("side-margins", { anchor: "top-left", width: 20, margin: { top: 2, left: 5, bottom: 4, right: 8 } }, [cell("LQ", "column", 5, [marginClaim("left", 5)]), cell("LQ", "row", 2, [marginClaim("top", 2)])]),
  askOverlayGeometry("far-side-margins", { anchor: "bottom-right", width: 20, margin: { top: 2, left: 5, bottom: 14, right: 8 } }, [gap("RQ", "rightGap", 8, [marginClaim("right", 8)]), gap("EQ", "bottomGap", 14, [marginClaim("bottom", 14)])]),
  askOverlayGeometry("max-height", { anchor: "top-left", width: 20, maxHeight: 2, margin: 0 }, [{ name: "second row fits inside the panel", terminal: { type: "screen", pattern: "MQ", read: "visible" }, browser: { type: "dom", selector: leaf("MQ"), read: "insideViewport", relativeTo: panel }, expected: true, proves: [claim("maxHeight", 2)] }, { name: "third row clipped", terminal: { type: "screen", pattern: "BQ", read: "visible" }, browser: { type: "dom", selector: leaf("BQ"), read: "insideViewport", relativeTo: panel }, expected: false }]),
];

export const fixtureCases: FixtureCase[] = [planReviewHead, loginSheet, approvalButtons, askNoteKey, ...geometryCases];

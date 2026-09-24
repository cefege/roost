// Terminal-menu debugging items in BOTH layout branches. Bun has no browser DOM,
// so this suite uses the repo's client-Solid virtual renderer (see
// pairRequestCard.dom.test.ts) and stubs the recorder seam plus the M3 primitives.
// It pins the three test IDs, their enabled/disabled state, the visible lease
// state, and that activating Start only freezes evidence — nothing is sent
// before the consent dialog is confirmed.

import { beforeEach, describe, expect, mock, test } from "bun:test";
import type * as SolidApi from "solid-js";
import type { Session } from "@roost/protocol/wire";
import type { TerminalCaptureReason } from "@roost/protocol/terminal-capture";

interface VNode {
  tag: unknown;
  props: Record<string, unknown>;
  rendered?: unknown;
}

const SESSION_ID = "00000000-0000-4000-8000-000000000001";

const seamOrder: string[] = [];
const announced: string[] = [];
let uiPhase: "idle" | "arming" | "recording" | "expired" | "error" = "idle";
let uiError: string | null = null;

const solidClientUrl = new URL("./solid.js", import.meta.resolve("solid-js"));
const Solid = await import(solidClientUrl.href) as typeof SolidApi;
mock.module("solid-js", () => Solid);

function invokeComponent(vnode: VNode): void {
  if (typeof vnode.tag !== "function") return;
  const component = vnode.tag as (props: Record<string, unknown>) => unknown;
  const owner = Solid.getOwner();
  vnode.rendered = Solid.runWithOwner(owner, () => component(vnode.props));
}

function createElement(tag: unknown, props: Record<string, unknown> | null): VNode {
  const vnode: VNode = { tag, props: { ...(props ?? {}) } };
  invokeComponent(vnode);
  return vnode;
}

const ReactShim = { Fragment: Symbol("terminal-menu-fragment"), createElement };
mock.module("react/jsx-dev-runtime", () => ({
  Fragment: ReactShim.Fragment,
  jsxDEV: (tag: unknown, props: Record<string, unknown> | null) => createElement(tag, props),
}));

mock.module("solid-js/web", () => ({
  Portal: (props: Record<string, unknown>) => props.children,
}));

mock.module("@solidjs/router", () => ({
  useNavigate: () => () => {},
  useLocation: () => ({ pathname: "/" }),
}));

mock.module("../src/lib/windowSizeClass.ts", () => ({
  isCompact: () => false,
}));

mock.module("../src/components/Settings/md/StatusDot.tsx", () => ({
  StatusDot: (props: Record<string, unknown>) => props.status,
}));
mock.module("../src/components/Settings/md/Button.tsx", () => ({
  Button: (props: Record<string, unknown>) => ({ tag: "button", props }),
}));
mock.module("../src/components/Settings/md/Dialog.tsx", () => ({
  Dialog: (props: Record<string, unknown>) => (props.open
    ? {
        tag: "dialog",
        props: {
          "data-testid": props.testId,
          children: [props.headline, props.description, props.children, props.actions],
        },
      }
    : null),
}));

mock.module("../src/lib/terminalIncidentCapture.ts", () => ({
  terminalCaptureUiState: () => ({
    sessionId: SESSION_ID,
    phase: uiPhase,
    recordingId: uiPhase === "idle" ? null : "11111111-1111-4111-8111-111111111111",
    expiresAtMs: null,
    lastResult: null,
    lastError: uiError,
    heldEvidence: false,
  }),
  subscribeTerminalCaptureUiState: () => () => {},
  freezeTerminalCaptureEvidence(_sessionId: string, reason: TerminalCaptureReason) {
    seamOrder.push(`freeze:${reason}`);
    return "frozen-1";
  },
  discardTerminalCaptureEvidence: (token: string) => seamOrder.push(`discard:${token}`),
  downloadLocalTerminalEvidence: () => seamOrder.push("local-download"),
  captureTerminalIncidentFrozen: async () => { seamOrder.push("capture"); return null; },
  captureTerminalIncident: async () => { seamOrder.push("capture-live"); return null; },
  startTerminalCapture: async () => { seamOrder.push("start"); return null; },
  stopTerminalCapture: async () => { seamOrder.push("stop"); return null; },
  disposeTerminalIncidentRecorder() {},
  // The replica/renderer observers share this module; a partial mock would
  // break every other importer in this file's graph.
  noteTerminalRenderApply: () => {},
  noteTerminalRenderApplied: () => {},
  noteTerminalRendererDisposed: () => {},
}));
mock.module("../src/lib/terminalCaptureDownload.ts", () => ({
  announceTerminalCaptureResult: () => announced.push("announced"),
  announceTerminalCaptureException: () => announced.push("exception"),
}));

// Mock registration must precede these imports: both bind the seam and the JSX
// runtime at evaluation time.
const { TerminalContextMenu } = await import("../src/components/TerminalContextMenu.tsx");
const { TerminalCaptureConsentDialog } = await import(
  "../src/components/TerminalCaptureConsentDialog.tsx"
);

const documentStub = {
  addEventListener() {},
  removeEventListener() {},
};
Object.defineProperty(globalThis, "document", { configurable: true, value: documentStub });

const session = { id: SESSION_ID, title: "shell" } as unknown as Session;

const openFixture = {
  x: 10,
  y: 20,
  selection: "",
  link: null,
  linkTarget: null,
};

function walk(node: unknown, onVNode: (vnode: VNode) => void): void {
  if (node === null || node === undefined || typeof node === "boolean") return;
  if (Array.isArray(node)) {
    for (const child of node) walk(child, onVNode);
    return;
  }
  if (typeof node === "function") {
    // Solid's Show passes an accessor to a callback child; memos take no argument.
    const produced = node.length > 0
      ? (node as (accessor: () => typeof openFixture) => unknown)(() => openFixture)
      : (node as () => unknown)();
    walk(produced, onVNode);
    return;
  }
  if (typeof node !== "object") return;
  const vnode = node as VNode;
  if (!vnode.props) return;
  onVNode(vnode);
  walk(vnode.props.children, onVNode);
  walk(vnode.props.fallback, onVNode);
  walk(vnode.rendered, onVNode);
}

function findVNode(root: unknown, match: (vnode: VNode) => boolean): VNode | null {
  let found: VNode | null = null;
  walk(root, (vnode) => {
    if (!found && match(vnode)) found = vnode;
  });
  return found;
}

/** Items of one layout branch, keyed by test ID. The menu-item wrapper wins over
 *  its rendered element, so `disabled` is readable for both primitives. */
function branchItems(root: unknown, variant: string): Map<string, VNode> {
  const container = findVNode(root, (vnode) => vnode.props["data-variant"] === variant);
  if (!container) throw new Error(`layout branch not rendered: ${variant}`);
  const items = new Map<string, VNode>();
  walk(container.props.children, (vnode) => {
    const id = vnode.props.testid ?? vnode.props["data-testid"];
    if (typeof id === "string" && !items.has(id)) items.set(id, vnode);
  });
  return items;
}

function itemText(vnode: VNode | undefined): string {
  if (!vnode) return "";
  const parts: string[] = [];
  const push = (node: unknown): void => {
    if (typeof node === "string" || typeof node === "number") parts.push(String(node));
  };
  push(vnode.props.children);
  walk(vnode.props.children, (child) => {
    push(child.props.children);
    if (Array.isArray(child.props.children)) child.props.children.forEach(push);
    push(child.props.status);
    push(child.props.title);
  });
  if (Array.isArray(vnode.props.children)) vnode.props.children.forEach(push);
  return parts.join(" ");
}

/** Label of the non-interactive lease-status row. The virtual renderer
 *  materializes `Show` children eagerly, so an unarmed lease is an EMPTY label
 *  rather than a missing node. */
function stateRowLabel(items: Map<string, VNode>): string {
  const row = items.get("ctx-capture-state-row");
  if (!row) throw new Error("lease status row not rendered");
  const label = findVNode(row.props.children, (vnode) => vnode.tag === "span");
  const text = label?.props.children;
  return typeof text === "string" ? text : "";
}

/** Canonical StatusDot status the row asks for. */
function stateRowDot(items: Map<string, VNode>): string {
  const row = items.get("ctx-capture-state-row");
  if (!row) throw new Error("lease status row not rendered");
  const dot = findVNode(row.props.children, (vnode) => typeof vnode.props.status === "string");
  return typeof dot?.props.status === "string" ? dot.props.status : "";
}

function renderMenu(): unknown {
  let rendered: unknown;
  const dispose = Solid.createRoot((disposeRoot) => {
    rendered = TerminalContextMenu({
      session,
      getContainer: () => null,
      onAttachFile: () => {},
      onPasteText: () => {},
      onOpenLink: () => {},
      describeLink: () => null,
    } as never);
    return disposeRoot;
  });
  dispose();
  return rendered;
}

beforeEach(() => {
  seamOrder.length = 0;
  announced.length = 0;
  uiPhase = "idle";
  uiError = null;
});

describe("terminal context menu debugging items", () => {
  test.each(["floating", "sheet"])("%s branch exposes all three debugging items", (variant) => {
    const items = branchItems(renderMenu(), variant);
    expect(items.has("ctx-debug-start")).toBe(true);
    expect(items.has("ctx-capture-diagnostics")).toBe(true);
    expect(items.has("ctx-debug-stop")).toBe(true);
    expect(itemText(items.get("ctx-debug-start"))).toContain("Start terminal debugging");
    expect(itemText(items.get("ctx-capture-diagnostics"))).toContain("Capture terminal diagnostic");
    expect(itemText(items.get("ctx-debug-stop"))).toContain("Stop terminal debugging");
    // An unarmed session has nothing to stop and no lease state to display.
    expect(items.get("ctx-debug-start")?.props.disabled).toBe(false);
    expect(items.get("ctx-debug-stop")?.props.disabled).toBe(true);
    expect(stateRowLabel(items)).toBe("");
  });

  test.each(["floating", "sheet"])("%s branch shows the recording lease", (variant) => {
    uiPhase = "recording";
    const items = branchItems(renderMenu(), variant);
    expect(stateRowLabel(items)).toBe("recording");
    expect(stateRowDot(items)).toBe("running");
    // The lease state must not live inside the item that is disabled while it
    // matters; the row carries it instead.
    expect(itemText(items.get("ctx-debug-start"))).not.toContain("recording");
    expect(items.get("ctx-debug-start")?.props.disabled).toBe(true);
    expect(items.get("ctx-capture-diagnostics")?.props.disabled).toBe(false);
    expect(items.get("ctx-debug-stop")?.props.disabled).toBe(false);
  });

  test.each(["floating", "sheet"])("%s branch shows an expired lease", (variant) => {
    uiPhase = "expired";
    const items = branchItems(renderMenu(), variant);
    expect(stateRowLabel(items)).toBe("lease expired · start again");
    expect(stateRowDot(items)).toBe("warn");
    // Expiry must be restartable, never silently renewed.
    expect(items.get("ctx-debug-start")?.props.disabled).toBe(false);
    expect(seamOrder).toEqual([]);
  });

  test.each(["floating", "sheet"])("%s branch shows the fixed failure code", (variant) => {
    uiPhase = "error";
    uiError = "worker_timeout";
    const items = branchItems(renderMenu(), variant);
    expect(stateRowLabel(items)).toBe("failed · worker_timeout");
    expect(stateRowDot(items)).toBe("error");
  });

  test("activating Start freezes evidence and sends nothing", () => {
    const items = branchItems(renderMenu(), "floating");
    const start = items.get("ctx-debug-start");
    (start?.props.onClick as () => void)();
    expect(seamOrder).toEqual(["freeze:manual"]);
    expect(announced).toEqual([]);
  });

  test("the consent dialog carries the confirmation controls", () => {
    let confirmed = 0;
    let cancelled = 0;
    let rendered: unknown;
    const dispose = Solid.createRoot((disposeRoot) => {
      rendered = TerminalCaptureConsentDialog({
        kind: "start",
        onConfirm: () => { confirmed++; },
        onCancel: () => { cancelled++; },
      } as never);
      return disposeRoot;
    });
    dispose();

    const dialog = findVNode(rendered, (vnode) => vnode.props["data-testid"] === "ctx-debug-consent");
    expect(dialog).not.toBe(null);
    const confirm = findVNode(rendered, (vnode) => vnode.props["data-testid"] === "ctx-debug-consent-confirm");
    const cancel = findVNode(rendered, (vnode) => vnode.props["data-testid"] === "ctx-debug-consent-cancel");
    (confirm?.props.onClick as () => void)();
    (cancel?.props.onClick as () => void)();
    expect(confirmed).toBe(1);
    expect(cancelled).toBe(1);
    expect(seamOrder).toEqual([]);
  });
});

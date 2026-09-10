// AgentLauncherPane dispatches persistence through void UI callbacks. This virtual
// Solid DOM suite forces the coordinator update gate to reject and observes the
// callbacks' user-visible error path without loading browser-only M3 elements.
// Browser smoke owns the native md-switch controlled-state restoration proof.

import { afterEach, beforeEach, describe, expect, mock, test } from "bun:test";
import type * as SolidApi from "solid-js";

type VNode = {
  tag: unknown;
  props: Record<string, unknown>;
  rendered?: unknown;
};

interface CapturedControls {
  select?: { onChange: (value: string) => void };
  textField?: { onInput: (value: string) => void };
  save?: { onClick: () => void };
  autoLaunch?: { checked: boolean; onChange: (enabled: boolean) => void };
}

const solidClientUrl = new URL("./solid.js", import.meta.resolve("solid-js"));
const Solid = await import(solidClientUrl.href) as typeof SolidApi;
const toasts: Array<{ message: string; kind: string | undefined }> = [];
let controls: CapturedControls = {};

mock.module("solid-js", () => Solid);

function invokeComponent(vnode: VNode): void {
  if (typeof vnode.tag !== "function") return;
  const component = vnode.tag as (props: Record<string, unknown>) => unknown;
  vnode.rendered = Solid.runWithOwner(Solid.getOwner(), () => component(vnode.props));
}

function createElement(
  tag: unknown,
  props: Record<string, unknown> | null,
  ...children: unknown[]
): VNode {
  const merged = { ...(props ?? {}) };
  if (children.length > 0) merged.children = children.length === 1 ? children[0] : children;
  const vnode: VNode = { tag, props: merged };
  invokeComponent(vnode);
  return vnode;
}

const ReactShim = { Fragment: Symbol("Fragment"), createElement };
const testGlobal = globalThis as typeof globalThis & { React: unknown };
testGlobal.React = ReactShim;
mock.module("react/jsx-dev-runtime", () => ({
  Fragment: ReactShim.Fragment,
  jsxDEV(tag: unknown, props: Record<string, unknown> | null): VNode {
    const children = props?.children === undefined ? [] : [props.children];
    return createElement(tag, props, ...children);
  },
}));

function captureSelect(props: Record<string, unknown>): null {
  controls.select = props as unknown as CapturedControls["select"];
  return null;
}

function captureTextField(props: Record<string, unknown>): null {
  controls.textField = props as unknown as CapturedControls["textField"];
  return null;
}

function captureSave(props: Record<string, unknown>): null {
  controls.save = props as unknown as CapturedControls["save"];
  return null;
}

function captureSwitch(props: Record<string, unknown>): null {
  controls.autoLaunch = props as unknown as CapturedControls["autoLaunch"];
  return null;
}

mock.module("../src/components/Settings/md/primitives.tsx", () => ({
  Select: captureSelect,
  TextField: captureTextField,
  Button: captureSave,
  Switch: captureSwitch,
}));
mock.module("../src/components/AgentGlyph.tsx", () => ({ AgentTile: () => null }));
mock.module("../src/store/toastStore.ts", () => ({
  addToast: mock((message: string, kind?: string) => {
    toasts.push({ message, kind });
    return () => undefined;
  }),
}));

const agentConfigSet = mock((_request: unknown) => Promise.resolve({}));
mock.module("../src/connect.ts", () => ({ coordClient: { agentConfigSet } }));

// These imports must follow the mock registration because the pane and its
// agent persistence helper bind their dependencies at module evaluation time.
const { AgentLauncherPane } = await import("../src/components/Settings/AgentLauncherPane.tsx");
const {
  autoLaunchEnabled,
  clearAgentConfigForAuthBoundary,
  saveAgentConfig,
  saveAutoLaunch,
} = await import("../src/lib/agents.ts");

function mountPane(): { dispose: () => void } {
  let dispose!: () => void;
  Solid.createRoot((rootDispose) => {
    dispose = rootDispose;
    AgentLauncherPane({});
  });
  return { dispose };
}

async function settleCallbacks(): Promise<void> {
  for (let turn = 0; turn < 4; turn++) await Promise.resolve();
}

async function dispatchWithoutUnhandledRejection(dispatch: () => void): Promise<unknown[]> {
  const unhandled: unknown[] = [];
  const onUnhandled = (event: PromiseRejectionEvent) => {
    unhandled.push(event.reason);
    event.preventDefault();
  };
  globalThis.addEventListener("unhandledrejection", onUnhandled);
  try {
    expect(dispatch()).toBeUndefined();
    await settleCallbacks();
    return unhandled;
  } finally {
    globalThis.removeEventListener("unhandledrejection", onUnhandled);
  }
}

beforeEach(() => {
  controls = {};
  toasts.length = 0;
  clearAgentConfigForAuthBoundary();
  agentConfigSet.mockClear();
  agentConfigSet.mockImplementation((_request: unknown) => Promise.resolve({}));
});

afterEach(() => {
  clearAgentConfigForAuthBoundary();
});

describe("AgentLauncherPane rejected coordinator updates", () => {
  test("Select contains a rejected update behind its void callback and surfaces an error toast", async () => {
    const mounted = mountPane();
    try {
      agentConfigSet.mockImplementation(() => Promise.reject(new Error("select denied")));

      const unhandled = await dispatchWithoutUnhandledRejection(() => controls.select!.onChange("codex"));

      expect(agentConfigSet).toHaveBeenCalledWith({
        selected: "codex",
        customCommand: "",
        autoLaunch: false,
      });
      expect(unhandled).toEqual([]);
      expect(toasts).toEqual([{ message: "Default agent save failed: select denied", kind: "err" }]);
    } finally {
      mounted.dispose();
    }
  });

  test("Save contains a rejected custom-command update behind its void callback and surfaces an error toast", async () => {
    await saveAgentConfig("custom", "existing-agent");
    agentConfigSet.mockClear();
    const mounted = mountPane();
    try {
      controls.textField!.onInput("  aider --model sonnet  ");
      agentConfigSet.mockImplementation(() => Promise.reject(new Error("custom denied")));

      const unhandled = await dispatchWithoutUnhandledRejection(() => controls.save!.onClick());

      expect(agentConfigSet).toHaveBeenCalledWith({
        selected: "custom",
        customCommand: "aider --model sonnet",
        autoLaunch: false,
      });
      expect(unhandled).toEqual([]);
      expect(toasts).toEqual([{ message: "Default agent save failed: custom denied", kind: "err" }]);
    } finally {
      mounted.dispose();
    }
  });

  test("Switch retains the persisted setting after a rejected update and surfaces an error toast", async () => {
    await saveAutoLaunch(true);
    agentConfigSet.mockClear();
    const mounted = mountPane();
    try {
      expect(controls.autoLaunch!.checked).toBe(true);
      agentConfigSet.mockImplementation(() => Promise.reject(new Error("auto denied")));

      const unhandled = await dispatchWithoutUnhandledRejection(() => controls.autoLaunch!.onChange(false));

      expect(agentConfigSet).toHaveBeenCalledWith({
        selected: "omp",
        customCommand: "",
        autoLaunch: false,
      });
      expect(autoLaunchEnabled()).toBe(true);
      expect(unhandled).toEqual([]);
      expect(toasts).toEqual([{ message: "Auto-launch save failed: auto denied", kind: "err" }]);
    } finally {
      mounted.dispose();
    }
  });
});

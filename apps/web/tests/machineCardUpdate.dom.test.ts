// MachineCard's row badge and MachineUpdateDetails' action are both decided by
// the ONE shared classifier (@roost/protocol/fleet-update). Bun has no browser
// DOM, so this suite uses the same client-Solid virtual renderer as the other
// DOM tests and stubs the M3 primitives that register browser custom elements.
// The renderer evaluates props once, so each state is its own render — which is
// also how the card behaves: the badge and the action come from one state read.

import { afterEach, beforeEach, describe, expect, mock, test } from "bun:test";
import type * as SolidApi from "solid-js";
import type { Worker } from "@roost/protocol/wire";
import type { MachineUpdateDetailsProps } from "../src/components/Settings/MachineUpdateDetails.tsx";

type VNode = {
  tag: unknown;
  props: Record<string, unknown>;
  rendered?: unknown;
};

const COORD_SHA = "a".repeat(40);
const WORKER_SHA = "b".repeat(40);
const FP = "c".repeat(64);

const solidClientUrl = new URL("./solid.js", import.meta.resolve("solid-js"));
const Solid = await import(solidClientUrl.href) as typeof SolidApi;
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

const ButtonStub = (props: Record<string, unknown>): unknown => props.children;
mock.module("../src/components/Settings/md/primitives.tsx", () => ({
  Button: ButtonStub,
  Chip: (props: Record<string, unknown>) => props.label,
  Icon: () => null,
  ListRow: (props: Record<string, unknown>) => [props.headline, props.support, props.trailing],
  MetricTile: () => null,
  StatusDot: () => null,
  TextField: () => null,
}));

const onlineFps = new Set<string>();
mock.module("../src/store/sync.ts", () => ({
  workerOnline: (worker: Worker) => onlineFps.has(worker.fp),
}));
mock.module("../src/store/root.ts", () => ({
  rootStore: { coord_identity: { git_sha: COORD_SHA, public_url: "" }, workers: {} },
  deleteStoreRecord: () => undefined,
}));
mock.module("../src/store/toastStore.ts", () => ({ addToast: () => () => undefined }));

type DeployStartRequest = { host: string; expectedGitSha?: string };
type DeployStartResponse = { ok: boolean; jobId: string; error: string };

const startRequests: DeployStartRequest[] = [];
let startResponse: (request: DeployStartRequest) => Promise<DeployStartResponse> = () =>
  new Promise<DeployStartResponse>(() => undefined);

mock.module("../src/connect.ts", () => ({
  coordClient: {
    workersDeployStart: (request: DeployStartRequest) => {
      startRequests.push(request);
      return startResponse(request);
    },
    workersDeployOutput: () => (async function* () {
      yield { kind: "done", text: "", exit: 0, error: "" };
    })(),
  },
}));

// The card and its deploy owner bind coordClient and the store at module
// evaluation time, so both imports must follow the mocks above.
const { MachineCard } = await import("../src/components/Settings/MachineCard.tsx");
const { MachineUpdateDetails } = await import(
  "../src/components/Settings/MachineUpdateDetails.tsx"
);
const { _resetMachineDeploys, machineDeployInFlight, startMachineUpdateDeploy } = await import(
  "../src/components/Settings/machine-update-deploy.ts"
);

function makeWorker(gitSha: string | null): Worker {
  return {
    fp: FP,
    label: "workshop",
    os: "linux",
    git_sha: gitSha,
    host_metrics: null,
    registered_at_ms: 1,
    last_seen_ms: Date.now(),
    reachable_addr: "100.64.0.2",
    keeper_runtime: null,
    terminal_core_capacity: null,
  } as Worker;
}

function resolvedNode(node: unknown): unknown {
  let resolved = node;
  while (typeof resolved === "function") resolved = resolved();
  return resolved;
}

/** Every vnode Solid would actually paint: <Show> is resolved through its memo,
 *  so a branch whose condition is false contributes nothing. */
function collectRendered(node: unknown, output: VNode[] = []): VNode[] {
  const resolved = resolvedNode(node);
  if (Array.isArray(resolved)) {
    for (const child of resolved) collectRendered(child, output);
    return output;
  }
  if (!resolved || typeof resolved !== "object") return output;
  const vnode = resolved as VNode;
  output.push(vnode);
  if (typeof vnode.tag === "function") collectRendered(vnode.rendered, output);
  else collectRendered(vnode.props.children, output);
  return output;
}

function collectText(node: unknown, output: string[] = []): string[] {
  const resolved = resolvedNode(node);
  if (typeof resolved === "string" || typeof resolved === "number") {
    output.push(String(resolved));
    return output;
  }
  if (Array.isArray(resolved)) {
    for (const child of resolved) collectText(child, output);
    return output;
  }
  if (!resolved || typeof resolved !== "object") return output;
  const vnode = resolved as VNode;
  if (typeof vnode.tag === "function") collectText(vnode.rendered, output);
  else collectText(vnode.props.children, output);
  return output;
}

/** Every vnode the render CREATED, memos unresolved — the only way to see the
 *  props a collapsed section was handed. */
function collectCreated(node: unknown, output: VNode[] = []): VNode[] {
  if (Array.isArray(node)) {
    for (const child of node) collectCreated(child, output);
    return output;
  }
  if (!node || typeof node !== "object") return output;
  const vnode = node as VNode;
  output.push(vnode);
  return collectCreated(vnode.props.children, output);
}

interface RenderedSurface {
  text: () => string;
  button: (testId: string) => Record<string, unknown> | undefined;
}

const disposers: Array<() => void> = [];

function render<Props>(component: (props: Props) => unknown, props: Props): {
  tree: unknown;
  surface: RenderedSurface;
} {
  let tree: unknown;
  Solid.createRoot((dispose) => {
    disposers.push(dispose);
    tree = component(props);
  });
  return {
    tree,
    surface: {
      text: () => collectText(tree).join(" ").replace(/\s+/g, " ").trim(),
      button: (testId: string) =>
        collectRendered(tree)
          .find((vnode) => vnode.tag === ButtonStub && vnode.props["data-testid"] === testId)
          ?.props,
    },
  };
}

/** The update surface as the card drives it: the card classifies, this renders
 *  the exact props it handed over. The card's details section starts collapsed,
 *  so the handoff is read from the created tree. */
function renderUpdateSurface(worker: Worker): RenderedSurface {
  const card = render(MachineCard, { worker });
  const handoff = collectCreated(card.tree)
    .find((vnode) => vnode.tag === MachineUpdateDetails)!
    .props as MachineUpdateDetailsProps;
  return render(MachineUpdateDetails, handoff).surface;
}

function renderRow(worker: Worker): RenderedSurface {
  return render(MachineCard, { worker }).surface;
}

beforeEach(() => {
  _resetMachineDeploys();
  onlineFps.clear();
  onlineFps.add(FP);
  startRequests.length = 0;
  startResponse = () => new Promise<DeployStartResponse>(() => undefined);
});

afterEach(() => {
  while (disposers.length > 0) disposers.pop()?.();
});

describe("MachineCard update affordance", () => {
  test("a machine on the coordinator's release reads up to date and offers no update", () => {
    const worker = makeWorker(COORD_SHA);

    expect(renderRow(worker).text()).toContain("Up to date");
    const surface = renderUpdateSurface(worker);
    expect(surface.button(`machines-update-btn-${FP}`)).toBeUndefined();
    expect(surface.text()).toBe("");
  });

  test("an online machine behind the coordinator offers an enabled update", () => {
    const worker = makeWorker(WORKER_SHA);

    expect(renderRow(worker).text()).toContain("Update available");
    const surface = renderUpdateSurface(worker);
    expect(surface.text()).toContain("Update");
    expect(surface.button(`machines-update-btn-${FP}`)?.disabled).toBe(false);
  });

  test("the update button starts the host's deploy job and the row then reads Updating…", () => {
    const worker = makeWorker(WORKER_SHA);
    const surface = renderUpdateSurface(worker);

    (surface.button(`machines-update-btn-${FP}`)!.onClick as () => void)();

    expect(startRequests).toEqual([{ host: FP, expectedGitSha: COORD_SHA }]);
    expect(machineDeployInFlight(FP)).toBe(true);

    const rowText = renderRow(worker).text();
    expect(rowText).toContain("Updating…");
    expect(rowText).not.toContain("Update available");
    expect(renderUpdateSurface(worker).button(`machines-update-btn-${FP}`)?.disabled).toBe(true);
  });

  test("an offline machine behind the coordinator defers instead of offering a button", () => {
    onlineFps.clear();
    const worker = makeWorker(WORKER_SHA);

    expect(renderRow(worker).text()).toContain("Update pending — offline");
    const surface = renderUpdateSurface(worker);
    // The copy must promise the coordinator's own retry AND name the one case
    // it cannot clear by itself; the exact sentence is not the contract.
    expect(surface.text()).toContain("when it reconnects");
    expect(surface.text()).toContain("keeper-refresh");
    expect(surface.button(`machines-update-btn-${FP}`)).toBeUndefined();
  });

  test("a machine with no reported version reads unknown and offers no update", () => {
    const worker = makeWorker(null);

    expect(renderRow(worker).text()).toContain("Version unknown");
    expect(renderUpdateSurface(worker).button(`machines-update-btn-${FP}`)).toBeUndefined();
  });
});

describe("machine deploy job ownership", () => {
  test("a refused start returns the coordinator's reason and releases the in-flight record", async () => {
    startResponse = async () => ({ ok: false, jobId: "", error: "worker not found" });

    const failure = await startMachineUpdateDeploy(FP, COORD_SHA);

    expect(failure).toBe("worker not found");
    expect(machineDeployInFlight(FP)).toBe(false);
  });

  test("a completed job reports success and releases the in-flight record", async () => {
    startResponse = async () => ({ ok: true, jobId: "job-1", error: "" });

    const failure = await startMachineUpdateDeploy(FP, COORD_SHA);

    expect(failure).toBeNull();
    expect(machineDeployInFlight(FP)).toBe(false);
  });
});

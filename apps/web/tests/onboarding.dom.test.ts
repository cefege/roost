// Onboarding DOM coverage for the unauthorized pairing gate's hierarchy: one
// primary Request approval before the collapsed Other pairing options, secrets
// and recovery hidden until asked for, and the approver list kept for authorized
// browsers. Uses the re-rendering client-Solid runtime and shared primitive stubs.

import { afterEach, describe, expect, mock, test } from "bun:test";
import type * as SolidApi from "solid-js";
import type { PairPollStatus } from "../src/components/OnboardingRequestCard.tsx";
import type { PairRequest } from "../src/store/root.ts";
import { pairingPrimitiveStubs } from "./helpers/pairingPrimitiveStubs.ts";
import { createRerenderingSolid } from "./helpers/rerenderingSolid.ts";

type VNode = { tag: unknown; props: Record<string, unknown>; rendered?: unknown };

const REQUEST_ID = "0123456789abcdef0123456789abcdef";

// Load client Solid after the module mocks select the browser-safe runtime.
const Solid = await import(new URL("./solid.js", import.meta.resolve("solid-js")).href) as typeof SolidApi;
const rerendering = createRerenderingSolid(Solid);
mock.module("solid-js", () => rerendering.runtime);

function createElement(tag: unknown, props: Record<string, unknown> | null, ...children: unknown[]): VNode {
  const merged = { ...(props ?? {}) };
  if (children.length > 0) merged.children = children.length === 1 ? children[0] : children;
  const vnode: VNode = { tag, props: merged };
  // A plain call keeps nested component bodies inside mount()'s tracking scope,
  // so their local disclosure state re-renders the tree.
  if (typeof tag === "function") vnode.rendered = tag(merged);
  return vnode;
}

const ReactShim = { Fragment: Symbol("Fragment"), createElement };
(globalThis as typeof globalThis & { React: unknown }).React = ReactShim;
mock.module("react/jsx-dev-runtime", () => ({
  Fragment: ReactShim.Fragment,
  jsxDEV(tag: unknown, props: Record<string, unknown> | null): VNode {
    const children = props?.children === undefined ? [] : [props.children];
    return createElement(tag, props, ...children);
  },
}));

mock.module("../src/components/Settings/md/primitives.tsx", () => pairingPrimitiveStubs);
mock.module("../src/components/Onboarding.css", () => ({}));
mock.module("../src/components/PairingStatusNotice.css", () => ({}));

const rootStore = {
  browser_access_state: "unauthorized" as "checking" | "authorized" | "unauthorized",
  workers: {} as Record<string, unknown>,
  pair_requests: {} as Record<string, PairRequest>,
};
mock.module("../src/store/root.ts", () => ({ rootStore }));
mock.module("../src/store/mutations.ts", () => ({ deletePairRequest: () => undefined }));
mock.module("../src/store/toastStore.ts", () => ({ addToast: () => undefined }));
mock.module("../src/connect.ts", () => ({ coordClient: {} }));
mock.module("../src/lib/overlayMotion.ts", () => ({ animateOverlayPanel: () => undefined }));
mock.module("../src/auth/redeemPairToken.ts", () => ({
  redeemPairToken: async () => ({ ok: false, error: "unused" }),
}));

let resetEligibilityProbes = 0;
let deviceRejected = false;
mock.module("../src/auth/web-key.ts", () => ({
  isResetWebKeyEligible: async () => {
    resetEligibilityProbes += 1;
    return deviceRejected;
  },
  resetWebKey: async () => undefined,
}));

mock.module("../src/components/PairApprovalProvider.tsx", () => ({
  usePairApproval: () => ({ busyRequestId: () => null, approve: async () => undefined }),
}));

const requesterState = {
  ephemeralId: null as string | null,
  pollStatus: "idle" as PairPollStatus,
  requestError: null as string | null,
};
mock.module("../src/components/PairingRequesterProvider.tsx", () => ({
  usePairingRequester: () => ({
    busy: () => false,
    confirmationError: () => null,
    ephemeralId: () => requesterState.ephemeralId,
    pollStatus: () => requesterState.pollStatus,
    verificationCode: () => "",
    requestError: () => requesterState.requestError,
    clear: () => undefined,
    clearRequestError: () => undefined,
    confirm: async () => undefined,
    start: async () => undefined,
    updateVerificationCode: () => undefined,
  }),
}));

// The component imports follow every module mock above.
const { Onboarding } = await import("../src/components/Onboarding.tsx");

const disposers: Array<() => void> = [];

function resolveNode(node: unknown): unknown {
  let resolved = node;
  while (typeof resolved === "function") resolved = resolved();
  return resolved;
}

/** Vnodes Solid would paint, in document order; untaken <Show> branches are skipped. */
function paintedNodes(node: unknown, output: VNode[] = []): VNode[] {
  const resolved = resolveNode(node);
  if (Array.isArray(resolved)) {
    for (const child of resolved) paintedNodes(child, output);
    return output;
  }
  if (!resolved || typeof resolved !== "object") {
    if (typeof resolved === "string" || typeof resolved === "number") {
      output.push({ tag: "#text", props: { text: String(resolved) } });
    }
    return output;
  }
  const vnode = resolved as VNode;
  output.push(vnode);
  paintedNodes(typeof vnode.tag === "function" ? vnode.rendered : vnode.props.children, output);
  return output;
}

function mountOnboarding() {
  const mounted = rerendering.mount(() => Onboarding());
  disposers.push(mounted.dispose);
  const nodes = () => paintedNodes(mounted.current());
  const testIds = () => nodes()
    .map((vnode) => vnode.props["data-testid"] ?? vnode.props.testId)
    .filter((id): id is string => typeof id === "string");
  const text = () => nodes()
    .filter((vnode) => vnode.tag === "#text")
    .map((vnode) => vnode.props.text)
    .join(" ");
  const clickTestId = (testId: string) => {
    const target = nodes().find((vnode) => vnode.props["data-testid"] === testId);
    (target?.props.onClick as () => void)();
  };
  return { testIds, text, clickTestId };
}

async function settleProbe(): Promise<void> {
  for (let tick = 0; tick < 5; tick += 1) await Promise.resolve();
}

afterEach(() => {
  while (disposers.length > 0) disposers.pop()?.();
  rootStore.browser_access_state = "unauthorized";
  rootStore.pair_requests = {};
  requesterState.ephemeralId = null;
  requesterState.pollStatus = "idle";
  requesterState.requestError = null;
  resetEligibilityProbes = 0;
  deviceRejected = false;
});

describe("Onboarding pairing gate", () => {
  test("offers Request approval before the collapsed Other pairing options", () => {
    const ids = mountOnboarding().testIds();

    const primary = ids.indexOf("onboarding-pair-start-btn");
    const optionsToggle = ids.indexOf("pairing-other-options-toggle");
    expect(primary).toBeGreaterThanOrEqual(0);
    expect(optionsToggle).toBeGreaterThan(primary);
    expect(ids).not.toContain("pairing-other-options-panel");
  });

  test("renders the setup token only after the options expand", () => {
    const page = mountOnboarding();
    expect(page.testIds()).not.toContain("onboarding-setup-token-input");

    page.clickTestId("pairing-other-options-toggle");
    expect(page.testIds()).toContain("pairing-other-options-panel");
    expect(page.testIds()).toContain("onboarding-setup-token-input");

    page.clickTestId("pairing-other-options-toggle");
    expect(page.testIds()).not.toContain("onboarding-setup-token-input");
  });

  test("probes key rejection only on first expand and offers recovery only for a rejected key", async () => {
    deviceRejected = true;
    const page = mountOnboarding();
    await settleProbe();
    expect(resetEligibilityProbes).toBe(0);

    page.clickTestId("pairing-other-options-toggle");
    await settleProbe();
    expect(resetEligibilityProbes).toBe(1);
    expect(page.testIds()).toContain("onboarding-reset-key-btn");
    page.clickTestId("pairing-other-options-toggle");
    page.clickTestId("pairing-other-options-toggle");
    await settleProbe();
    expect(resetEligibilityProbes).toBe(1);

    deviceRejected = false;
    const accepted = mountOnboarding();
    accepted.clickTestId("pairing-other-options-toggle");
    await settleProbe();
    expect(accepted.testIds()).toContain("onboarding-setup-token-input");
    expect(accepted.testIds()).not.toContain("onboarding-reset-key-btn");
  });

  test("hides the request ID while pending and the code input until approval", () => {
    requesterState.ephemeralId = REQUEST_ID;
    requesterState.pollStatus = "pending";
    const pending = mountOnboarding();
    expect(pending.text()).not.toContain(REQUEST_ID);
    expect(pending.testIds()).not.toContain("onboarding-pair-verification-input");
    expect(pending.testIds()).not.toContain("onboarding-pair-confirm");

    requesterState.pollStatus = "verification_required";
    const approved = mountOnboarding();
    expect(approved.text()).not.toContain(REQUEST_ID);
    expect(approved.testIds()).toContain("onboarding-pair-verification-input");
    expect(approved.testIds()).toContain("onboarding-pair-confirm");
  });

  test("reports a terminal request failure once, through the page alert", () => {
    requesterState.ephemeralId = REQUEST_ID;
    requesterState.pollStatus = "error";
    requesterState.requestError = "Pair poll failed: unavailable";
    const ids = mountOnboarding().testIds();

    expect(ids).toContain("onboarding-request-error");
    expect(ids).not.toContain("onboarding-pair-poll-status");
  });

  test("keeps the approval list, not the gate, for an authorized browser", () => {
    rootStore.browser_access_state = "authorized";
    rootStore.pair_requests = {
      [REQUEST_ID]: {
        ephemeral_id: REQUEST_ID,
        label: "Requester",
        created_at_ms: Date.now(),
        userAgent: "",
        clientBrowser: "Chrome",
        clientOs: "macOS",
        clientDeviceType: "desktop",
        sourceIp: "",
        countryCode: "",
        region: "",
        city: "",
        edgeIdentityProvider: "",
        edgeIdentity: "",
        edgeIdentityVerified: false,
        expiresAtMs: Date.now() + 60_000,
      },
    };
    const ids = mountOnboarding().testIds();

    expect(ids).toContain("pair-approval-list");
    expect(ids).toContain("pair-card-approve");
    expect(ids).not.toContain("onboarding-pair-start-btn");
    expect(ids).not.toContain("pairing-other-options-toggle");
  });
});

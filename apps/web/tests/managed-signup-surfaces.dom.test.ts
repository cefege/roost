// Managed account-entry components must apply gateway policy before mounting provider effects.
// Bun uses Solid's server build by default, so this harness supplies the client runtime and a
// minimal virtual JSX renderer to observe conditional subtrees and real mount effects.

import { afterEach, beforeEach, describe, expect, mock, test } from "bun:test";
import type * as SolidApi from "solid-js";
import { MANAGED_SIGNUP_UNAVAILABLE_MESSAGE } from "../src/auth/managed-signup-policy.ts";

type AuthConfig = {
  signupEnabled: boolean;
  googleEnabled: boolean;
  turnstileSiteKey: string;
};
type VNode = {
  tag: unknown;
  props: Record<string, unknown>;
  rendered?: unknown;
};

let currentConfig: AuthConfig = {
  signupEnabled: false,
  googleEnabled: false,
  turnstileSiteKey: "",
};
let capturedCredential: { kind: "email-signup"; token: string } | null = null;
let configRequests = 0;
let turnstileMounts = 0;
let scriptAppends = 0;
let emailStarts = 0;
let googleStarts = 0;
let emailVerifications = 0;
let resultPolls = 0;
let activations = 0;
let assignedLocations: string[] = [];
const disposedRoots: Array<() => void> = [];

const solidClientUrl = new URL("./solid.js", import.meta.resolve("solid-js"));
const Solid = await import(solidClientUrl.href) as typeof SolidApi;

function synchronousResource<T>(
  sourceOrFetcher: (() => unknown) | undefined,
  fetcher?: (source: unknown) => T,
): unknown {
  let value: T | undefined;
  let error: unknown;
  try {
    if (fetcher) {
      const source = sourceOrFetcher?.();
      if (source !== false && source !== null && source !== undefined) value = fetcher(source);
    } else {
      value = sourceOrFetcher?.() as T | undefined;
    }
  } catch (caught) {
    error = caught;
  }
  const accessor = (() => value) as (() => T | undefined) & {
    loading: boolean;
    error: unknown;
  };
  accessor.loading = false;
  accessor.error = error;
  return [accessor, {}];
}

mock.module("solid-js", () => ({
  ...Solid,
  createResource: synchronousResource,
}));

function invokeComponent(vnode: VNode): void {
  if (typeof vnode.tag !== "function") return;
  const component = vnode.tag;
  const owner = Solid.getOwner();
  vnode.rendered = Solid.runWithOwner(owner, () => component(vnode.props));
}

function createElement(
  tag: unknown,
  props: Record<string, unknown> | null,
  ...children: unknown[]
): VNode {
  const merged = { ...(props ?? {}) };
  if (children.length > 0) merged.children = children.length === 1 ? children[0] : children;
  const vnode = { tag, props: merged };
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

Object.defineProperty(globalThis, "document", {
  configurable: true,
  value: {
    head: { append: () => { scriptAppends += 1; } },
    querySelector: () => null,
    createElement: () => ({}),
  },
});
Object.defineProperty(globalThis, "location", {
  configurable: true,
  value: {
    assign: (href: string) => { assignedLocations.push(href); },
    replace: () => undefined,
  },
});

class FakeGatewayError extends Error {
  constructor(readonly status: number, readonly code: string | null = null) {
    super("managed auth gateway request failed");
  }
}

mock.module("../src/auth/managed-auth-gateway.ts", () => ({
  ManagedAuthGatewayError: FakeGatewayError,
  getManagedAuthConfig: () => {
    configRequests += 1;
    return currentConfig;
  },
  startManagedEmailSignup: async () => { emailStarts += 1; },
  startManagedGoogle: async () => {
    googleStarts += 1;
    return "https://accounts.google.com/o/oauth2/v2/auth";
  },
  verifyManagedEmailSignup: async () => { emailVerifications += 1; },
  getManagedAuthResult: async () => {
    resultPolls += 1;
    return { state: "capacity" };
  },
}));
mock.module("../src/auth/fragment-credential.ts", () => ({
  peekCapturedFragmentCredential: () => capturedCredential,
  markCapturedEmailSignupSubmitted: () => true,
  clearCapturedFragmentCredential: () => true,
}));
mock.module("../src/auth/managed-auth-progress.ts", () => ({
  clearManagedEmailSignupActivationProgress: () => undefined,
  clearManagedGoogleCompletionProgress: () => undefined,
}));
mock.module("../src/auth/managed-signup-verify.ts", () => ({
  activateManagedEmailSignup: async () => { activations += 1; },
}));
mock.module("../src/auth/managed-account.ts", () => ({
  isAuthoritativeCredentialDenial: () => false,
  managedActivationErrorMessage: () => "activation failed",
  managedNewPasswordIssue: () => null,
  managedNewPasswordIssueMessage: () => "invalid password",
}));
mock.module("../src/components/ManagedAuthLayout.tsx", () => ({
  ManagedAuthLayout: (props: Record<string, unknown>) => createElement(
    "main",
    { "data-testid": props.testId },
    createElement("h1", null, props.title),
    createElement("p", null, props.description),
    props.children,
  ),
}));
mock.module("../src/components/Settings/md/Button.tsx", () => ({
  Button: (props: Record<string, unknown>) => createElement("button", props, props.children),
}));
mock.module("../src/components/Settings/md/Dialog.tsx", () => ({
  Dialog: (props: Record<string, unknown>) => createElement(
    "div",
    { "data-testid": props.testId },
    props.children,
  ),
}));
mock.module("../src/components/Settings/md/TextField.tsx", () => ({
  TextField: (props: Record<string, unknown>) => createElement("input", {
    "data-testid": props.testId,
    ...props,
    onInput: (event: { currentTarget: { value: string } }) => {
      const onInput = props.onInput;
      if (typeof onInput === "function") onInput(event.currentTarget.value);
    },
  }),
}));
mock.module("../src/components/TurnstileWidget.tsx", () => ({
  TurnstileWidget: (props: Record<string, unknown>) => {
    Solid.onMount(() => {
      turnstileMounts += 1;
      const onToken = props.onToken;
      if (typeof onToken !== "function") throw new Error("Turnstile mock requires onToken");
      onToken("turnstile-token");
    });
    return createElement("div", { "data-testid": "managed-turnstile" });
  },
}));
mock.module("@solidjs/router", () => ({ useNavigate: () => () => undefined }));
mock.module("../src/auth/managed-login.ts", () => ({
  loginManagedBrowser: async () => undefined,
  managedLoginErrorMessage: () => "login failed",
}));
mock.module("../src/auth/web-key.ts", () => ({
  isResetWebKeyEligible: async () => false,
  resetWebKey: async () => undefined,
}));
mock.module("../src/store/root.ts", () => ({
  rootStore: { browser_unauthorized: false, coord_identity: { saas_mode: true } },
  hasConfirmedDashboardAccess: () => false,
}));

// Static imports would bind production providers before the component seams above are mocked.
const { ManagedLogin } = await import("../src/components/ManagedLogin.tsx");
const { ManagedSignup } = await import("../src/components/ManagedSignup.tsx");
const { ManagedSignupEnabled } = await import("../src/components/ManagedSignupEnabled.tsx");
const { ManagedSignupVerifyEnabled } = await import("../src/components/ManagedSignupVerifyEnabled.tsx");
const { ManagedSignupVerify } = await import("../src/components/ManagedSignupVerify.tsx");

function resolvedNode(node: unknown): unknown {
  let resolved = node;
  while (typeof resolved === "function") resolved = resolved();
  return resolved;
}

function visit(node: unknown, visitor: (vnode: VNode) => boolean): VNode | undefined {
  const resolved = resolvedNode(node);
  if (Array.isArray(resolved)) {
    for (const child of resolved) {
      const match = visit(child, visitor);
      if (match) return match;
    }
    return undefined;
  }
  if (!resolved || typeof resolved !== "object") return undefined;
  const vnode = resolved as VNode;
  if (visitor(vnode)) return vnode;
  return visit(typeof vnode.tag === "function" ? vnode.rendered : vnode.props.children, visitor);
}

function findByTestId(tree: unknown, testId: string): VNode | undefined {
  return visit(tree, (vnode) => vnode.props?.["data-testid"] === testId);
}

function findByHref(tree: unknown, href: string): VNode | undefined {
  return visit(tree, (vnode) => vnode.props?.href === href);
}

function collectText(node: unknown, output: string[] = []): string[] {
  const resolved = resolvedNode(node);
  if (typeof resolved === "string") {
    output.push(resolved);
    return output;
  }
  if (Array.isArray(resolved)) {
    for (const child of resolved) collectText(child, output);
    return output;
  }
  if (!resolved || typeof resolved !== "object") return output;
  const vnode = resolved as VNode;
  collectText(typeof vnode.tag === "function" ? vnode.rendered : vnode.props.children, output);
  return output;
}

function mount(component: () => unknown): unknown {
  let tree: unknown;
  Solid.createRoot((dispose) => {
    disposedRoots.push(dispose);
    tree = component();
    collectText(tree);
  });
  return tree;
}


beforeEach(() => {
  currentConfig = { signupEnabled: false, googleEnabled: false, turnstileSiteKey: "" };
  capturedCredential = null;
  configRequests = 0;
  turnstileMounts = 0;
  scriptAppends = 0;
  emailStarts = 0;
  googleStarts = 0;
  emailVerifications = 0;
  resultPolls = 0;
  activations = 0;
  assignedLocations = [];
});

afterEach(() => {
  while (disposedRoots.length > 0) disposedRoots.pop()?.();
});

describe("managed signup route surfaces", () => {
  test("both disabled direct routes are inert and render the exact operator copy", async () => {
    currentConfig = {
      signupEnabled: false,
      googleEnabled: true,
      turnstileSiteKey: "stale-site-key",
    };
    capturedCredential = { kind: "email-signup", token: "v".repeat(43) };

    const signup = mount(ManagedSignup);
    const verify = mount(ManagedSignupVerify);
    await Promise.resolve();

    expect(collectText(signup)).toContain(MANAGED_SIGNUP_UNAVAILABLE_MESSAGE);
    expect(collectText(verify)).toContain(MANAGED_SIGNUP_UNAVAILABLE_MESSAGE);
    expect(findByTestId(signup, "managed-signup-email")).toBeUndefined();
    expect(findByTestId(signup, "managed-signup-google")).toBeUndefined();
    expect(findByTestId(signup, "managed-turnstile")).toBeUndefined();
    expect(findByTestId(verify, "managed-signup-activate")).toBeUndefined();
    expect(configRequests).toBe(2);
    expect({
      turnstileMounts,
      scriptAppends,
      emailStarts,
      googleStarts,
      emailVerifications,
      resultPolls,
      activations,
    }).toEqual({
      turnstileMounts: 0,
      scriptAppends: 0,
      emailStarts: 0,
      googleStarts: 0,
      emailVerifications: 0,
      resultPolls: 0,
      activations: 0,
    });
  });

  test("enabled signup still mounts verification and dispatches both enrollment actions", async () => {
    currentConfig = { signupEnabled: true, googleEnabled: true, turnstileSiteKey: "site-key" };
    const tree = mount(() => ManagedSignupEnabled({ config: currentConfig }));
    await Promise.resolve();

    expect(turnstileMounts).toBe(1);
    expect(findByTestId(tree, "managed-signup-email")).toBeDefined();
    expect(findByTestId(tree, "managed-signup-google")).toBeDefined();
    const email = findByTestId(tree, "managed-signup-email");
    const form = visit(tree, (vnode) => vnode.tag === "form");
    const onInput = email?.props.onInput;
    const onSubmit = form?.props.onSubmit;
    if (typeof onInput !== "function" || typeof onSubmit !== "function") {
      throw new Error("enabled signup form did not render");
    }
    onInput({ currentTarget: { value: "owner@example.test" } });
    onSubmit({ preventDefault: () => undefined });
    await Promise.resolve();
    await Promise.resolve();
    expect(emailStarts).toBe(1);

    const google = findByTestId(tree, "managed-signup-google");
    const onGoogleClick = google?.props.onClick;
    if (typeof onGoogleClick !== "function") throw new Error("enabled Google action did not render");
    onGoogleClick();
    await Promise.resolve();
    await Promise.resolve();
    expect(googleStarts).toBe(1);
    expect(assignedLocations).toEqual(["https://accounts.google.com/o/oauth2/v2/auth"]);
  });

  test("enabled verification mounts its credential-owned progress state", async () => {
    currentConfig = { signupEnabled: true, googleEnabled: false, turnstileSiteKey: "" };
    capturedCredential = { kind: "email-signup", token: "v".repeat(43) };
    const tree = mount(ManagedSignupVerifyEnabled);
    await Promise.resolve();

    expect(collectText(tree)).toContain("Verifying your email…");
  });
});

describe("managed login feature entries", () => {
  test("password login remains while Google and signup links follow gateway config", () => {
    let tree = mount(ManagedLogin);
    expect(findByTestId(tree, "managed-login-password")).toBeDefined();
    expect(findByTestId(tree, "managed-login-google")).toBeUndefined();
    expect(findByHref(tree, "/signup")).toBeUndefined();

    currentConfig = { signupEnabled: true, googleEnabled: true, turnstileSiteKey: "site-key" };
    tree = mount(ManagedLogin);
    expect(findByTestId(tree, "managed-login-google")).toBeDefined();
    expect(findByHref(tree, "/signup")).toBeDefined();
  });
});

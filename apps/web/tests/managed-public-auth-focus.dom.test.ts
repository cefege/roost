// Public managed-auth transitions must never strand keyboard focus on a removed control.
// Solid's universal renderer exposes the current loading, error, and success subtree.
// Focus assertions cover access, signup, login configuration, and reset acknowledgement.

import { afterEach, beforeEach, describe, expect, mock, test } from "bun:test";
import type * as SolidApi from "solid-js";
import {
  click,
  createSolidTestHarness,
  findButton,
  findByTag,
  findByTestId,
  textOf,
  type TestElement,
} from "./solid-universal-test-harness.ts";

interface Deferred<T> {
  promise: Promise<T>;
  resolve: (value: T) => void;
  reject: (error: unknown) => void;
}

interface AuthConfig {
  signupEnabled: boolean;
  googleEnabled: boolean;
  turnstileSiteKey: string;
}

function deferred<T>(): Deferred<T> {
  let resolve!: (value: T) => void;
  let reject!: (error: unknown) => void;
  const promise = new Promise<T>((resolvePromise, rejectPromise) => {
    resolve = resolvePromise;
    reject = rejectPromise;
  });
  return { promise, resolve, reject };
}

const solidClientUrl = new URL("./solid.js", import.meta.resolve("solid-js"));
const Solid = await import(solidClientUrl.href) as typeof SolidApi;
const harness = await createSolidTestHarness(Solid);
const { document: fakeDocument, element, renderer } = harness;
let assignedLocations: string[] = [];
Object.defineProperty(globalThis, "document", { configurable: true, value: fakeDocument });
Object.defineProperty(globalThis, "location", {
  configurable: true,
  value: { assign: (href: string) => { assignedLocations.push(href); }, replace: () => undefined },
});
Object.defineProperty(globalThis, "window", {
  configurable: true,
  value: { location: { assign: (href: string) => { assignedLocations.push(href); } } },
});

let authConfigRequests: Array<Deferred<AuthConfig>> = [];
let accessRequests: Array<Deferred<boolean>> = [];
let resetRequests: Array<Deferred<string>> = [];
let emailSignupRequests: Array<Deferred<void>> = [];
let googleSignupRequests: Array<Deferred<string>> = [];
const [dashboardAccess, setDashboardAccess] = Solid.createSignal(false);

class FakeGatewayError extends Error {
  readonly code = "unavailable";
}

mock.module("../src/auth/managed-auth-gateway.ts", () => ({
  ManagedAuthGatewayError: FakeGatewayError,
  getManagedAuthConfig: () => {
    const request = authConfigRequests.shift();
    if (!request) throw new Error("unexpected managed auth config request");
    return request.promise;
  },
  startManagedEmailSignup: () => {
    const request = emailSignupRequests.shift();
    if (!request) throw new Error("unexpected email signup request");
    return request.promise;
  },
  startManagedGoogle: () => {
    const request = googleSignupRequests.shift();
    if (!request) throw new Error("unexpected Google signup request");
    return request.promise;
  },
}));
mock.module("../src/store/dashboard-selection.ts", () => ({
  bootstrapDashboardAccess: () => {
    const request = accessRequests.shift();
    if (!request) throw new Error("unexpected dashboard access request");
    return request.promise.then((confirmed) => {
      if (confirmed) setDashboardAccess(true);
      return confirmed;
    });
  },
}));
mock.module("../src/store/root.ts", () => ({
  rootStore: { browser_unauthorized: false, coord_identity: { saas_mode: true } },
  hasConfirmedDashboardAccess: () => dashboardAccess(),
}));
mock.module("../src/connect.ts", () => ({ classifyAuthFailure: () => "tenant" }));
mock.module("@solidjs/router", () => ({
  Navigate: (props: Record<string, unknown>) => element("a", props),
  useLocation: () => ({ pathname: "/app" }),
  useNavigate: () => () => undefined,
}));
mock.module("../src/auth/managed-account.ts", () => ({
  MANAGED_PASSWORD_RESET_ACKNOWLEDGEMENT: "If the address is eligible, a reset link was sent.",
  requestManagedPasswordReset: () => {
    const request = resetRequests.shift();
    if (!request) throw new Error("unexpected password reset request");
    return request.promise;
  },
}));
mock.module("../src/auth/fragment-credential.ts", () => ({
  clearCapturedFragmentCredential: () => true,
}));
mock.module("../src/auth/managed-auth-progress.ts", () => ({
  clearManagedGoogleCompletionProgress: () => undefined,
}));
mock.module("../src/auth/managed-login.ts", () => ({
  loginManagedBrowser: async () => undefined,
  managedLoginErrorMessage: () => "Sign-in failed.",
}));
mock.module("../src/auth/web-key.ts", () => ({
  isResetWebKeyEligible: async () => false,
  resetWebKey: async () => undefined,
}));
mock.module("../src/components/ManagedAuthLayout.tsx", () => ({
  ManagedAuthLayout: (props: Record<string, unknown>) => element("main", {
    "data-testid": props.testId,
    get children() { return props.children; },
  }),
}));
mock.module("../src/components/Settings/md/Button.tsx", () => ({
  Button: (props: Record<string, unknown>) => element("button", props),
}));
mock.module("../src/components/Settings/md/Dialog.tsx", () => ({
  Dialog: (props: Record<string, unknown>) => element("div", props),
}));
mock.module("../src/components/Settings/md/TextField.tsx", () => ({
  TextField: (props: Record<string, unknown>) => element("input", {
    ...props,
    "data-testid": props.testId,
  }),
}));
mock.module("../src/components/TurnstileWidget.tsx", () => ({
  TurnstileWidget: (props: Record<string, unknown>) => {
    Solid.onMount(() => (props.onToken as (token: string) => void)("challenge-token"));
    return element("div", { "data-testid": "managed-turnstile" });
  },
}));

// Static imports would bind production providers before this test installs its seams.
const { ManagedForgotPassword } = await import("../src/components/ManagedForgotPassword.tsx");
const { ManagedLogin } = await import("../src/components/ManagedLogin.tsx");
const { ManagedRouteGate } = await import("../src/components/ManagedRouteGate.tsx");
const { ManagedSignup } = await import("../src/components/ManagedSignup.tsx");
const { ManagedSignupEnabled, MANAGED_SIGNUP_PENDING_MESSAGE } = await import(
  "../src/components/ManagedSignupEnabled.tsx"
);

beforeEach(() => {
  authConfigRequests = [];
  accessRequests = [];
  resetRequests = [];
  emailSignupRequests = [];
  googleSignupRequests = [];
  assignedLocations = [];
  setDashboardAccess(false);
  fakeDocument.activeElement = null;
});

afterEach(() => harness.cleanup());

describe("managed public auth focus", () => {
  test("access retry transfers owned focus without stealing deliberately moved focus", async () => {
    const first = deferred<boolean>();
    const immediateFailure = deferred<boolean>();
    const recovered = deferred<boolean>();
    const laterRefresh = deferred<boolean>();
    accessRequests.push(first, immediateFailure, recovered, laterRefresh);
    const root = harness.mount(() => ManagedRouteGate({
      children: element("main", { "data-testid": "protected-main" }),
    } as never));
    await harness.settle();
    expect(fakeDocument.activeElement?.testId).toBe("managed-access-loading-status");

    first.reject(new TypeError("offline"));
    await harness.settle();
    let retry = findByTestId(root, "managed-access-retry");
    if (!retry) throw new Error("dashboard retry did not mount");
    expect(fakeDocument.activeElement).toBe(retry);

    immediateFailure.reject(new TypeError("immediate retry failure"));
    click(retry);
    await harness.settle();
    expect(retry.isConnected).toBe(false);
    retry = findByTestId(root, "managed-access-retry")!;
    expect(fakeDocument.activeElement).toBe(retry);

    click(retry);
    await harness.settle();
    expect(fakeDocument.activeElement?.testId).toBe("managed-access-loading-status");
    recovered.resolve(true);
    await harness.settle();
    expect(fakeDocument.activeElement?.testId).toBe("protected-main");

    const externalControl = element("button", { "data-testid": "external-control" });
    renderer.insertNode(root, externalControl);
    setDashboardAccess(false);
    externalControl.focus();
    await harness.settle();
    expect(fakeDocument.activeElement).toBe(externalControl);
    laterRefresh.resolve(true);
    await harness.settle();
    expect(fakeDocument.activeElement).toBe(externalControl);
  });

  test("signup policy recovery focuses the newly mounted email field", async () => {
    const failed = deferred<AuthConfig>();
    const recovered = deferred<AuthConfig>();
    authConfigRequests.push(failed, recovered);
    const root = harness.mount(ManagedSignup);
    failed.reject(new TypeError("offline"));
    await harness.settle();

    const retry = findButton(root, "Try again");
    if (!retry) throw new Error("signup retry did not mount");
    retry.focus();
    click(retry);
    expect(retry.isConnected).toBe(false);
    recovered.resolve({ signupEnabled: true, googleEnabled: true, turnstileSiteKey: "site-key" });
    await harness.settle();
    expect(fakeDocument.activeElement?.testId).toBe("managed-signup-email");
  });

  test("signup attempts keep focus on one live status through error and success", async () => {
    const googleFailure = deferred<string>();
    const emailSuccess = deferred<void>();
    googleSignupRequests.push(googleFailure);
    emailSignupRequests.push(emailSuccess);
    const root = harness.mount(() => ManagedSignupEnabled({
      config: { signupEnabled: true, googleEnabled: true, turnstileSiteKey: "site-key" },
    }));
    await harness.settle();
    const message = findByTestId(root, "managed-signup-message");
    const google = findByTestId(root, "managed-signup-google");
    if (!message || !google) throw new Error("signup actions did not mount");

    google.focus();
    click(google);
    expect(fakeDocument.activeElement).toBe(message);
    googleFailure.reject(new TypeError("offline"));
    await harness.settle();
    expect(message.isConnected).toBe(true);
    expect(fakeDocument.activeElement).toBe(message);

    const submit = findByTestId(root, "managed-signup-email-submit");
    const form = findByTag(root, "form");
    if (!submit || !form) throw new Error("email signup form did not mount");
    submit.focus();
    (form.properties.onSubmit as (event: { preventDefault: () => void }) => void)({
      preventDefault: () => undefined,
    });
    expect(fakeDocument.activeElement).toBe(message);
    emailSuccess.resolve(undefined);
    await harness.settle();
    expect(textOf(message)).toBe(MANAGED_SIGNUP_PENDING_MESSAGE);
    expect(fakeDocument.activeElement).toBe(message);
    expect(assignedLocations).toEqual([]);
  });

  test("password reset replaces its submit control with a focused acknowledgement", async () => {
    const request = deferred<string>();
    resetRequests.push(request);
    const root = harness.mount(ManagedForgotPassword);
    const submit = findByTestId(root, "managed-forgot-password-submit");
    const form = findByTag(root, "form");
    if (!submit || !form) throw new Error("password reset form did not mount");
    submit.focus();
    (form.properties.onSubmit as (event: { preventDefault: () => void }) => void)({
      preventDefault: () => undefined,
    });
    request.resolve("Check your inbox if this address is eligible.");
    await harness.settle();
    const acknowledgement = findByTestId(root, "managed-forgot-password-ack");
    if (!acknowledgement) throw new Error("password reset acknowledgement did not mount");
    expect(submit.isConnected).toBe(false);
    expect(fakeDocument.activeElement).toBe(acknowledgement);
  });

  test("login config retry keeps focus through loading, repeated error, and success", async () => {
    const first = deferred<AuthConfig>();
    const repeatedFailure = deferred<AuthConfig>();
    const recovered = deferred<AuthConfig>();
    authConfigRequests.push(first, repeatedFailure, recovered);
    const root = harness.mount(ManagedLogin);
    first.reject(new TypeError("offline"));
    await harness.settle();
    let retry = findByTestId(root, "managed-login-config-retry");
    if (!retry) throw new Error("login config retry did not mount");

    retry.focus();
    click(retry);
    expect(fakeDocument.activeElement?.testId).toBe("managed-login-config-status");
    repeatedFailure.reject(new TypeError("still offline"));
    await harness.settle();
    retry = findByTestId(root, "managed-login-config-retry")!;
    expect(fakeDocument.activeElement).toBe(retry);

    click(retry);
    expect(fakeDocument.activeElement?.testId).toBe("managed-login-config-status");
    recovered.resolve({ signupEnabled: false, googleEnabled: false, turnstileSiteKey: "" });
    await harness.settle();
    expect(fakeDocument.activeElement?.testId).toBe("managed-login-email");
  });
});

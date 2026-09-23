// Shared fixture for the PairApprovalProvider DOM suites. Importing it installs
// the tab storage, protocol, RPC, toast, and dialog mocks; the loader then swaps
// in the RPC client and client Solid before the provider graph loads. Mounting
// renders the provider under a JSX shim so a suite observes the code dialog,
// the tab record, RPC calls, and toasts.

import { mock } from "bun:test";
import type * as SolidApi from "solid-js";
import type { PairApprovalContextValue } from "../../src/components/PairApprovalProvider.tsx";

export const REQUEST_ID = "0123456789abcdef0123456789abcdef";
export const REQUEST_CODE = "654321";
export const sessionValues = new Map<string, string>();
const originalSessionStorage = Object.getOwnPropertyDescriptor(globalThis, "sessionStorage");

Object.defineProperty(globalThis, "sessionStorage", {
  configurable: true,
  value: {
    get length() { return sessionValues.size; },
    clear: () => sessionValues.clear(),
    getItem: (key: string) => sessionValues.get(key) ?? null,
    key: (index: number) => [...sessionValues.keys()][index] ?? null,
    removeItem: (key: string) => { sessionValues.delete(key); },
    setItem: (key: string, value: string) => { sessionValues.set(key, value); },
  } satisfies Storage,
});

export function restoreSessionStorage(): void {
  if (originalSessionStorage) Object.defineProperty(globalThis, "sessionStorage", originalSessionStorage);
  else Reflect.deleteProperty(globalThis, "sessionStorage");
}

type PairApproveInput = { ceremonyVersion: number; ephemeralId: string; verificationCode: string };
type PairApprovalStatusInput = { ceremonyVersion: number; ephemeralId: string };

/** Per-test RPC behavior; the coordClient mocks below delegate here. */
export const rpc = {
  approve: async (_request: PairApproveInput): Promise<{ ok: boolean }> => ({ ok: true }),
  deny: async (_request: { ephemeralId: string }): Promise<{ ok: boolean }> => ({ ok: true }),
  status: async (_request: PairApprovalStatusInput): Promise<{ status: string }> =>
    ({ status: "verification_required" }),
};
export const pairApprove = mock((request: PairApproveInput) => rpc.approve(request));
export const pairDeny = mock((request: { ephemeralId: string }) => rpc.deny(request));
export const pairApprovalStatus = mock((request: PairApprovalStatusInput) => rpc.status(request));
export const deletePairRequest = mock((_ephemeralId: string) => undefined);
export const toasts: { message: string; kind: string }[] = [];

export function resetPairApprovalFixture(): void {
  sessionValues.clear();
  toasts.length = 0;
  pairApprove.mockClear();
  pairDeny.mockClear();
  pairApprovalStatus.mockClear();
  deletePairRequest.mockClear();
  rpc.approve = async () => ({ ok: true });
  rpc.deny = async () => ({ ok: true });
  rpc.status = async () => ({ status: "verification_required" });
}

mock.module("@roost/shared/pairing", () => ({
  PAIRING_CEREMONY_VERSION: 1,
  PAIR_VERIFICATION_CODE_LENGTH: 6,
  generatePairRequestId: () => REQUEST_ID,
  generatePairRequesterToken: () => "0".repeat(64),
  generatePairVerificationCode: () => REQUEST_CODE,
  normalizePairRequestId: (value: string) => /^[0-9a-f]{32}$/.test(value) ? value : null,
  normalizePairRequesterToken: (value: string) => /^[0-9a-f]{64}$/.test(value) ? value : null,
  normalizePairVerificationCode: (value: string) => /^\d{6}$/.test(value) ? value : null,
}));
mock.module("@roost/shared/retry", () => ({ backoffDelayMs: () => 10 }));
mock.module("../../src/store/mutations.ts", () => ({ deletePairRequest }));
mock.module("../../src/store/toastStore.ts", () => ({
  addToast: (message: string, kind = "ok") => {
    toasts.push({ message, kind });
    return () => undefined;
  },
}));

export interface CodeDialogProps {
  verificationCode: string;
  requesterLabel: string;
  state: string;
  onCancel: () => void;
}

// The provider creates its dialog inside a memo. The shim invokes this stand-in
// at creation and the memo's disposal clears it, so `mountedDialog` tracks the
// dialog's mount, prop updates, and unmount exactly.
let mountedDialog: CodeDialogProps | null = null;
let registerDialogCleanup: (cleanup: () => void) => void = () => undefined;
function CodeDialogStandIn(props: CodeDialogProps): null {
  mountedDialog = props;
  registerDialogCleanup(() => {
    if (mountedDialog === props) mountedDialog = null;
  });
  return null;
}
mock.module("../../src/components/PairVerificationCodeDialog.tsx", () => ({
  PairVerificationCodeDialog: CodeDialogStandIn,
}));

type VNode = { tag: unknown; props: Record<string, unknown> };

function createElement(
  tag: unknown,
  props: Record<string, unknown> | null,
  ...children: unknown[]
): VNode {
  const merged = { ...(props ?? {}) };
  if (children.length > 0) merged.children = children.length === 1 ? children[0] : children;
  if (tag === CodeDialogStandIn) CodeDialogStandIn(merged as unknown as CodeDialogProps);
  return { tag, props: merged };
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

export interface MountedPairApprovalProvider {
  context: PairApprovalContextValue;
  dispose: () => void;
  /** Props of the mounted code dialog, or null when none is rendered. */
  dialog: () => CodeDialogProps | null;
  setEnabled: (value: boolean) => void;
}

export interface PairApprovalProviderFixture {
  PAIR_APPROVAL_STORAGE_KEY: string;
  announcePairedBrowser: (notice: { ephemeralId: string; label: string }) => void;
  mount: (initiallyEnabled?: boolean) => MountedPairApprovalProvider;
}

/** Loads the provider graph behind the mocks above; call once per suite at
 *  module scope, before any test mounts a provider (module-loading boundary). */
export async function loadPairApprovalProviderFixture(): Promise<PairApprovalProviderFixture> {
  // The real module supplies the device-rejection classifier the lifecycle
  // rules depend on; only the network client is replaced.
  const realConnect = await import("../../src/connect.ts");
  mock.module("../../src/connect.ts", () => ({
    ...realConnect,
    coordClient: { pairApprove, pairDeny, pairApprovalStatus },
  }));
  // Client Solid is the renderer-free runtime this JSX shim drives.
  const Solid = await import(
    new URL("./solid.js", import.meta.resolve("solid-js")).href
  ) as typeof SolidApi;
  mock.module("solid-js", () => Solid);
  registerDialogCleanup = Solid.onCleanup;
  const { PAIR_APPROVAL_STORAGE_KEY } = await import("../../src/auth/pairing-approval.ts");
  const { announcePairedBrowser } = await import("../../src/lib/pairedBrowserNotice.ts");
  const { PairApprovalProvider } = await import("../../src/components/PairApprovalProvider.tsx");

  function mount(initiallyEnabled = true): MountedPairApprovalProvider {
    let context: PairApprovalContextValue | undefined;
    let setEnabled: ((value: boolean) => void) | undefined;
    let dispose: (() => void) | undefined;
    Solid.createRoot((disposeRoot) => {
      dispose = disposeRoot;
      const [enabled, setEnabledSignal] = Solid.createSignal(initiallyEnabled);
      setEnabled = (value) => { setEnabledSignal(value); };
      const tree = PairApprovalProvider({
        get enabled() { return enabled(); },
        children: null,
      }) as unknown as VNode;
      context = tree.props.value as PairApprovalContextValue;
    });
    return {
      context: context!,
      dispose: dispose!,
      dialog: () => mountedDialog,
      setEnabled: setEnabled!,
    };
  }

  return { PAIR_APPROVAL_STORAGE_KEY, announcePairedBrowser, mount };
}

export async function settleApprovalWork(): Promise<void> {
  for (let turn = 0; turn < 12; turn++) await Promise.resolve();
}
